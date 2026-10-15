//! Pure-Rust **MP3** (MPEG-1/2/2.5 Layer III) decode *and* encode, on
//! [`rusty_mp3`].
//!
//! MP3 is the one codec in this tier that needs no `extradata` in either
//! direction: the frame headers carry the sample rate, channel mode and bitrate
//! per frame, and the encoder derives its stream header from the geometry the
//! caller configured. A `HEADER` packet is therefore ignored on the way in, and
//! the encoder's own `Xing`/`Info` frame is flagged `HEADER` on the way out.
//!
//! Output is interleaved [`SampleFormat::F32`], which is what `rusty_mp3`
//! produces, so no integer conversion decision is made here.

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
use super::{audio_frame_from_interleaved_f32, interleaved_f32, layout_for, probe, Direction};

/// The one codec this module serves.
pub(crate) const CODEC: Codec = Codec::Audio(AudioCodec::Mp3);

/// The crate that decodes it.
pub(crate) const DECODER_CRATE: &str = "rusty_mp3";

/// The crate that encodes it.
pub(crate) const ENCODER_CRATE: &str = "rusty_mp3";

/// Every sample rate MPEG Layer III can carry, and the only rates the encoder
/// and decoder in this module accept.
pub(crate) const SAMPLE_RATES: [u32; 9] =
    [8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000];

/// Sample frames in one Layer III frame for `sample_rate`.
///
/// MPEG-1 carries two granules of 576; MPEG-2 and MPEG-2.5 carry one.
pub const fn samples_per_frame(sample_rate: u32) -> u32 {
    match sample_rate {
        32_000 | 44_100 | 48_000 => 1_152,
        _ => 576,
    }
}

/// `true` when `sample_rate` is one of the nine MPEG Layer III rates.
pub const fn is_mpeg_rate(sample_rate: u32) -> bool {
    let mut index = 0;
    while index < SAMPLE_RATES.len() {
        if SAMPLE_RATES[index] == sample_rate {
            return true;
        }
        index += 1;
    }
    false
}

/// A pure-Rust MP3 decoder.
pub struct Mp3Decoder {
    path: CodecPath,
    inner: rusty_mp3::Mp3Decoder,
    pending: Vec<Frame>,
    /// The first packet's `pts` seeds the output timestamp cursor.
    first_packet: bool,
    pts_cursor: i64,
}

impl std::fmt::Debug for Mp3Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp3Decoder")
            .field("path", &self.path.to_string())
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl Mp3Decoder {
    /// Builds a decoder from `config`.
    ///
    /// Fails with [`crate::error::ErrorKind::Unsupported`] for any codec other
    /// than MP3. No `extradata` is required, and any supplied is ignored: an MP3
    /// elementary stream is self-describing.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {DECODER_CRATE} decoder produces mp3 only, not {}",
                config.codec
            )));
        }
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {DECODER_CRATE} decoder"),
            ),
            inner: rusty_mp3::Mp3Decoder::new(),
            pending: Vec::new(),
            first_packet: true,
            pts_cursor: 0,
        })
    }

    /// Decodes every whole frame the last push completed.
    fn drain(&mut self) -> Result<()> {
        loop {
            match self.inner.next_frame() {
                Ok(audio) => {
                    let channels = audio.channels.max(1) as u32;
                    if audio.samples.len() % channels as usize != 0 {
                        return Err(Error::backend(format!(
                            "{DECODER_CRATE} returned {} samples that are not a whole number of \
                             {channels}-channel frames",
                            audio.samples.len()
                        )));
                    }
                    let frames = (audio.samples.len() / channels as usize) as u32;
                    let layout = layout_for(CODEC, channels)?;
                    let frame = audio_frame_from_interleaved_f32(
                        audio.sample_rate,
                        layout,
                        frames,
                        &audio.samples,
                        self.pts_cursor,
                    )?;
                    self.pts_cursor += i64::from(frames);
                    self.pending.push(Frame::Audio(frame));
                }
                Err(rusty_mp3::Error::Again) | Err(rusty_mp3::Error::Eof) => return Ok(()),
                Err(error) => {
                    return Err(Error::backend(format!("{DECODER_CRATE} decode failed: {error}")))
                }
            }
        }
    }
}

impl Decoder for Mp3Decoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_eos() {
            self.inner.flush();
            return self.drain();
        }
        if packet.is_header() {
            // MP3 carries its configuration in every frame header.
            return Ok(());
        }
        if self.first_packet {
            self.pts_cursor = packet.pts;
            self.first_packet = false;
        }
        self.inner.push(packet.data.as_slice());
        self.drain()
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        self.inner.flush();
        self.drain()?;
        Ok(std::mem::take(&mut self.pending))
    }
}

/// A pure-Rust MP3 encoder.
pub struct Mp3Encoder {
    path: CodecPath,
    inner: rusty_mp3::Mp3Encoder,
    sample_rate: u32,
    channels: u16,
    /// CBR target in kbps; `0` lets the crate choose.
    cbr_kbps: u32,
    /// VBR target average bitrate in kbps, when the caller asked for VBR.
    vbr_quality: Option<f32>,
    samples_per_frame: u32,
    pending: Vec<Packet>,
    pts_cursor: i64,
    first_frame: bool,
    /// The stream's `Xing`/`Info` frame is prepended to the queue at
    /// [`Encoder::flush`], so the first packet drained there carries it.
    header_pending: bool,
    pushed: bool,
    finished: bool,
}

impl std::fmt::Debug for Mp3Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp3Encoder")
            .field("path", &self.path.to_string())
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl Mp3Encoder {
    /// Builds an encoder from `config`.
    ///
    /// Recognises two options: `vbr_quality`, a VBR target average bitrate in
    /// kbps (see `rusty_mp3::vbr_quality_index` for a LAME-style mapping), and
    /// nothing else. The bitrate comes from [`EncoderConfig::bitrate`].
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {ENCODER_CRATE} encoder produces mp3 only, not {}",
                config.codec
            )));
        }
        let channels = config.channels.channels();
        if channels > 2 {
            return Err(Error::invalid_argument(format!(
                "{ENCODER_CRATE} encodes 1 or 2 channels, got {channels}"
            )));
        }
        if !is_mpeg_rate(config.sample_rate) {
            return Err(Error::invalid_argument(format!(
                "{ENCODER_CRATE} needs one of the nine MPEG Layer III sample rates \
                 ({SAMPLE_RATES:?}), got {}",
                config.sample_rate
            )));
        }
        let vbr_quality = match config.option("vbr_quality") {
            Some(text) => Some(text.parse::<f32>().map_err(|_| {
                Error::invalid_argument(format!("`vbr_quality` is not a number: {text:?}"))
            })?),
            None => None,
        };
        let cbr_kbps = u32::try_from(config.bitrate / 1_000).unwrap_or(u32::MAX);
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {ENCODER_CRATE} encoder"),
            ),
            inner: Self::make_inner(cbr_kbps, vbr_quality),
            sample_rate: config.sample_rate,
            channels: channels as u16,
            cbr_kbps,
            vbr_quality,
            samples_per_frame: samples_per_frame(config.sample_rate),
            pending: Vec::new(),
            pts_cursor: 0,
            first_frame: true,
            header_pending: false,
            pushed: false,
            finished: false,
        })
    }

    fn make_inner(cbr_kbps: u32, vbr_quality: Option<f32>) -> rusty_mp3::Mp3Encoder {
        rusty_mp3::Mp3Encoder::new(rusty_mp3::Mp3EncoderConfig {
            bitrate_kbps: cbr_kbps,
            vbr_quality,
        })
    }

    /// `true` once at least one audio frame has been accepted.
    pub const fn has_input(&self) -> bool {
        self.pushed
    }

    /// Pulls every packet the last push (or the finish) produced.
    fn drain(&mut self) -> Result<()> {
        loop {
            match self.inner.next_packet() {
                Ok(bytes) => {
                    let header = self.header_pending;
                    self.header_pending = false;
                    self.pending.push(
                        Packet::new(
                            CODEC,
                            Buffer::from_vec(bytes),
                            self.pts_cursor,
                            if header { flags::HEADER } else { flags::KEYFRAME },
                        )
                        .with_duration(i64::from(self.samples_per_frame)),
                    );
                    self.pts_cursor += i64::from(self.samples_per_frame);
                }
                Err(rusty_mp3::Error::Again) | Err(rusty_mp3::Error::Eof) => return Ok(()),
                Err(error) => {
                    return Err(Error::backend(format!("{ENCODER_CRATE} encode failed: {error}")))
                }
            }
        }
    }
}

impl Encoder for Mp3Encoder {
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
        if audio.sample_rate != self.sample_rate {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder was configured for {} Hz but received {} Hz",
                self.sample_rate, audio.sample_rate
            )));
        }
        if audio.channels.channels() != u32::from(self.channels) {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder was configured for {} but received {}",
                self.channels,
                audio.channels.name()
            )));
        }
        if self.first_frame {
            self.pts_cursor = audio.pts;
            self.first_frame = false;
        }
        let samples = interleaved_f32(&audio)?;
        self.inner
            .push_pcm_f32(&samples, self.channels, self.sample_rate)
            .map_err(|error| {
                Error::invalid_argument(format!("{ENCODER_CRATE} rejected the input: {error}"))
            })?;
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
            self.inner.finish();
            // `finish` prepends the `Xing`/`Info` frame to the queue.
            self.header_pending = true;
            self.finished = true;
        }
        self.drain()?;
        Ok(std::mem::take(&mut self.pending))
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        if self.pushed {
            return Err(Error::unsupported(
                "the mp3 stream header is fixed by the first frame; \
                 change the bitrate before pushing audio",
            ));
        }
        self.cbr_kbps = u32::try_from(bitrate / 1_000).unwrap_or(u32::MAX);
        self.inner = Self::make_inner(self.cbr_kbps, self.vbr_quality);
        Ok(())
    }
}

/// Registers the pure-Rust MP3 encoder and decoder.
pub fn register(registry: &mut Registry) {
    registry.register_decoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &DecoderConfig| Ok(Some(Box::new(Mp3Decoder::new(config)?))),
        probe(CODEC, DECODER_CRATE, Direction::Decode),
    );
    registry.register_encoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &EncoderConfig| Ok(Some(Box::new(Mp3Encoder::new(config)?))),
        probe(CODEC, ENCODER_CRATE, Direction::Encode),
    );
}

/// Says why this module cannot serve `codec`, or `None` when it can.
pub fn unavailable(codec: Codec, direction: Direction) -> Option<BackendAvailability> {
    if codec == CODEC {
        None
    } else {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} tier serves mp3 only, not {codec} ({})",
            direction.name()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::format::{ChannelLayout, SampleFormat};
    use crate::soft::builtin::pure::testing::{best_rms, interleave, sine};

    fn decoder_config() -> DecoderConfig {
        DecoderConfig::new(CODEC).with_audio(44_100, ChannelLayout::Stereo)
    }

    fn encoder_config() -> EncoderConfig {
        EncoderConfig::audio(AudioCodec::Mp3, 44_100, ChannelLayout::Stereo).with_bitrate(128_000)
    }

    #[test]
    fn the_sample_rate_table_is_the_mpeg_set() {
        for rate in SAMPLE_RATES {
            assert!(is_mpeg_rate(rate));
            assert!(samples_per_frame(rate) == 1_152 || samples_per_frame(rate) == 576);
        }
        assert!(!is_mpeg_rate(44_000));
        assert_eq!(samples_per_frame(44_100), 1_152);
        assert_eq!(samples_per_frame(24_000), 576);
    }

    #[test]
    fn the_decoder_rejects_any_other_codec() {
        for codec in Codec::ALL {
            let config = DecoderConfig::new(codec).with_audio(44_100, ChannelLayout::Stereo);
            let result = Mp3Decoder::new(&config);
            if codec == CODEC {
                assert!(result.is_ok(), "{codec} must be accepted");
            } else {
                let error = result.err().expect("only mp3 may be accepted");
                assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
                assert!(error.message().contains("mp3"), "{}", error.message());
            }
        }
    }

    #[test]
    fn the_decoder_ignores_extradata_and_header_packets() {
        let config = DecoderConfig::new(CODEC)
            .with_audio(44_100, ChannelLayout::Stereo)
            .with_extradata(Buffer::from_slice(&[1, 2, 3]));
        let mut decoder = Mp3Decoder::new(&config).unwrap();
        decoder.push(Packet::header(CODEC, Buffer::from_slice(&[9; 4]))).unwrap();
        assert!(decoder.receive().unwrap().is_none());
    }

    #[test]
    fn the_encoder_rejects_channels_and_rates_mp3_cannot_carry() {
        let mut config = encoder_config();
        config.channels = ChannelLayout::FiveOne;
        let error = Mp3Encoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("2 channels"), "{}", error.message());

        let mut config = encoder_config();
        config.sample_rate = 44_000;
        let error = Mp3Encoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("44000"), "{}", error.message());
    }

    #[test]
    fn the_encoder_rejects_a_video_frame_and_a_bad_vbr_option() {
        let mut encoder = Mp3Encoder::new(&encoder_config()).unwrap();
        let frame = Frame::Video(
            crate::media::frame::VideoFrame::new(
                crate::media::format::PixelFormat::Rgba8,
                1,
                1,
                vec![crate::media::frame::Plane::packed(Buffer::from_slice(&[0, 0, 0, 0])).unwrap()],
                0,
            )
            .unwrap(),
        );
        assert_eq!(
            encoder.push(frame).unwrap_err().kind(),
            crate::error::ErrorKind::InvalidArgument
        );

        let mut config = encoder_config();
        config.options.push(("vbr_quality".into(), "loud".into()));
        assert_eq!(
            Mp3Encoder::new(&config).unwrap_err().kind(),
            crate::error::ErrorKind::InvalidArgument
        );
    }

    #[test]
    fn flushing_without_input_is_a_state_error() {
        let mut encoder = Mp3Encoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.flush().unwrap_err().kind(), crate::error::ErrorKind::State);
    }

    #[test]
    fn pushes_after_flush_are_rejected() {
        let mut encoder = Mp3Encoder::new(&encoder_config()).unwrap();
        let frame = crate::media::frame::AudioFrame::from_f32_planar(
            44_100,
            ChannelLayout::Stereo,
            4,
            vec![vec![0.0; 4]; 2],
            0,
        )
        .unwrap();
        encoder.push(Frame::Audio(frame.clone())).unwrap();
        encoder.flush().unwrap();
        assert_eq!(encoder.push(Frame::Audio(frame)).unwrap_err().kind(), crate::error::ErrorKind::State);
    }

    #[test]
    fn the_paths_name_the_crate_and_the_software_tier() {
        let decoder = Mp3Decoder::new(&decoder_config()).unwrap();
        let path = decoder.path();
        assert_eq!(path.backend, BackendKind::Rust);
        assert!(path.is_software());
        assert!(!path.hardware_accelerated);
        assert!(path.detail.contains(DECODER_CRATE), "{}", path.detail);

        let encoder = Mp3Encoder::new(&encoder_config()).unwrap();
        let path = encoder.path();
        assert_eq!(path.backend, BackendKind::Rust);
        assert!(path.is_software());
        assert!(!path.hardware_accelerated);
        assert!(path.detail.contains(ENCODER_CRATE), "{}", path.detail);
    }

    #[test]
    fn a_truncated_bitstream_is_an_error_not_a_panic() {
        let mut decoder = Mp3Decoder::new(&decoder_config()).unwrap();
        // A sync word claiming a frame, with nothing behind it.
        decoder.push(Packet::new(CODEC, Buffer::from_slice(&[0xFF, 0xFB, 0x90, 0x00]), 0, 0)).unwrap();
        assert!(decoder.receive().unwrap().is_none());
        assert!(decoder.flush().is_ok());
    }

    /// The end-to-end proof: encode with the pure-Rust MP3 encoder and decode
    /// with the pure-Rust MP3 decoder. MP3 is lossy, so the comparison is the
    /// best RMS error over every plausible codec delay rather than an equality.
    #[test]
    fn software_mp3_round_trips_through_the_software_decoder() {
        let frames = 8_192usize;
        let left = sine(frames, 441.0, 44_100.0);
        let right = sine(frames, 660.0, 44_100.0);
        let planar = vec![left.clone(), right.clone()];

        let mut encoder = Mp3Encoder::new(&encoder_config()).unwrap();
        let frame = crate::media::frame::AudioFrame::from_f32_planar(
            44_100,
            ChannelLayout::Stereo,
            frames as u32,
            planar.clone(),
            0,
        )
        .unwrap();
        encoder.push(Frame::Audio(frame)).unwrap();
        let packets = encoder.flush().unwrap();
        assert!(packets.len() > 1, "an Info frame and several audio frames");
        assert!(packets[0].is_header(), "the Info frame comes first");

        let mut decoder = Mp3Decoder::new(&decoder_config()).unwrap();
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
        assert!(channels >= 1);
        assert!(
            decoded.len() / channels >= frames,
            "decoded {} sample frames, expected at least {frames}",
            decoded.len() / channels
        );

        let expected = interleave(&left, &right, channels);
        let rms = best_rms(&expected, &decoded, 4_608);
        assert!(rms < 0.1, "worst-case RMS error {rms} is too large for 128 kbps MP3");
    }

    #[test]
    fn the_decoder_output_is_interleaved_f32() {
        let frames = 2_048usize;
        let mono = sine(frames, 300.0, 44_100.0);
        let mut encoder = Mp3Encoder::new(&EncoderConfig::audio(
            AudioCodec::Mp3,
            44_100,
            ChannelLayout::Mono,
        ))
        .unwrap();
        let frame = crate::media::frame::AudioFrame::from_f32_planar(
            44_100,
            ChannelLayout::Mono,
            frames as u32,
            vec![mono],
            0,
        )
        .unwrap();
        encoder.push(Frame::Audio(frame)).unwrap();
        let packets = encoder.flush().unwrap();

        let mut decoder = Mp3Decoder::new(
            &DecoderConfig::new(CODEC).with_audio(44_100, ChannelLayout::Mono),
        )
        .unwrap();
        for packet in &packets {
            decoder.push(packet.clone()).unwrap();
        }
        let frame = decoder.receive().unwrap().expect("a decoded frame");
        let audio = frame.as_audio().unwrap();
        assert_eq!(audio.format, SampleFormat::F32);
        assert_eq!(audio.channels, ChannelLayout::Mono);
        assert_eq!(audio.sample_rate, 44_100);
        // Interleaved means one sample per channel per frame.
        assert_eq!(audio.byte_len(), audio.frames as usize * 4);
    }

    #[test]
    fn unavailable_names_the_codec_it_cannot_serve() {
        assert!(unavailable(CODEC, Direction::Decode).is_none());
        let note = unavailable(Codec::Audio(AudioCodec::Flac), Direction::Decode)
            .expect("flac is not this module's codec");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("flac"));
    }
}
