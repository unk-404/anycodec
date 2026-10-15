//! Pure-Rust **ALAC** (Apple Lossless) decoding, on [`alac`].
//!
//! ALAC is a packet codec with no in-band configuration: a *magic cookie* 鈥?the
//! `alac` atom's `ALACSpecificConfig` 鈥?carries the frame length, bit depth,
//! channel count and sample rate that every packet's Rice parameters depend on.
//! So [`DecoderConfig::extradata`] is mandatory here, and a decoder built
//! without it is refused by name rather than producing silence.
//!
//! Decoding is exact: ALAC is lossless, so the samples that come back are the
//! samples that went in, at the bit depth the cookie declares.
//!
//! ## What is *not* claimed
//!
//! `alac` has no encoder, so ALAC is decode-only in this tier. (`afconvert` and
//! Apple's own encoder are the reference producers; there is no pure-Rust ALAC
//! encoder in the dependency set.)

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::Decoder;
use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, Result};
use crate::media::codec::{AudioCodec, Codec};
use crate::media::config::DecoderConfig;
use crate::media::frame::Frame;
use crate::media::packet::Packet;

use crate::soft::PRIORITY_BUILTIN;
use super::{audio_frame_from_interleaved_f32, layout_for, probe, Direction};

/// The one codec this module serves.
pub(crate) const CODEC: Codec = Codec::Audio(AudioCodec::Alac);

/// The crate that decodes it.
pub(crate) const DECODER_CRATE: &str = "alac";

/// The shortest valid magic cookie: the 24-byte `ALACSpecificConfig`.
pub(crate) const COOKIE_LEN: usize = 24;

/// Full scale of the left-aligned `i32` samples `alac` produces.
const I32_FULL_SCALE: f32 = 2_147_483_648.0;

/// A pure-Rust ALAC decoder.
pub struct AlacDecoder {
    path: CodecPath,
    inner: alac::Decoder,
    channels: u32,
    sample_rate: u32,
    bits_per_sample: u8,
    /// `alac` writes into a caller slice sized for the cookie's maximum packet.
    scratch: Vec<i32>,
    pending: Vec<Frame>,
    first_packet: bool,
    pts_cursor: i64,
}

impl std::fmt::Debug for AlacDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlacDecoder")
            .field("path", &self.path.to_string())
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl AlacDecoder {
    /// Builds a decoder from `config`.
    ///
    /// `config.extradata` must be the ALAC magic cookie (with or without the
    /// `frma`/`alac` atom wrappers an MP4 `alac` box carries). Fails with
    /// [`crate::error::ErrorKind::Unsupported`] for any codec other than ALAC and with
    /// [`crate::error::ErrorKind::InvalidArgument`] when the cookie is missing or unreadable.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {DECODER_CRATE} decoder produces alac only, not {}",
                config.codec
            )));
        }
        if config.extradata.is_empty() {
            return Err(Error::invalid_argument(format!(
                "the {DECODER_CRATE} decoder needs an ALAC magic cookie in \
                 `DecoderConfig::extradata` (the `alac` atom's ALACSpecificConfig, at least \
                 {COOKIE_LEN} bytes); ALAC packets carry no configuration of their own"
            )));
        }
        let cookie = config.extradata.as_slice();
        let info = alac::StreamInfo::from_cookie(cookie).map_err(|error| {
            Error::invalid_argument(format!(
                "the {DECODER_CRATE} decoder rejected the {} bytes of extradata as an ALAC magic \
                 cookie; it needs the `alac` atom's ALACSpecificConfig (at least {COOKIE_LEN} \
                 bytes): {error}",
                cookie.len()
            ))
        })?;
        let channels = u32::from(info.channels());
        // Validate the channel count here, so an unusable cookie is refused by
        // `new` rather than by the first packet.
        layout_for(CODEC, channels)?;
        let max_samples = info.max_samples_per_packet() as usize;
        if max_samples == 0 {
            return Err(Error::invalid_argument(
                "the ALAC magic cookie declares a zero maximum packet size",
            ));
        }
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {DECODER_CRATE} decoder"),
            ),
            inner: alac::Decoder::new(info.clone()),
            channels,
            sample_rate: info.sample_rate(),
            bits_per_sample: info.bit_depth(),
            scratch: vec![0i32; max_samples],
            pending: Vec::new(),
            first_packet: true,
            pts_cursor: 0,
        })
    }

    /// The number of channels the cookie declares.
    pub const fn channels(&self) -> u32 {
        self.channels
    }

    /// The sample rate the cookie declares.
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The bit depth the cookie declares.
    pub const fn bits_per_sample(&self) -> u8 {
        self.bits_per_sample
    }
}

impl Decoder for AlacDecoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_eos() || packet.is_header() || packet.is_empty() {
            // ALAC's configuration is `extradata`; there is no header packet.
            return Ok(());
        }
        if self.first_packet {
            self.pts_cursor = packet.pts;
            self.first_packet = false;
        }
        let channels = self.channels as usize;
        let (frames, interleaved) = {
            let scratch = &mut self.scratch;
            let decoded = self
                .inner
                .decode_packet::<i32>(packet.data.as_slice(), scratch)
                .map_err(|error| {
                    Error::backend(format!("{DECODER_CRATE} decode failed: {error}"))
                })?;
            if decoded.len() % channels != 0 {
                return Err(Error::backend(format!(
                    "{DECODER_CRATE} produced {} samples that are not a whole number of \
                     {channels}-channel frames",
                    decoded.len()
                )));
            }
            let frames = (decoded.len() / channels) as u32;
            // `alac` left-aligns each sample in its `i32`, so full scale is 2^31.
            let interleaved: Vec<f32> = decoded.iter().map(|&s| s as f32 / I32_FULL_SCALE).collect();
            (frames, interleaved)
        };
        if frames == 0 {
            return Ok(());
        }
        let layout = layout_for(CODEC, self.channels)?;
        let frame = audio_frame_from_interleaved_f32(
            self.sample_rate,
            layout,
            frames,
            &interleaved,
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

/// Registers the pure-Rust ALAC decoder.
pub fn register(registry: &mut Registry) {
    registry.register_decoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &DecoderConfig| Ok(Some(Box::new(AlacDecoder::new(config)?))),
        probe(CODEC, DECODER_CRATE, Direction::Decode),
    );
}

/// Says why this module cannot serve `codec` in `direction`, or `None` when it can.
pub fn unavailable(codec: Codec, direction: Direction) -> Option<BackendAvailability> {
    if codec == CODEC && direction == Direction::Decode {
        None
    } else if codec == CODEC {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} crate has no {} for {codec}",
            Direction::Encode.name()
        )))
    } else {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} tier serves alac decode only, not {codec} ({})",
            direction.name()
        )))
    }
}

/// Builds a 24-byte `ALACSpecificConfig` with the given geometry.
///
/// The layout is the one `alac::StreamInfo::from_cookie` reads: frame length,
/// compatible version, bit depth, `pb`, `mb`, `kb`, channel count, max run, max
/// frame bytes, average bit rate and sample rate, all big-endian where wider
/// than a byte.
#[cfg(test)]
pub fn cookie(frame_length: u32, bit_depth: u8, channels: u8, sample_rate: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(COOKIE_LEN);
    out.extend_from_slice(&frame_length.to_be_bytes());
    out.push(0); // compatible version
    out.push(bit_depth);
    out.push(40); // pb
    out.push(10); // mb
    out.push(14); // kb
    out.push(channels);
    out.extend_from_slice(&255u16.to_be_bytes()); // max run
    out.extend_from_slice(&0u32.to_be_bytes()); // max frame bytes
    out.extend_from_slice(&0u32.to_be_bytes()); // average bit rate
    out.extend_from_slice(&sample_rate.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::buffer::Buffer;
    use crate::media::format::ChannelLayout;

    fn decoder_config() -> DecoderConfig {
        DecoderConfig::new(CODEC)
            .with_audio(44_100, ChannelLayout::Stereo)
            .with_extradata(Buffer::from_vec(cookie(4_096, 16, 2, 44_100)))
    }

    #[test]
    fn the_cookie_layout_is_the_one_the_crate_reads() {
        let bytes = cookie(4_096, 16, 2, 44_100);
        assert_eq!(bytes.len(), COOKIE_LEN);
        let info = alac::StreamInfo::from_cookie(&bytes).unwrap();
        assert_eq!(info.max_frames_per_packet(), 4_096);
        assert_eq!(info.bit_depth(), 16);
        assert_eq!(info.channels(), 2);
        assert_eq!(info.sample_rate(), 44_100);
        assert_eq!(info.max_samples_per_packet(), 8_192);
    }

    #[test]
    fn the_decoder_rejects_any_other_codec() {
        for codec in Codec::ALL {
            let config = DecoderConfig::new(codec);
            let result = AlacDecoder::new(&config);
            if codec == CODEC {
                // ALAC always needs a cookie, so even its own codec fails here 鈥?                // the point is that it fails for *that* reason.
                assert!(result.is_err());
            } else {
                let error = result.err().expect("only alac may get past the codec check");
                assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
                assert!(error.message().contains("alac"), "{}", error.message());
            }
        }
    }

    #[test]
    fn a_missing_magic_cookie_names_what_is_missing() {
        let config = DecoderConfig::new(CODEC).with_audio(44_100, ChannelLayout::Stereo);
        let error = AlacDecoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("magic cookie"), "{}", error.message());
        assert!(error.message().contains("ALACSpecificConfig"), "{}", error.message());
    }

    #[test]
    fn a_short_magic_cookie_is_rejected_with_a_readable_reason() {
        let config = DecoderConfig::new(CODEC)
            .with_extradata(Buffer::from_slice(&[0u8; COOKIE_LEN - 1]));
        let error = AlacDecoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("magic cookie"), "{}", error.message());
    }

    #[test]
    fn a_valid_cookie_configures_the_decoder() {
        let decoder = AlacDecoder::new(&decoder_config()).unwrap();
        assert_eq!(decoder.channels(), 2);
        assert_eq!(decoder.sample_rate(), 44_100);
        assert_eq!(decoder.bits_per_sample(), 16);
        assert_eq!(decoder.path().backend, BackendKind::Rust);
        assert!(decoder.path().is_software());
        assert!(!decoder.path().hardware_accelerated);
        assert!(decoder.path().detail.contains(DECODER_CRATE));
    }

    #[test]
    fn an_unsupported_channel_count_is_named() {
        let config = DecoderConfig::new(CODEC)
            .with_extradata(Buffer::from_vec(cookie(4_096, 16, 5, 44_100)));
        let error = AlacDecoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
        assert!(error.message().contains('5'), "{}", error.message());
    }

    #[test]
    fn a_malformed_packet_is_an_error_not_a_panic() {
        let mut decoder = AlacDecoder::new(&decoder_config()).unwrap();
        for bytes in [vec![0u8; 1], vec![0xFF; 32], vec![0x07], vec![0xE0; 8]] {
            let _ = decoder.push(Packet::new(CODEC, Buffer::from_vec(bytes), 0, 0));
        }
        assert!(decoder.flush().is_ok());
    }

    #[test]
    fn eos_and_header_packets_are_accepted() {
        let mut decoder = AlacDecoder::new(&decoder_config()).unwrap();
        decoder.push(Packet::eos(CODEC)).unwrap();
        decoder.push(Packet::header(CODEC, Buffer::from_slice(&[0; 4]))).unwrap();
        assert!(decoder.receive().unwrap().is_none());
    }

    #[test]
    fn unavailable_names_the_direction_it_does_not_serve() {
        assert!(unavailable(CODEC, Direction::Decode).is_none());
        let note = unavailable(CODEC, Direction::Encode).expect("alac encode is not served");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("encoder"));
        let note = unavailable(Codec::Audio(AudioCodec::Flac), Direction::Decode)
            .expect("flac is not this module's codec");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("flac"));
    }
}
