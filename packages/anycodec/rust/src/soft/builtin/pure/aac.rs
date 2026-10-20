//! Pure-Rust **AAC** (MPEG-4 Advanced Audio Coding) decode *and* encode, on
//! [`rusty_aac`].
//!
//! ## Decode
//!
//! Two input shapes reach the same decoder:
//!
//! * **ADTS** elementary streams, where each packet starts with its own header — //!   no `extradata` is needed and the decoder learns its configuration from the
//!   first header; and
//! * **raw access units** (what an MP4 `stsd`/`esds` carries), which carry no
//!   configuration at all, so the `AudioSpecificConfig` *must* arrive through
//!   [`DecoderConfig::extradata`]. A caller that pushes a bare access unit
//!   without one gets an [`crate::error::ErrorKind::InvalidArgument`] naming exactly what
//!   is missing rather than silence.
//!
//! ## Encode
//!
//! The encoder produces **raw access units** and exposes the matching
//! `AudioSpecificConfig` through [`Encoder::extradata`], so a caller can round
//! trip its own output without a container. AAC is lossy, so the encoder's
//! packets are *not* a lossless representation of the input.

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, ErrorKind, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::{AudioCodec, Codec};
use crate::media::config::{DecoderConfig, EncoderConfig};
use crate::media::frame::Frame;
use crate::media::packet::{flags, Packet};

use crate::soft::PRIORITY_BUILTIN;
use super::{audio_frame_from_interleaved_f32, interleaved_f32, layout_for, probe, Direction};

/// The one codec this module serves.
pub(crate) const CODEC: Codec = Codec::Audio(AudioCodec::Aac);

/// The crate that decodes it.
pub(crate) const DECODER_CRATE: &str = "rusty_aac";

/// The crate that encodes it.
pub(crate) const ENCODER_CRATE: &str = "rusty_aac";

/// Samples per channel in one AAC frame this encoder/decoder pair uses (LC).
pub(crate) const FRAME_LEN: u32 = 1_024;

/// The channels the pure-Rust AAC encoder can lay out.
///
/// Seven and eight channels need a `program_config_element`, which `rusty_aac`'s
/// encoder does not write, so 7.1 is deliberately not claimed by this tier.
pub(crate) const ENCODABLE_CHANNELS: [u32; 5] = [1, 2, 3, 4, 6];

/// The `AudioSpecificConfig` bytes an AAC-LC stream at this geometry needs.
pub fn audio_specific_config(sample_rate: u32, channels: u32) -> Buffer {
    Buffer::from_vec(rusty_aac::audio_specific_config_bytes(sample_rate, channels as u16))
}

/// A pure-Rust AAC decoder.
pub struct AacDecoder {
    path: CodecPath,
    inner: rusty_aac::AacDecoder,
    /// `true` when an `AudioSpecificConfig` was supplied, which is the only way
    /// a bare (non-ADTS) access unit can be decoded.
    configured: bool,
    pending: Vec<Frame>,
    first_packet: bool,
    pts_cursor: i64,
}

impl std::fmt::Debug for AacDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AacDecoder")
            .field("path", &self.path.to_string())
            .field("configured", &self.configured)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl AacDecoder {
    /// Builds a decoder from `config`.
    ///
    /// Fails with [`crate::error::ErrorKind::Unsupported`] for any codec other than AAC, and
    /// with [`crate::error::ErrorKind::InvalidArgument`] when `extradata` is present but not a
    /// readable `AudioSpecificConfig`.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {DECODER_CRATE} decoder produces aac only, not {}",
                config.codec
            )));
        }
        let configured = !config.extradata.is_empty();
        let inner = if configured {
            rusty_aac::AacDecoder::with_config_bytes(config.extradata.as_slice()).map_err(
                |error| {
                    Error::invalid_argument(format!(
                        "the {DECODER_CRATE} decoder rejected the {} bytes of extradata; it \
                         needs an AudioSpecificConfig (the MP4 `esds` DecoderSpecificInfo): \
                         {error}",
                        config.extradata.len()
                    ))
                },
            )?
        } else {
            // No configuration: only ADTS framing can be decoded.
            rusty_aac::AacDecoder::new()
        };
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {DECODER_CRATE} decoder"),
            ),
            inner,
            configured,
            pending: Vec::new(),
            first_packet: true,
            pts_cursor: 0,
        })
    }

    /// `true` when this decoder was given an `AudioSpecificConfig`.
    pub const fn is_configured(&self) -> bool {
        self.configured
    }
}

impl Decoder for AacDecoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_eos() {
            return Ok(());
        }
        if packet.is_header() {
            // Configuration reaches this tier through `DecoderConfig::extradata`.
            return Ok(());
        }
        if packet.is_empty() {
            return Ok(());
        }
        if !self.configured && !rusty_aac::is_adts(packet.data.as_slice()) {
            return Err(Error::invalid_argument(format!(
                "the {DECODER_CRATE} decoder was given a bare access unit but no \
                 AudioSpecificConfig: supply the MP4 `esds`/`stsd` configuration bytes in \
                 `DecoderConfig::extradata`, or push ADTS-framed packets"
            )));
        }
        if self.first_packet {
            self.pts_cursor = packet.pts;
            self.first_packet = false;
        }
        match self.inner.decode(packet.data.as_slice(), Some(self.pts_cursor)) {
            Ok(audio) => {
                let channels = u32::from(audio.channels.max(1));
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
                Ok(())
            }
            Err(rusty_aac::Error::Again) | Err(rusty_aac::Error::Eof) => Ok(()),
            Err(rusty_aac::Error::InvalidData(detail)) => {
                Err(Error::backend(format!("{DECODER_CRATE} decode error: {detail}")))
            }
            Err(rusty_aac::Error::Unsupported(detail)) => Err(Error::new(
                ErrorKind::Unsupported,
                format!("{DECODER_CRATE} cannot decode this aac stream: {detail}"),
            )),
            Err(error) => Err(Error::backend(format!("{DECODER_CRATE} decode failed: {error}"))),
        }
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        Ok(std::mem::take(&mut self.pending))
    }
}

/// A pure-Rust AAC-LC encoder.
pub struct AacEncoder {
    path: CodecPath,
    inner: rusty_aac::AacEncoder,
    sample_rate: u32,
    channels: u32,
    extradata: Buffer,
    pending: Vec<Packet>,
    pts_cursor: i64,
    first_frame: bool,
    pushed: bool,
    finished: bool,
}

impl std::fmt::Debug for AacEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AacEncoder")
            .field("path", &self.path.to_string())
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl AacEncoder {
    /// Builds an encoder from `config`.
    ///
    /// The target bitrate comes from [`EncoderConfig::bitrate`] (defaulting to
    /// 128 kbit/s when it is zero). 7.1 and non-standard sample rates are
    /// rejected with a message naming the constraint, because `rusty_aac`'s
    /// encoder would otherwise fail later with less context.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {ENCODER_CRATE} encoder produces aac only, not {}",
                config.codec
            )));
        }
        let channels = config.channels.channels();
        if !ENCODABLE_CHANNELS.contains(&channels) {
            return Err(Error::invalid_argument(format!(
                "{ENCODER_CRATE} encodes {ENCODABLE_CHANNELS:?} channels; {channels} would need \
                 a program_config_element it does not write"
            )));
        }
        if rusty_aac::sf_index_for_rate(config.sample_rate).is_none() {
            return Err(Error::invalid_argument(format!(
                "{ENCODER_CRATE} needs one of the thirteen standard AAC sample rates \
                 ({}), got {}",
                rusty_aac::SAMPLE_RATES
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                config.sample_rate
            )));
        }
        let bitrate_bps = u32::try_from(config.bitrate).unwrap_or(u32::MAX);
        let encoder_config = rusty_aac::AacEncoderConfig {
            bitrate_bps: if bitrate_bps == 0 { 128_000 } else { bitrate_bps },
            ..Default::default()
        };
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {ENCODER_CRATE} encoder"),
            ),
            inner: rusty_aac::AacEncoder::new(encoder_config),
            sample_rate: config.sample_rate,
            channels,
            extradata: audio_specific_config(config.sample_rate, channels),
            pending: Vec::new(),
            pts_cursor: 0,
            first_frame: true,
            pushed: false,
            finished: false,
        })
    }

    /// The `AudioSpecificConfig` written so far, for callers that want it before
    /// [`Encoder::extradata`] is meaningful.
    pub fn config_bytes(&self) -> &Buffer {
        &self.extradata
    }

    /// `true` once at least one audio frame has been accepted.
    pub const fn has_input(&self) -> bool {
        self.pushed
    }

    fn drain(&mut self) -> Result<()> {
        loop {
            match self.inner.next_packet() {
                Ok(packet) => {
                    let duration =
                        if packet.duration == 0 { FRAME_LEN } else { packet.duration };
                    self.pending.push(
                        Packet::new(
                            CODEC,
                            Buffer::from_vec(packet.data),
                            self.pts_cursor,
                            flags::KEYFRAME,
                        )
                        .with_duration(i64::from(duration)),
                    );
                    self.pts_cursor += i64::from(duration);
                }
                Err(rusty_aac::Error::Again) | Err(rusty_aac::Error::Eof) => return Ok(()),
                Err(error) => {
                    return Err(Error::backend(format!("{ENCODER_CRATE} encode failed: {error}")))
                }
            }
        }
    }
}

impl Encoder for AacEncoder {
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
        if audio.channels.channels() != self.channels {
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
        self.inner
            .push_pcm(&samples, self.channels as u16, self.sample_rate)
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
            self.finished = true;
            self.drain()?;
        }
        Ok(std::mem::take(&mut self.pending))
    }

    fn extradata(&self) -> Option<Buffer> {
        Some(self.extradata.clone())
    }
}

/// Registers the pure-Rust AAC encoder and decoder.
pub fn register(registry: &mut Registry) {
    registry.register_decoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &DecoderConfig| Ok(Some(Box::new(AacDecoder::new(config)?))),
        probe(CODEC, DECODER_CRATE, Direction::Decode),
    );
    registry.register_encoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &EncoderConfig| Ok(Some(Box::new(AacEncoder::new(config)?))),
        probe(CODEC, ENCODER_CRATE, Direction::Encode),
    );
}

/// Says why this module cannot serve `codec`, or `None` when it can.
pub fn unavailable(codec: Codec, direction: Direction) -> Option<BackendAvailability> {
    if codec == CODEC {
        None
    } else {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} tier serves aac only, not {codec} ({})",
            direction.name()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::format::ChannelLayout;
    use crate::soft::builtin::pure::testing::{best_rms, interleave, sine};

    const RATE: u32 = 44_100;

    fn decoder_config() -> DecoderConfig {
        DecoderConfig::new(CODEC).with_audio(RATE, ChannelLayout::Stereo)
    }

    fn encoder_config() -> EncoderConfig {
        EncoderConfig::audio(AudioCodec::Aac, RATE, ChannelLayout::Stereo).with_bitrate(128_000)
    }

    #[test]
    fn a_stereo_44100_config_is_two_bytes() {
        // object_type 2, sf_index 4, channel configuration 2 — the value every
        // MP4 muxer stores for this geometry.
        assert_eq!(audio_specific_config(44_100, 2).as_slice(), &[0x12, 0x10]);
    }

    #[test]
    fn the_frame_length_matches_the_crate() {
        assert_eq!(FRAME_LEN as usize, rusty_aac::encode::FRAME_LEN);
    }

    #[test]
    fn the_decoder_rejects_any_other_codec() {
        for codec in Codec::ALL {
            let config = DecoderConfig::new(codec).with_audio(RATE, ChannelLayout::Stereo);
            let result = AacDecoder::new(&config);
            if codec == CODEC {
                assert!(result.is_ok(), "{codec} must be accepted");
            } else {
                let error = result.err().expect("only aac may be accepted");
                assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
                assert!(error.message().contains("aac"), "{}", error.message());
            }
        }
    }

    #[test]
    fn a_bare_access_unit_without_extradata_names_what_is_missing() {
        let mut decoder = AacDecoder::new(&decoder_config()).unwrap();
        assert!(!decoder.is_configured());
        let error = decoder
            .push(Packet::new(CODEC, Buffer::from_slice(&[0x21, 0x00, 0x11]), 0, 0))
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("AudioSpecificConfig"), "{}", error.message());
    }

    #[test]
    fn a_malformed_extradata_is_rejected_with_a_readable_reason() {
        let config = decoder_config().with_extradata(Buffer::from_slice(&[]));
        // An empty buffer means "unconfigured", not "malformed" — it is accepted
        // as an ADTS-only decoder.
        assert!(AacDecoder::new(&config).is_ok());

        let config = decoder_config().with_extradata(Buffer::from_slice(&[0xFF]));
        let error = AacDecoder::new(&config).unwrap_err();
        assert!(
            matches!(
                error.kind(),
                crate::error::ErrorKind::InvalidArgument | crate::error::ErrorKind::Unsupported
            ),
            "unexpected kind {:?}",
            error.kind()
        );
        assert!(error.message().contains("AudioSpecificConfig"), "{}", error.message());
    }

    #[test]
    fn the_encoder_rejects_geometry_aac_cannot_carry() {
        let mut config = encoder_config();
        config.channels = ChannelLayout::SevenOne;
        let error = AacEncoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("8"), "{}", error.message());

        let mut config = encoder_config();
        config.sample_rate = 44_000;
        let error = AacEncoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("44000"), "{}", error.message());
    }

    #[test]
    fn flushing_without_input_is_a_state_error() {
        let mut encoder = AacEncoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.flush().unwrap_err().kind(), crate::error::ErrorKind::State);
    }

    #[test]
    fn the_encoder_exposes_the_audio_specific_config() {
        let encoder = AacEncoder::new(&encoder_config()).unwrap();
        let extradata = encoder.extradata().expect("an AAC encoder has extradata");
        assert_eq!(extradata.as_slice(), &[0x12, 0x10]);
        assert_eq!(extradata.as_slice(), encoder.config_bytes().as_slice());
    }

    #[test]
    fn the_paths_name_the_crate_and_the_software_tier() {
        let decoder = AacDecoder::new(&decoder_config()).unwrap();
        assert_eq!(decoder.path().backend, BackendKind::Rust);
        assert!(decoder.path().is_software());
        assert!(!decoder.path().hardware_accelerated);
        assert!(decoder.path().detail.contains(DECODER_CRATE));

        let encoder = AacEncoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.path().backend, BackendKind::Rust);
        assert!(encoder.path().is_software());
        assert!(!encoder.path().hardware_accelerated);
        assert!(encoder.path().detail.contains(ENCODER_CRATE));
    }

    /// The end-to-end proof: encode raw access units with the pure-Rust AAC
    /// encoder, configure the pure-Rust AAC decoder from the encoder's own
    /// `AudioSpecificConfig`, and compare the waveforms. AAC is lossy, so the
    /// comparison allows for the codec delay.
    #[test]
    fn software_aac_round_trips_through_the_software_decoder() {
        let frames = 8_192usize;
        let left = sine(frames, 441.0, RATE as f32);
        let right = sine(frames, 660.0, RATE as f32);
        let planar = vec![left.clone(), right.clone()];

        let mut encoder = AacEncoder::new(&encoder_config()).unwrap();
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
        assert!(!packets.is_empty(), "one raw access unit per 1024 samples");
        let extradata = encoder.extradata().expect("an AAC encoder has extradata");

        let mut decoder = AacDecoder::new(&decoder_config().with_extradata(extradata)).unwrap();
        assert!(decoder.is_configured());
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
        let rms = best_rms(&expected, &decoded, 4_096);
        assert!(rms < 0.1, "worst-case RMS error {rms} is too large for 128 kbps AAC");
    }

    #[test]
    fn unavailable_names_the_codec_it_cannot_serve() {
        assert!(unavailable(CODEC, Direction::Encode).is_none());
        let note = unavailable(Codec::Video(crate::media::codec::VideoCodec::Vp9), Direction::Decode)
            .expect("vp9 is not this module's codec");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("vp9"));
    }
}
