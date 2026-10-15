//! Pure-Rust **FLAC** (Xiph lossless) decode *and* encode, on [`rusty_flac`].
//!
//! ## Decode
//!
//! FLAC is a file-oriented format: the `STREAMINFO` metadata block carries the
//! sample rate, channel count and bit depth that every frame depends on, and
//! `rusty_flac`'s decoder wants a complete stream in memory. So this decoder:
//!
//! 1. builds the stream header from [`DecoderConfig::extradata`], which must be
//!    the **34-byte `STREAMINFO` body** 鈥?the same value
//!    [`super::super::flac_enc::FlacEncoder`] exposes; and
//! 2. appends each pushed packet's bytes to a growing buffer and decodes every
//!    frame that is complete so far.
//!
//! A packet that only completes a frame part way is buffered, not lost: a
//! truncated tail is indistinguishable from "more data is coming", so the
//! decoder waits. The cost is that this decoder re-decodes the buffered frames
//! whenever it has been drained and new bytes have arrived, which is linear in
//! the *buffer* rather than in the packet 鈥?acceptable for the container-shaped
//! input this tier serves, and the honest reason a caller streaming one frame
//! per packet should prefer [`super::super::symphonia_dec`].
//!
//! ## Encode
//!
//! Lossless, like the `flacenc` encoder it sits in front of: the encoder buffers
//! and then produces exactly two packets on [`Encoder::flush`] 鈥?a `HEADER`
//! packet carrying the 34-byte `STREAMINFO` body, and one packet carrying every
//! encoded frame. [`Encoder::extradata`] returns the same `STREAMINFO`, so a
//! caller that forgets to keep packet 1 can still store the configuration.

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::{Buffer, BufferBuilder};
use crate::media::codec::{AudioCodec, Codec};
use crate::media::config::{DecoderConfig, EncoderConfig};
use crate::media::format::ChannelLayout;
use crate::media::frame::Frame;
use crate::media::packet::{flags, Packet};

use super::super::flac_enc::split_flac_stream;
use crate::soft::PRIORITY_BUILTIN;
use super::{
    audio_frame_from_interleaved_f32, interleaved_f32, layout_for, probe, Direction,
};

/// The one codec this module serves.
pub(crate) const CODEC: Codec = Codec::Audio(AudioCodec::Flac);

/// The crate that decodes it.
pub(crate) const DECODER_CRATE: &str = "rusty_flac";

/// The crate that encodes it.
pub(crate) const ENCODER_CRATE: &str = "rusty_flac";

/// Length of a `STREAMINFO` metadata block body.
pub(crate) const STREAMINFO_LEN: usize = 34;

/// The default bit depth when `EncoderConfig::option("bits_per_sample")` is unset.
pub(crate) const DEFAULT_BITS_PER_SAMPLE: u8 = 16;

/// The bit depths `rusty_flac` can encode.
pub(crate) const ENCODABLE_BITS: [u8; 3] = [8, 16, 24];

/// The geometry a FLAC `STREAMINFO` block declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// Samples per second.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u32,
    /// Bits per sample.
    pub bits_per_sample: u32,
    /// Total sample frames, or `0` when the stream did not know.
    pub total_samples: u64,
}

/// Parses the fields of a 34-byte `STREAMINFO` block body.
///
/// The layout is fixed by the FLAC format: two 16-bit block sizes, two 24-bit
/// frame sizes, then a 64-bit word holding a 20-bit sample rate, a 3-bit
/// (channels - 1), a 5-bit (bits per sample - 1) and a 36-bit sample count.
pub fn parse_streaminfo(body: &[u8]) -> Result<StreamInfo> {
    if body.len() != STREAMINFO_LEN {
        return Err(Error::invalid_argument(format!(
            "a FLAC STREAMINFO block body is {STREAMINFO_LEN} bytes, got {}",
            body.len()
        )));
    }
    let sample_rate =
        (u32::from(body[10]) << 12) | (u32::from(body[11]) << 4) | (u32::from(body[12]) >> 4);
    let channels = ((u32::from(body[12]) >> 1) & 0x07) + 1;
    let bits_per_sample = (((u32::from(body[12]) & 0x01) << 4) | (u32::from(body[13]) >> 4)) + 1;
    let total_samples = ((u64::from(body[13]) & 0x0F) << 32)
        | (u64::from(body[14]) << 24)
        | (u64::from(body[15]) << 16)
        | (u64::from(body[16]) << 8)
        | u64::from(body[17]);
    Ok(StreamInfo { sample_rate, channels, bits_per_sample, total_samples })
}

/// The `fLaC` marker plus the 4-byte metadata block header the decoder needs in
/// front of the `STREAMINFO` body.
const STREAM_HEADER_LEN: usize = 4 + 4 + STREAMINFO_LEN;

/// Rebuilds the stream prefix a decoder needs from a `STREAMINFO` body.
fn stream_prefix(streaminfo: &[u8]) -> Result<Vec<u8>> {
    if streaminfo.len() != STREAMINFO_LEN {
        return Err(Error::invalid_argument(format!(
            "the {DECODER_CRATE} decoder needs a {STREAMINFO_LEN}-byte STREAMINFO metadata block \
             body in `DecoderConfig::extradata`, got {} bytes",
            streaminfo.len()
        )));
    }
    let mut stream = Vec::with_capacity(STREAM_HEADER_LEN);
    stream.extend_from_slice(b"fLaC");
    // Last metadata block, type 0 (STREAMINFO), 34-byte body.
    stream.push(0x80);
    stream.extend_from_slice(&[0x00, 0x00, STREAMINFO_LEN as u8]);
    stream.extend_from_slice(streaminfo);
    Ok(stream)
}

/// A pure-Rust FLAC decoder.
pub struct FlacDecoder {
    path: CodecPath,
    codec: Codec,
    /// `fLaC` + `STREAMINFO` + every frame byte pushed so far.
    stream: Vec<u8>,
    /// Samples per channel already handed to the caller.
    emitted: usize,
    pending: Vec<Frame>,
    /// Length of `stream` at the last decode attempt, so an unchanged buffer is
    /// not decoded twice.
    last_attempt: usize,
    attempted: bool,
    first_packet: bool,
    pts_cursor: i64,
    layout: ChannelLayout,
    sample_rate: u32,
    bits_per_sample: u32,
}

impl std::fmt::Debug for FlacDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlacDecoder")
            .field("path", &self.path.to_string())
            .field("buffered", &self.stream.len())
            .field("emitted", &self.emitted)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl FlacDecoder {
    /// Builds a decoder from `config`.
    ///
    /// `config.extradata` must be the 34-byte `STREAMINFO` body; a stream
    /// without it cannot be decoded, and the error says so rather than producing
    /// silence.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {DECODER_CRATE} decoder produces flac only, not {}",
                config.codec
            )));
        }
        let stream = stream_prefix(config.extradata.as_slice())?;
        let info = parse_streaminfo(config.extradata.as_slice())?;
        if info.sample_rate == 0 {
            return Err(Error::invalid_argument(
                "the STREAMINFO block declares a zero sample rate",
            ));
        }
        let layout = layout_for(CODEC, info.channels)?;
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {DECODER_CRATE} decoder"),
            ),
            codec: CODEC,
            stream,
            emitted: 0,
            pending: Vec::new(),
            last_attempt: STREAM_HEADER_LEN,
            attempted: false,
            first_packet: true,
            pts_cursor: 0,
            layout,
            sample_rate: info.sample_rate,
            bits_per_sample: info.bits_per_sample,
        })
    }

    /// The geometry the `STREAMINFO` block declared.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The sample format this decoder produces, always interleaved `f32`.
    pub fn bits_per_sample(&self) -> u32 {
        self.bits_per_sample
    }

    /// Decodes every frame the buffer now holds that it did not hold before.
    ///
    /// A truncated tail is not an error: it means more input is needed.
    fn pump(&mut self) -> Result<()> {
        if self.stream.len() <= STREAM_HEADER_LEN {
            return Ok(());
        }
        if self.attempted && self.stream.len() == self.last_attempt {
            return Ok(());
        }
        self.attempted = true;
        self.last_attempt = self.stream.len();

        let mut decoder = rusty_flac::Decoder::new(&self.stream).map_err(|error| {
            Error::invalid_argument(format!("{DECODER_CRATE} rejected the stream header: {error}"))
        })?;
        let info = decoder.streaminfo().clone();
        let mut planes: Vec<Vec<i32>> = (0..info.channels as usize).map(|_| Vec::new()).collect();
        let mut blocks: Vec<usize> = Vec::new();
        loop {
            match decoder.next_frame(&mut planes) {
                Ok(Some(block)) => blocks.push(block),
                Ok(None) => break,
                // A partial trailing frame simply means "more bytes are coming".
                Err(rusty_flac::DecodeError::Truncated) => break,
                Err(error) => {
                    return Err(Error::backend(format!("{DECODER_CRATE} decode failed: {error}")))
                }
            }
        }

        let scale = (1i64 << (self.bits_per_sample.saturating_sub(1))) as f32;
        let channels = self.layout.channels() as usize;
        let mut offset = 0usize;
        for block in blocks {
            if offset >= self.emitted {
                let mut interleaved = Vec::with_capacity(block * channels);
                for index in 0..block {
                    for plane in planes.iter().take(channels) {
                        let sample = plane.get(offset + index).copied().unwrap_or(0);
                        interleaved.push(sample as f32 / scale);
                    }
                }
                let frame = audio_frame_from_interleaved_f32(
                    self.sample_rate,
                    self.layout,
                    block as u32,
                    &interleaved,
                    self.pts_cursor,
                )?;
                self.pts_cursor += block as i64;
                self.pending.push(Frame::Audio(frame));
            }
            offset += block;
        }
        self.emitted = offset;
        Ok(())
    }
}

impl Decoder for FlacDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_eos() || packet.is_header() {
            // FLAC's configuration is `extradata`; a header packet carries the
            // same STREAMINFO and is redundant here.
            return Ok(());
        }
        if self.first_packet {
            self.pts_cursor = packet.pts;
            self.first_packet = false;
        }
        self.stream.extend_from_slice(packet.data.as_slice());
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        if self.pending.is_empty() {
            self.pump()?;
        }
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        self.pump()?;
        Ok(std::mem::take(&mut self.pending))
    }
}

/// A pure-Rust FLAC encoder.
pub struct FlacEncoder {
    path: CodecPath,
    inner: Option<rusty_flac::Encoder>,
    sample_rate: u32,
    channels: u32,
    bits_per_sample: u8,
    frames: u64,
    first_pts: i64,
    streaminfo: Option<Buffer>,
    pending: Vec<Packet>,
}

impl std::fmt::Debug for FlacEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlacEncoder")
            .field("path", &self.path.to_string())
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("bits_per_sample", &self.bits_per_sample)
            .field("frames", &self.frames)
            .finish()
    }
}

impl FlacEncoder {
    /// Builds an encoder from `config`.
    ///
    /// Recognises the option `bits_per_sample`, which must be 8, 16 or 24.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {ENCODER_CRATE} encoder produces flac only, not {}",
                config.codec
            )));
        }
        let bits_per_sample = match config.option("bits_per_sample") {
            Some(text) => text.parse::<u8>().map_err(|_| {
                Error::invalid_argument(format!("`bits_per_sample` is not a number: {text:?}"))
            })?,
            None => DEFAULT_BITS_PER_SAMPLE,
        };
        if !ENCODABLE_BITS.contains(&bits_per_sample) {
            return Err(Error::invalid_argument(format!(
                "`bits_per_sample` must be one of {ENCODABLE_BITS:?}, got {bits_per_sample}"
            )));
        }
        let channels = config.channels.channels();
        let inner = rusty_flac::Encoder::new(config.sample_rate, channels, u32::from(bits_per_sample))
            .map_err(|error| {
                Error::invalid_argument(format!("{ENCODER_CRATE} rejected the geometry: {error}"))
            })?;
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {ENCODER_CRATE} {bits_per_sample}-bit encoder"),
            ),
            inner: Some(inner),
            sample_rate: config.sample_rate,
            channels,
            bits_per_sample,
            frames: 0,
            first_pts: 0,
            streaminfo: None,
            pending: Vec::new(),
        })
    }

    /// Number of sample frames buffered so far.
    pub const fn buffered_frames(&self) -> u64 {
        self.frames
    }

    /// The `STREAMINFO` block body, once [`Encoder::flush`] has run.
    pub fn streaminfo(&self) -> Option<&Buffer> {
        self.streaminfo.as_ref()
    }

    /// Serialises the buffered samples and splits the stream into its
    /// `STREAMINFO` body and its frames.
    fn encode_buffered(&mut self) -> Result<()> {
        if self.frames == 0 {
            return Err(Error::state(format!(
                "the {ENCODER_CRATE} encoder was flushed with no input"
            )));
        }
        let encoder = self
            .inner
            .take()
            .ok_or_else(|| Error::state("the FLAC encoder was already flushed".to_string()))?;
        let stream = encoder.finish();
        let (streaminfo, frames) = split_flac_stream(&stream)?;
        let streaminfo = Buffer::from_vec(streaminfo);
        self.pending.push(Packet::new(CODEC, streaminfo.clone(), 0, flags::HEADER));
        self.pending.push(
            Packet::new(CODEC, Buffer::from_vec(frames), self.first_pts, flags::KEYFRAME)
                .with_duration(self.frames as i64),
        );
        self.streaminfo = Some(streaminfo);
        Ok(())
    }
}

impl Encoder for FlacEncoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        let Frame::Audio(audio) = frame else {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder takes audio frames only"
            )));
        };
        if audio.sample_rate != self.sample_rate {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder was configured for {} Hz but received {} Hz",
                self.sample_rate, audio.sample_rate
            )));
        }
        if audio.channels.channels() != self.channels {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder was configured for {} channels but received {}",
                self.channels,
                audio.channels.name()
            )));
        }
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| Error::state("the FLAC encoder was already flushed".to_string()))?;
        if self.frames == 0 {
            self.first_pts = audio.pts;
        }
        let samples = interleaved_f32(&audio)?;
        let mut builder = BufferBuilder::with_capacity(samples.len() * 4);
        for sample in &samples {
            builder.push(&sample.to_le_bytes());
        }
        inner
            .push_f32le_bytes(builder.finish().as_slice())
            .map_err(|error| {
                Error::invalid_argument(format!("{ENCODER_CRATE} rejected the input: {error}"))
            })?;
        self.frames += u64::from(audio.frames);
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        if self.pending.is_empty() && self.streaminfo.is_none() {
            self.encode_buffered()?;
        }
        Ok(std::mem::take(&mut self.pending))
    }

    fn extradata(&self) -> Option<Buffer> {
        self.streaminfo.clone()
    }
}

/// Registers the pure-Rust FLAC encoder and decoder.
///
/// Both are registered before [`super::super::flac_enc`], so the registry tries
/// `rusty_flac` first and keeps `flacenc` as the fallback encoder it has always
/// been documented as.
pub fn register(registry: &mut Registry) {
    registry.register_decoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &DecoderConfig| Ok(Some(Box::new(FlacDecoder::new(config)?))),
        probe(CODEC, DECODER_CRATE, Direction::Decode),
    );
    registry.register_encoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &EncoderConfig| Ok(Some(Box::new(FlacEncoder::new(config)?))),
        probe(CODEC, ENCODER_CRATE, Direction::Encode),
    );
}

/// Says why this module cannot serve `codec`, or `None` when it can.
pub fn unavailable(codec: Codec, direction: Direction) -> Option<BackendAvailability> {
    if codec == CODEC {
        None
    } else {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} tier serves flac only, not {codec} ({})",
            direction.name()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::frame::AudioFrame;
    use crate::soft::builtin::pure::testing::sine;

    const RATE: u32 = 44_100;

    fn encoder_config() -> EncoderConfig {
        EncoderConfig::audio(AudioCodec::Flac, RATE, ChannelLayout::Stereo)
    }

    fn decoded_samples(frames: Vec<Frame>) -> (Vec<f32>, usize) {
        let mut samples = Vec::new();
        let mut channels = 0usize;
        for frame in frames {
            let audio = frame.as_audio().expect("audio frames only").clone();
            channels = audio.channels.channels() as usize;
            samples.extend(interleaved_f32(&audio).unwrap());
        }
        (samples, channels)
    }

    #[test]
    fn the_streaminfo_layout_is_parsed_field_by_field() {
        // sample_rate 44100 (0x0AC44), 2 channels, 16 bits, 4096 samples.
        let mut body = [0u8; STREAMINFO_LEN];
        let rate = 44_100u32;
        body[10] = (rate >> 12) as u8;
        body[11] = (rate >> 4) as u8;
        body[12] = (((rate & 0x0F) as u8) << 4) | (((2u32 - 1) as u8) << 1);
        body[13] = ((16u32 - 1) as u8) << 4;
        body[16] = 0x10;
        body[17] = 0x00;
        let info = parse_streaminfo(&body).unwrap();
        assert_eq!(info.sample_rate, RATE);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits_per_sample, 16);
        assert_eq!(info.total_samples, 4_096);
        assert_eq!(parse_streaminfo(&body[..33]).unwrap_err().kind(), crate::error::ErrorKind::InvalidArgument);
    }

    #[test]
    fn the_decoder_needs_a_streaminfo_block_in_extradata() {
        let config = DecoderConfig::new(CODEC).with_audio(RATE, ChannelLayout::Stereo);
        let error = FlacDecoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("STREAMINFO"), "{}", error.message());

        let config = config.with_extradata(Buffer::from_slice(&[0; 4]));
        let error = FlacDecoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("34"), "{}", error.message());
    }

    #[test]
    fn the_decoder_rejects_any_other_codec() {
        let config = DecoderConfig::new(Codec::Audio(AudioCodec::Opus));
        let error = FlacDecoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
        assert!(error.message().contains("flac"), "{}", error.message());
    }

    #[test]
    fn the_encoder_validates_its_bit_depth_and_geometry() {
        let mut config = encoder_config();
        config.options.push(("bits_per_sample".into(), "20".into()));
        let error = FlacEncoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("20"), "{}", error.message());

        let mut config = encoder_config();
        config.sample_rate = 0;
        assert_eq!(
            FlacEncoder::new(&config).unwrap_err().kind(),
            crate::error::ErrorKind::InvalidArgument
        );
    }

    #[test]
    fn mismatched_geometry_is_rejected() {
        let mut encoder = FlacEncoder::new(&encoder_config()).unwrap();
        let frame =
            AudioFrame::from_f32_planar(48_000, ChannelLayout::Stereo, 4, vec![vec![0.0; 4]; 2], 0)
                .unwrap();
        let error = encoder.push(Frame::Audio(frame)).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("Hz"), "{}", error.message());
    }

    #[test]
    fn flushing_without_input_is_a_state_error() {
        let mut encoder = FlacEncoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.flush().unwrap_err().kind(), crate::error::ErrorKind::State);
    }

    #[test]
    fn the_paths_name_the_crate_and_the_software_tier() {
        let encoder = FlacEncoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.path().backend, BackendKind::Rust);
        assert!(encoder.path().is_software());
        assert!(!encoder.path().hardware_accelerated);
        assert!(encoder.path().detail.contains(ENCODER_CRATE), "{}", encoder.path().detail);
    }

    /// The end-to-end proof: encode with the pure-Rust encoder, decode with the
    /// pure-Rust decoder, and compare sample by sample. FLAC is lossless, so the
    /// only error allowed is the quantisation of the input onto the encoder's
    /// bit depth.
    #[test]
    fn software_flac_round_trips_through_the_software_decoder() {
        let frames = 4_096usize;
        let left = sine(frames, 441.0, RATE as f32);
        let right = sine(frames, 330.0, RATE as f32);
        let planar = vec![left.clone(), right.clone()];

        let mut encoder = FlacEncoder::new(&encoder_config()).unwrap();
        let frame = AudioFrame::from_f32_planar(
            RATE,
            ChannelLayout::Stereo,
            frames as u32,
            planar.clone(),
            0,
        )
        .unwrap();
        encoder.push(Frame::Audio(frame)).unwrap();
        assert_eq!(encoder.buffered_frames(), frames as u64);

        let packets = encoder.flush().unwrap();
        assert_eq!(packets.len(), 2, "a header packet and a frame packet");
        assert!(packets[0].is_header());
        assert_eq!(packets[0].len(), STREAMINFO_LEN);
        assert!(!packets[1].is_empty());
        let extradata = encoder.extradata().expect("a FLAC encoder has extradata");
        assert_eq!(extradata.as_slice(), packets[0].data.as_slice());

        let mut decoder = FlacDecoder::new(
            &DecoderConfig::new(CODEC)
                .with_extradata(extradata)
                .with_audio(RATE, ChannelLayout::Stereo),
        )
        .unwrap();
        assert_eq!(decoder.sample_rate(), RATE);
        assert_eq!(decoder.bits_per_sample(), 16);
        decoder.push(packets[1].clone()).unwrap();
        let mut drained: Vec<Frame> = Vec::new();
        while let Some(frame) = decoder.receive().unwrap() {
            drained.push(frame);
        }
        drained.extend(decoder.flush().unwrap());
        let (samples, channels) = decoded_samples(drained);
        assert_eq!(channels, 2);
        assert_eq!(samples.len(), frames * 2, "every sample must come back");

        let tolerance = 2.0 / 32_768.0;
        let mut worst = 0.0f32;
        for index in 0..frames {
            for channel in 0..2 {
                let expected = planar[channel][index];
                let actual = samples[index * 2 + channel];
                worst = worst.max((expected - actual).abs());
            }
        }
        assert!(worst < tolerance, "worst sample error {worst} exceeds {tolerance}");
    }

    #[test]
    fn a_stream_split_across_packets_still_decodes() {
        let frames = 4_096usize;
        let mono = sine(frames, 220.0, RATE as f32);
        let mut encoder = FlacEncoder::new(&EncoderConfig::audio(
            AudioCodec::Flac,
            RATE,
            ChannelLayout::Mono,
        ))
        .unwrap();
        let frame =
            AudioFrame::from_f32_planar(RATE, ChannelLayout::Mono, frames as u32, vec![mono], 0)
                .unwrap();
        encoder.push(Frame::Audio(frame)).unwrap();
        let packets = encoder.flush().unwrap();
        let extradata = encoder.extradata().unwrap();

        let mut decoder = FlacDecoder::new(
            &DecoderConfig::new(CODEC)
                .with_extradata(extradata)
                .with_audio(RATE, ChannelLayout::Mono),
        )
        .unwrap();
        // Feed the frame bytes in hostile chunks, exactly as a demuxer might.
        let bytes = packets[1].data.as_slice();
        for chunk in bytes.chunks(97) {
            decoder.push(Packet::new(CODEC, Buffer::from_slice(chunk), 0, 0)).unwrap();
        }
        let mut drained: Vec<Frame> = Vec::new();
        while let Some(frame) = decoder.receive().unwrap() {
            drained.push(frame);
        }
        drained.extend(decoder.flush().unwrap());
        let (samples, channels) = decoded_samples(drained);
        assert_eq!(channels, 1);
        assert_eq!(samples.len(), frames);
    }

    #[test]
    fn unavailable_names_the_codec_it_cannot_serve() {
        assert!(unavailable(CODEC, Direction::Decode).is_none());
        let note = unavailable(Codec::Audio(AudioCodec::Mp3), Direction::Decode)
            .expect("mp3 is not this module's codec");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("mp3"));
    }
}
