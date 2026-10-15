//! Pure-Rust **Opus** (RFC 6716) decode *and* encode, on [`opus_pure`].
//!
//! Opus needs no `extradata` in either direction: each packet starts with a
//! table-of-contents byte that carries its mode, bandwidth, frame duration and
//! channel count, so the framing is self-describing. An `OpusHead` supplied as
//! `extradata` is therefore ignored 鈥?it describes the container's pre-skip and
//! channel mapping, which only an Ogg/WebM muxer and its `Trim` need.
//!
//! Opus has its own sample-rate ladder (8, 12, 16, 24 and 48 kHz) and its own
//! frame durations (2.5 to 60 ms). This module decodes at whatever rate
//! [`DecoderConfig::sample_rate`] asks for (48 kHz by default) and encodes
//! 20 ms frames at the rate [`EncoderConfig::sample_rate`] asks for, padding the
//! final frame with zeros to reach a whole one.

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::{AudioCodec, Codec};
use crate::media::config::{DecoderConfig, EncoderConfig};
use crate::media::frame::Frame;
use crate::media::packet::{flags, Packet};

use crate::soft::PRIORITY_BUILTIN;
use super::{audio_frame_from_interleaved_f32, interleaved_f32, probe, Direction};

/// The one codec this module serves.
pub(crate) const CODEC: Codec = Codec::Audio(AudioCodec::Opus);

/// The crate that decodes it.
pub(crate) const DECODER_CRATE: &str = "opus-pure";

/// The crate that encodes it.
pub(crate) const ENCODER_CRATE: &str = "opus-pure";

/// The five rates Opus runs at; anything else does not exist in the format.
pub(crate) const SAMPLE_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// The rate used when the caller does not ask for one.
pub(crate) const DEFAULT_SAMPLE_RATE: u32 = 48_000;

/// The frame duration this encoder produces, in milliseconds.
pub(crate) const FRAME_MS: u32 = 20;

/// The default target bitrate when [`EncoderConfig::bitrate`] is zero.
pub(crate) const DEFAULT_BITRATE: u64 = 128_000;

/// Resolves the sample rate a configuration asks for.
fn rate_for(codec: Codec, requested: u32) -> Result<i32> {
    let rate = if requested == 0 { DEFAULT_SAMPLE_RATE } else { requested };
    if !SAMPLE_RATES.contains(&rate) {
        return Err(Error::invalid_argument(format!(
            "{codec} runs at one of the rates {SAMPLE_RATES:?}, got {rate}"
        )));
    }
    Ok(rate as i32)
}

/// Resolves the channel count a configuration asks for.
fn channels_for(codec: Codec, channels: u32) -> Result<usize> {
    if !(1..=2).contains(&channels) {
        return Err(Error::invalid_argument(format!(
            "the {DECODER_CRATE} {codec} codec in this build handles 1 or 2 channels, got \
             {channels}; Opus multistream is not wired up"
        )));
    }
    Ok(channels as usize)
}

/// A pure-Rust Opus decoder.
pub struct OpusDecoder {
    path: CodecPath,
    inner: opus_pure::OpusDecoder,
    sample_rate: i32,
    channels: usize,
    pending: Vec<Frame>,
    first_packet: bool,
    pts_cursor: i64,
}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder")
            .field("path", &self.path.to_string())
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl OpusDecoder {
    /// Builds a decoder from `config`.
    ///
    /// The output rate comes from [`DecoderConfig::sample_rate`] (48 kHz when it
    /// is zero) and the output channels from [`DecoderConfig::channels`]
    /// (stereo when it is unset).
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {DECODER_CRATE} decoder produces opus only, not {}",
                config.codec
            )));
        }
        let sample_rate = rate_for(CODEC, config.sample_rate)?;
        let layout = config.channels.unwrap_or(crate::media::format::ChannelLayout::Stereo);
        let channels = channels_for(CODEC, layout.channels())?;
        let inner = opus_pure::OpusDecoder::new(sample_rate, channels).map_err(|error| {
            Error::invalid_argument(format!(
                "{DECODER_CRATE} rejected a {sample_rate} Hz, {channels}-channel decoder: {error}"
            ))
        })?;
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {DECODER_CRATE} decoder"),
            ),
            inner,
            sample_rate,
            channels,
            pending: Vec::new(),
            first_packet: true,
            pts_cursor: 0,
        })
    }

    /// The sample rate this decoder produces.
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate as u32
    }

    /// The number of channels this decoder produces.
    pub const fn channels(&self) -> u32 {
        self.channels as u32
    }
}

impl Decoder for OpusDecoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_eos() || packet.is_header() || packet.is_empty() {
            // Opus carries its framing per packet; there is no header to apply.
            return Ok(());
        }
        if self.first_packet {
            self.pts_cursor = packet.pts;
            self.first_packet = false;
        }
        let samples = opus_pure::packet::samples(packet.data.as_slice(), self.sample_rate)
            .map_err(|error| {
                Error::backend(format!("{DECODER_CRATE} rejected the packet framing: {error}"))
            })?;
        if samples == 0 {
            return Ok(());
        }
        let mut out = vec![0f32; samples * self.channels];
        let produced = self
            .inner
            .decode(packet.data.as_slice(), samples, &mut out)
            .map_err(|error| Error::backend(format!("{DECODER_CRATE} decode failed: {error}")))?;
        let frames = produced as u32;
        if frames == 0 {
            return Ok(());
        }
        let layout = super::layout_for(CODEC, self.channels as u32)?;
        let frame = audio_frame_from_interleaved_f32(
            self.sample_rate as u32,
            layout,
            frames,
            &out,
            self.pts_cursor,
        )?;
        self.pts_cursor += i64::from(frames);
        self.pending.push(Frame::Audio(frame));
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        Ok(std::mem::take(&mut self.pending))
    }
}

/// A pure-Rust Opus encoder.
pub struct OpusEncoder {
    path: CodecPath,
    inner: opus_pure::OpusEncoder,
    sample_rate: i32,
    channels: usize,
    frame_size: usize,
    /// Interleaved `f32` samples that do not yet fill a whole frame.
    staging: Vec<f32>,
    pending: Vec<Packet>,
    pts_cursor: i64,
    first_frame: bool,
    pushed: bool,
    finished: bool,
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("path", &self.path.to_string())
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("frame_size", &self.frame_size)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl OpusEncoder {
    /// Builds an encoder from `config`.
    ///
    /// The bitrate comes from [`EncoderConfig::bitrate`] and defaults to
    /// [`DEFAULT_BITRATE`] when it is zero. Frames are 20 ms, which is the
    /// duration Opus is normally used at and the one the crate documents.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {ENCODER_CRATE} encoder produces opus only, not {}",
                config.codec
            )));
        }
        let sample_rate = rate_for(CODEC, config.sample_rate)?;
        let channels = channels_for(CODEC, config.channels.channels())?;
        let frame_size = (sample_rate as usize / 1_000) * FRAME_MS as usize;
        if frame_size == 0 {
            return Err(Error::invalid_argument(format!(
                "a {FRAME_MS} ms opus frame at {sample_rate} Hz is not a whole number of samples"
            )));
        }
        let mut inner = opus_pure::OpusEncoder::new(
            sample_rate,
            channels,
            opus_pure::Application::Audio,
        )
        .map_err(|error| {
            Error::invalid_argument(format!(
                "{ENCODER_CRATE} rejected a {sample_rate} Hz, {channels}-channel encoder: {error}"
            ))
        })?;
        let bitrate = if config.bitrate == 0 { DEFAULT_BITRATE } else { config.bitrate };
        inner.bitrate_bps = i32::try_from(bitrate).unwrap_or(i32::MAX);
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {ENCODER_CRATE} encoder"),
            ),
            inner,
            sample_rate,
            channels,
            frame_size,
            staging: Vec::new(),
            pending: Vec::new(),
            pts_cursor: 0,
            first_frame: true,
            pushed: false,
            finished: false,
        })
    }

    /// Samples per channel in each packet this encoder produces.
    pub const fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// `true` once at least one audio frame has been accepted.
    pub const fn has_input(&self) -> bool {
        self.pushed
    }

    /// The encoder's algorithmic delay, in samples at the configured rate.
    pub fn lookahead(&self) -> usize {
        self.inner.lookahead()
    }

    /// Encodes exactly one frame's worth of interleaved samples.
    fn encode_frame(&mut self, frame: &[f32]) -> Result<()> {
        if frame.len() != self.frame_size * self.channels {
            return Err(Error::internal(format!(
                "an opus frame needs {} samples, got {}",
                self.frame_size * self.channels,
                frame.len()
            )));
        }
        let mut out = vec![0u8; opus_pure::MAX_PACKET_BYTES];
        let written = self
            .inner
            .encode(frame, self.frame_size, &mut out)
            .map_err(|error| Error::backend(format!("{ENCODER_CRATE} encode failed: {error}")))?;
        out.truncate(written);
        // The TOC byte says which layers coded the packet: a CELT-only packet
        // needs no earlier packet, a SILK or hybrid one does.
        let keyframe = opus_pure::packet::mode(&out)
            .map(|mode| mode == opus_pure::OpusMode::CeltOnly)
            .unwrap_or(false);
        self.pending.push(
            Packet::new(
                CODEC,
                Buffer::from_vec(out),
                self.pts_cursor,
                if keyframe { flags::KEYFRAME } else { 0 },
            )
            .with_duration(self.frame_size as i64),
        );
        self.pts_cursor += self.frame_size as i64;
        Ok(())
    }

    /// Encodes every whole frame currently staged.
    fn drain(&mut self) -> Result<()> {
        let frame_samples = self.frame_size * self.channels;
        while self.staging.len() >= frame_samples {
            let frame: Vec<f32> = self.staging.drain(..frame_samples).collect();
            self.encode_frame(&frame)?;
        }
        Ok(())
    }
}

impl Encoder for OpusEncoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        if self.finished {
            return Err(Error::state(format!("the {ENCODER_CRATE} encoder was already flushed")));
        }
        let Frame::Audio(audio) = frame else {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder takes audio frames only"
            )));
        };
        if audio.sample_rate != self.sample_rate as u32 {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder was configured for {} Hz but received {} Hz",
                self.sample_rate, audio.sample_rate
            )));
        }
        if audio.channels.channels() as usize != self.channels {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder was configured for {} channels but received {}",
                self.channels,
                audio.channels.name()
            )));
        }
        if self.first_frame {
            self.pts_cursor = audio.pts;
            self.first_frame = false;
        }
        let samples = interleaved_f32(&audio)?;
        self.staging.extend_from_slice(&samples);
        self.pushed = true;
        self.drain()
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        if !self.pushed {
            return Err(Error::state(format!(
                "the {ENCODER_CRATE} encoder was flushed with no input"
            )));
        }
        if !self.finished {
            // Pad the tail to a whole frame: Opus has no way to code a frame
            // shorter than the 2.5 ms rung, and a caller that pushed 100 ms plus
            // one sample should not have that sample dropped silently.
            let frame_samples = self.frame_size * self.channels;
            if !self.staging.is_empty() {
                self.staging.resize(frame_samples, 0.0);
                self.drain()?;
            }
            self.finished = true;
        }
        Ok(std::mem::take(&mut self.pending))
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        let bitrate = if bitrate == 0 { DEFAULT_BITRATE } else { bitrate };
        self.inner.bitrate_bps = i32::try_from(bitrate).unwrap_or(i32::MAX);
        Ok(())
    }
}

/// Registers the pure-Rust Opus encoder and decoder.
pub fn register(registry: &mut Registry) {
    registry.register_decoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &DecoderConfig| Ok(Some(Box::new(OpusDecoder::new(config)?))),
        probe(CODEC, DECODER_CRATE, Direction::Decode),
    );
    registry.register_encoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &EncoderConfig| Ok(Some(Box::new(OpusEncoder::new(config)?))),
        probe(CODEC, ENCODER_CRATE, Direction::Encode),
    );
}

/// Says why this module cannot serve `codec`, or `None` when it can.
pub fn unavailable(codec: Codec, direction: Direction) -> Option<BackendAvailability> {
    if codec == CODEC {
        None
    } else {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} tier serves opus only, not {codec} ({})",
            direction.name()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::format::ChannelLayout;
    use crate::soft::builtin::pure::testing::{best_rms, interleave, sine};

    const RATE: u32 = 48_000;

    fn decoder_config() -> DecoderConfig {
        DecoderConfig::new(CODEC).with_audio(RATE, ChannelLayout::Stereo)
    }

    fn encoder_config() -> EncoderConfig {
        EncoderConfig::audio(AudioCodec::Opus, RATE, ChannelLayout::Stereo).with_bitrate(128_000)
    }

    #[test]
    fn the_rate_ladder_is_the_opus_set() {
        assert_eq!(rate_for(CODEC, 0).unwrap(), 48_000);
        assert_eq!(rate_for(CODEC, 16_000).unwrap(), 16_000);
        let error = rate_for(CODEC, 44_100).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("44100"), "{}", error.message());
    }

    #[test]
    fn only_one_or_two_channels_are_claimed() {
        assert!(channels_for(CODEC, 1).is_ok());
        assert!(channels_for(CODEC, 2).is_ok());
        let error = channels_for(CODEC, 6).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("multistream"), "{}", error.message());
    }

    #[test]
    fn the_decoder_rejects_any_other_codec() {
        for codec in Codec::ALL {
            let config = DecoderConfig::new(codec).with_audio(RATE, ChannelLayout::Stereo);
            let result = OpusDecoder::new(&config);
            if codec == CODEC {
                assert!(result.is_ok(), "{codec} must be accepted");
            } else {
                let error = result.err().expect("only opus may be accepted");
                assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
                assert!(error.message().contains("opus"), "{}", error.message());
            }
        }
    }

    #[test]
    fn a_header_packet_is_ignored_and_an_empty_packet_is_not_an_error() {
        let mut decoder = OpusDecoder::new(&decoder_config()).unwrap();
        decoder.push(Packet::header(CODEC, Buffer::from_slice(b"OpusHead"))).unwrap();
        decoder.push(Packet::new(CODEC, Buffer::new(), 0, 0)).unwrap();
        assert!(decoder.receive().unwrap().is_none());
    }

    #[test]
    fn a_malformed_packet_is_an_error_not_a_panic() {
        let mut decoder = OpusDecoder::new(&decoder_config()).unwrap();
        // A TOC byte claiming code 3 with a truncated frame table.
        let error = decoder
            .push(Packet::new(CODEC, Buffer::from_slice(&[0xFF, 0x00]), 0, 0))
            .unwrap_err();
        assert!(
            matches!(
                error.kind(),
                crate::error::ErrorKind::Backend | crate::error::ErrorKind::InvalidArgument
            ),
            "unexpected kind {:?}",
            error.kind()
        );
    }

    #[test]
    fn flushing_without_input_is_a_state_error() {
        let mut encoder = OpusEncoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.flush().unwrap_err().kind(), crate::error::ErrorKind::State);
    }

    #[test]
    fn the_encoder_frames_are_twenty_milliseconds() {
        let encoder = OpusEncoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.frame_size(), 960);
        let encoder = OpusEncoder::new(
            &EncoderConfig::audio(AudioCodec::Opus, 16_000, ChannelLayout::Mono),
        )
        .unwrap();
        assert_eq!(encoder.frame_size(), 320);
    }

    #[test]
    fn the_paths_name_the_crate_and_the_software_tier() {
        let decoder = OpusDecoder::new(&decoder_config()).unwrap();
        assert_eq!(decoder.sample_rate(), RATE);
        assert_eq!(decoder.channels(), 2);
        assert_eq!(decoder.path().backend, BackendKind::Rust);
        assert!(decoder.path().is_software());
        assert!(!decoder.path().hardware_accelerated);
        assert!(decoder.path().detail.contains(DECODER_CRATE));

        let encoder = OpusEncoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.path().backend, BackendKind::Rust);
        assert!(encoder.path().is_software());
        assert!(!encoder.path().hardware_accelerated);
        assert!(encoder.path().detail.contains(ENCODER_CRATE));
    }

    /// The end-to-end proof: encode with the pure-Rust Opus encoder, decode with
    /// the pure-Rust Opus decoder, and compare the waveforms. Opus is lossy and
    /// has a look-ahead, so the comparison allows for the codec delay.
    #[test]
    fn software_opus_round_trips_through_the_software_decoder() {
        let frames = 9_600usize;
        let left = sine(frames, 200.0, RATE as f32);
        let right = sine(frames, 300.0, RATE as f32);
        let planar = vec![left.clone(), right.clone()];

        let mut encoder = OpusEncoder::new(&encoder_config()).unwrap();
        let frame = crate::media::frame::AudioFrame::from_f32_planar(
            RATE,
            ChannelLayout::Stereo,
            frames as u32,
            planar,
            0,
        )
        .unwrap();
        encoder.push(Frame::Audio(frame)).unwrap();
        let packets = encoder.flush().unwrap();
        assert!(packets.len() >= frames / 960, "at least one packet per 20 ms");

        let mut decoder = OpusDecoder::new(&decoder_config()).unwrap();
        for packet in &packets {
            decoder.push(packet.clone()).unwrap();
        }
        let mut decoded: Vec<f32> = Vec::new();
        let mut channels = 0usize;
        let mut drained: Vec<Frame> = Vec::new();
        while let Some(frame) = decoder.receive().unwrap() {
            drained.push(frame);
        }
        drained.extend(decoder.flush().unwrap());
        for frame in drained {
            let audio = frame.as_audio().expect("audio frames only").clone();
            channels = audio.channels.channels() as usize;
            decoded.extend(interleaved_f32(&audio).unwrap());
        }
        assert!(channels >= 1, "the decoder produced no audio");
        assert!(
            decoded.len() / channels >= frames,
            "decoded {} sample frames, expected at least {frames}",
            decoded.len() / channels
        );

        let expected = interleave(&left, &right, channels);
        let rms = best_rms(&expected, &decoded, 4_800);
        assert!(rms < 0.15, "worst-case RMS error {rms} is too large for 128 kbps Opus");
    }

    #[test]
    fn unavailable_names_the_codec_it_cannot_serve() {
        assert!(unavailable(CODEC, Direction::Decode).is_none());
        let note = unavailable(Codec::Audio(AudioCodec::Vorbis), Direction::Decode)
            .expect("vorbis is not this module's codec");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("vorbis"));
    }
}
