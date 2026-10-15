//! A pure-Rust software **FLAC encoder**.
//!
//! This is what makes the software *encode* path concrete: it needs no FFmpeg,
//! no C toolchain and no platform framework, and its output round-trips through
//! [`super::symphonia_dec`], which gives AnyCodec an end-to-end software test
//! that runs on every host.
//!
//! ## Shape of the output
//!
//! FLAC is a lossless, file-oriented format: the `STREAMINFO` metadata block
//! carries global state (block size, frame size, MD5) that can only be known once
//! the whole input has been seen. So this encoder buffers samples and produces,
//! on [`Encoder::flush`], exactly two packets:
//!
//! 1. a `HEADER` packet carrying the 34-byte `STREAMINFO` block body — this is
//!    the value to feed back in as `DecoderConfig::extradata`; and
//! 2. one packet carrying every encoded frame.
//!
//! [`Encoder::extradata`] also returns the `STREAMINFO` body, so a caller that
//! forgets to keep packet 1 can still store the configuration.

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::Encoder;
use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::{AudioCodec, Codec};
use crate::media::config::EncoderConfig;
use crate::media::format::{ChannelLayout, SampleFormat};
use crate::media::frame::{AudioFrame, Frame};
use crate::media::packet::{flags, Packet};

use flacenc::component::BitRepr;
use flacenc::error::Verify;

use super::super::PRIORITY_BUILTIN;

/// The one codec this encoder produces.
pub(crate) const CODEC: Codec = Codec::Audio(AudioCodec::Flac);

/// The default bit depth when `EncoderConfig::option("bits_per_sample")` is unset.
pub(crate) const DEFAULT_BITS_PER_SAMPLE: u8 = 16;

/// The FLAC block size used for every frame; 4096 is the reference default.
pub(crate) const BLOCK_SIZE: usize = 4096;

/// The codecs this encoder handles.
pub fn registered_codecs() -> Vec<Codec> {
    vec![CODEC]
}

/// A pure-Rust FLAC encoder.
#[derive(Debug)]
pub struct FlacEncoder {
    path: CodecPath,
    sample_rate: u32,
    channels: ChannelLayout,
    bits_per_sample: u8,
    /// Interleaved samples already scaled to `bits_per_sample`.
    samples: Vec<i32>,
    frames: u64,
    first_pts: i64,
    streaminfo: Option<Buffer>,
    pending: Vec<Packet>,
}

impl FlacEncoder {
    /// Builds an encoder from `config`.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the built-in software FLAC encoder cannot produce {}",
                config.codec
            )));
        }
        let bits_per_sample = match config.option("bits_per_sample") {
            Some(text) => text.parse::<u8>().map_err(|_| {
                Error::invalid_argument(format!("`bits_per_sample` is not a number: {text:?}"))
            })?,
            None => DEFAULT_BITS_PER_SAMPLE,
        };
        if !(4..=24).contains(&bits_per_sample) {
            return Err(Error::invalid_argument(format!(
                "`bits_per_sample` must be 4..=24, got {bits_per_sample}"
            )));
        }
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: flacenc {bits_per_sample}-bit encoder"),
            ),
            sample_rate: config.sample_rate,
            channels: config.channels,
            bits_per_sample,
            samples: Vec::new(),
            frames: 0,
            first_pts: 0,
            streaminfo: None,
            pending: Vec::new(),
        })
    }

    /// Number of sample frames buffered so far.
    pub fn buffered_frames(&self) -> u64 {
        self.frames
    }

    /// The `STREAMINFO` block body, once [`Encoder::flush`] has run.
    pub fn streaminfo(&self) -> Option<&Buffer> {
        self.streaminfo.as_ref()
    }

    /// Appends one audio frame, converting it to the configured bit depth.
    fn append(&mut self, frame: &AudioFrame) -> Result<()> {
        if frame.sample_rate != self.sample_rate {
            return Err(Error::invalid_argument(format!(
                "the FLAC encoder was configured for {} Hz but received {} Hz",
                self.sample_rate, frame.sample_rate
            )));
        }
        if frame.channels != self.channels {
            return Err(Error::invalid_argument(format!(
                "the FLAC encoder was configured for {} but received {}",
                self.channels.name(),
                frame.channels.name()
            )));
        }
        if self.frames == 0 {
            self.first_pts = frame.pts;
        }
        let scale = Scale::new(frame.format, self.bits_per_sample)?;
        let expected = frame.channels.byte_len(frame.frames, frame.format);
        let bytes = &frame.samples.as_slice()[..expected];
        self.samples.reserve(frame.frames as usize * self.channels.channels() as usize);
        for chunk in bytes.chunks_exact(scale.bytes_per_sample()) {
            self.samples.push(scale.sample(chunk));
        }
        self.frames += frame.frames as u64;
        Ok(())
    }

    /// Encodes everything buffered so far into `self.pending`.
    fn encode_buffered(&mut self) -> Result<()> {
        if self.frames == 0 {
            return Err(Error::state("the FLAC encoder was flushed with no input"));
        }
        let config = flacenc::config::Encoder::default()
            .into_verified()
            .map_err(|(_, error)| {
                Error::internal(format!("the built-in FLAC encoder config is invalid: {error}"))
            })?;
        let source = flacenc::source::MemSource::from_samples(
            &self.samples,
            self.channels.channels() as usize,
            self.bits_per_sample as usize,
            self.sample_rate as usize,
        );
        let stream = flacenc::encode_with_fixed_block_size(&config, source, BLOCK_SIZE)
            .map_err(|error| Error::backend(format!("FLAC encoding failed: {error:?}")))?;

        let mut sink = flacenc::bitsink::ByteSink::new();
        stream
            .write(&mut sink)
            .map_err(|error| Error::backend(format!("FLAC serialisation failed: {error:?}")))?;
        let bytes = sink.as_slice();
        let (streaminfo, frames) = split_flac_stream(bytes)?;

        self.streaminfo = Some(Buffer::from_slice(&streaminfo));
        self.pending.push(Packet::new(
            CODEC,
            self.streaminfo.clone().expect("just assigned"),
            0,
            flags::HEADER,
        ));
        self.pending.push(
            Packet::new(CODEC, Buffer::from_slice(&frames), self.first_pts, flags::KEYFRAME)
                .with_duration(self.frames as i64),
        );
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
            return Err(Error::invalid_argument("the FLAC encoder takes audio frames only"));
        };
        self.append(&audio)
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

/// Splits a complete FLAC stream into `(STREAMINFO body, frame bytes)`.
///
/// The stream is `"fLaC"`, then a chain of metadata blocks (1 byte of
/// `last`/`type` flags, 3 bytes of big-endian length, then the body), then the
/// audio frames. `STREAMINFO` is always the first block and is always 34 bytes.
pub fn split_flac_stream(stream: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if stream.len() < 8 || &stream[0..4] != b"fLaC" {
        return Err(Error::backend("the encoder did not produce a FLAC stream header"));
    }
    let mut cursor = 4usize;
    let mut streaminfo = None;
    loop {
        if cursor + 4 > stream.len() {
            return Err(Error::backend("the FLAC metadata block header is truncated"));
        }
        let header = stream[cursor];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7f;
        let length = ((stream[cursor + 1] as usize) << 16)
            | ((stream[cursor + 2] as usize) << 8)
            | (stream[cursor + 3] as usize);
        cursor += 4;
        if cursor + length > stream.len() {
            return Err(Error::backend("the FLAC metadata block body is truncated"));
        }
        if block_type == 0 {
            if length != 34 {
                return Err(Error::backend(format!(
                    "STREAMINFO must be 34 bytes, got {length}"
                )));
            }
            streaminfo = Some(stream[cursor..cursor + length].to_vec());
        }
        cursor += length;
        if is_last {
            break;
        }
    }
    let streaminfo = streaminfo
        .ok_or_else(|| Error::backend("the FLAC stream has no STREAMINFO metadata block"))?;
    Ok((streaminfo, stream[cursor..].to_vec()))
}

/// Converts one input sample format to the encoder's bit depth.
#[derive(Debug, Clone, Copy)]
enum Scale {
    F32 {
        headroom: f32,
    },
    S16 {
        shift: i32,
    },
    S32 {
        shift: i32,
    },
    U8 {
        shift: i32,
    },
}

impl Scale {
    fn new(format: SampleFormat, bits_per_sample: u8) -> Result<Self> {
        let bits = bits_per_sample as i32;
        Ok(match format {
            SampleFormat::F32 => {
                // Keep one bit of headroom so +1.0 cannot overflow the target.
                let headroom = ((1i64 << (bits - 1)) - 1) as f32;
                Self::F32 { headroom }
            }
            SampleFormat::S16 => Self::S16 { shift: bits - 16 },
            SampleFormat::S32 => Self::S32 { shift: bits - 32 },
            SampleFormat::U8 => Self::U8 { shift: bits - 8 },
        })
    }

    fn bytes_per_sample(self) -> usize {
        match self {
            Scale::F32 { .. } | Scale::S32 { .. } => 4,
            Scale::S16 { .. } => 2,
            Scale::U8 { .. } => 1,
        }
    }

    /// Converts one sample. `chunk` is exactly `bytes_per_sample()` long.
    fn sample(self, chunk: &[u8]) -> i32 {
        match self {
            Scale::F32 { headroom } => {
                let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                (value.clamp(-1.0, 1.0) * headroom).round() as i32
            }
            Scale::S16 { shift } => {
                let value = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
                if shift >= 0 {
                    value << shift
                } else {
                    value >> (-shift)
                }
            }
            Scale::S32 { shift } => {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if shift >= 0 {
                    value << shift
                } else {
                    value >> (-shift)
                }
            }
            Scale::U8 { shift } => {
                let value = (chunk[0] as i32) - 128;
                if shift >= 0 {
                    value << shift
                } else {
                    value >> (-shift)
                }
            }
        }
    }
}

/// Registers the built-in FLAC encoder.
pub fn register(registry: &mut Registry) {
    registry.register_encoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &EncoderConfig| Ok(Some(Box::new(FlacEncoder::new(config)?))),
        |codec: Codec| {
            if codec == CODEC {
                BackendAvailability::Available
            } else {
                BackendAvailability::Unavailable(format!(
                    "the built-in software tier can only encode flac, not {codec}"
                ))
            }
        },
    );
}

/// Builds an interleaved `f32` payload, for tests and embedders.
pub fn f32_frame(
    sample_rate: u32,
    channels: ChannelLayout,
    frames: u32,
    planes: Vec<Vec<f32>>,
    pts: i64,
) -> Result<AudioFrame> {
    AudioFrame::from_f32_planar(sample_rate, channels, frames, planes, pts)
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::soft::builtin::symphonia_dec::{interleaved_f32, SymphoniaAudioDecoder};
    use crate::engine::Decoder as _;
    use crate::media::config::DecoderConfig;

    fn sine(frames: usize, sample_rate: f32) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                (index as f32 * sample_rate * 2.0 * std::f32::consts::PI).sin() * 0.5
            })
            .collect()
    }

    fn config() -> EncoderConfig {
        EncoderConfig::audio(AudioCodec::Flac, 44_100, ChannelLayout::Stereo)
    }

    #[test]
    fn only_flac_is_accepted() {
        let mut config = config();
        config.codec = Codec::Audio(AudioCodec::Opus);
        let error = FlacEncoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
        assert!(error.message().contains("opus"));
    }

    #[test]
    fn bit_depth_option_is_validated() {
        let mut config = config();
        config.options.push(("bits_per_sample".into(), "99".into()));
        assert_eq!(
            FlacEncoder::new(&config).unwrap_err().kind(),
            crate::error::ErrorKind::InvalidArgument
        );
        config.options.clear();
        config.options.push(("bits_per_sample".into(), "wibble".into()));
        assert_eq!(
            FlacEncoder::new(&config).unwrap_err().kind(),
            crate::error::ErrorKind::InvalidArgument
        );
    }

    #[test]
    fn mismatched_geometry_is_rejected() {
        let mut encoder = FlacEncoder::new(&config()).unwrap();
        let frame = f32_frame(48_000, ChannelLayout::Stereo, 4, vec![vec![0.0; 4]; 2], 0).unwrap();
        let error = encoder.push(Frame::Audio(frame)).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("Hz"));
    }

    #[test]
    fn video_frames_are_rejected() {
        let mut encoder = FlacEncoder::new(&config()).unwrap();
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
    }

    #[test]
    fn flushing_without_input_is_a_state_error() {
        let mut encoder = FlacEncoder::new(&config()).unwrap();
        assert_eq!(encoder.flush().unwrap_err().kind(), crate::error::ErrorKind::State);
    }

    #[test]
    fn encoder_reports_the_rust_backend() {
        let encoder = FlacEncoder::new(&config()).unwrap();
        let path = encoder.path();
        assert_eq!(path.backend, BackendKind::Rust);
        assert!(path.is_software(), "the built-in encoder is the software fallback");
        assert!(!path.hardware_accelerated);
        assert!(path.detail.contains("flacenc"));
    }

    #[test]
    fn registering_adds_the_flac_encoder_only() {
        let mut registry = Registry::new();
        register(&mut registry);
        let reports = registry.capabilities();
        for codec in Codec::ALL {
            let report = reports.iter().find(|r| r.codec == codec).unwrap();
            if codec == CODEC {
                assert_eq!(report.encoders.len(), 1);
                assert_eq!(report.encoders[0].backend, BackendKind::Rust);
                assert!(report.encoders[0].available);
            } else {
                assert!(report.encoders.is_empty(), "{codec} must not register an encoder");
            }
        }
    }

    #[test]
    fn stream_splitting_extracts_streaminfo() {
        // A synthetic stream: "fLaC", STREAMINFO (34 bytes, last block), one byte.
        let mut stream = b"fLaC".to_vec();
        stream.push(0x80); // last block, type 0
        stream.extend_from_slice(&[0, 0, 34]);
        stream.extend_from_slice(&[0xAB; 34]);
        stream.push(0x42);
        let (streaminfo, frames) = split_flac_stream(&stream).unwrap();
        assert_eq!(streaminfo, vec![0xAB; 34]);
        assert_eq!(frames, vec![0x42]);
    }

    #[test]
    fn stream_splitting_rejects_a_bad_header() {
        assert!(split_flac_stream(b"nope").is_err());
        assert!(split_flac_stream(b"fLaC\x80\x00\x00\x22").is_err());
    }

    /// The end-to-end proof that the software tier works: encode with the
    /// pure-Rust FLAC encoder, then decode with the pure-Rust symphonia decoder
    /// and compare samples. No FFmpeg, no platform framework, no test asset.
    #[test]
    fn software_flac_round_trips_through_the_software_decoder() {
        let frames = 4096usize;
        let left = sine(frames, 441.0);
        let right = sine(frames, 330.0);
        let planar = vec![left.clone(), right.clone()];

        let mut encoder = FlacEncoder::new(&config()).unwrap();
        let frame =
            AudioFrame::from_f32_planar(44_100, ChannelLayout::Stereo, frames as u32, planar.clone(), 0)
                .unwrap();
        encoder.push(Frame::Audio(frame)).unwrap();

        let packets = encoder.flush().unwrap();
        assert_eq!(packets.len(), 2, "a header packet and a frame packet");
        assert!(packets[0].is_header(), "the first packet carries STREAMINFO");
        assert_eq!(packets[0].len(), 34);
        assert!(!packets[1].is_empty());

        let extradata = encoder.extradata().expect("extradata is exposed after flush");
        assert_eq!(extradata.as_slice(), packets[0].data.as_slice());

        let mut decoder = SymphoniaAudioDecoder::new(
            &DecoderConfig::new(CODEC)
                .with_extradata(extradata)
                .with_audio(44_100, ChannelLayout::Stereo),
        )
        .expect("the built-in decoder accepts the built-in encoder's STREAMINFO");

        decoder.push(packets[1].clone()).unwrap();
        let mut decoded = decoder.receive().unwrap().expect("one decoded frame");
        // FLAC is lossless and the block size may split the input, so drain the
        // remainder into one payload.
        let mut samples = interleaved_f32(decoded.as_audio().unwrap()).unwrap();
        while let Some(next) = decoder.receive().unwrap() {
            decoded = next;
            samples.extend(interleaved_f32(decoded.as_audio().unwrap()).unwrap());
        }
        assert_eq!(samples.len(), frames * 2, "every sample must come back");

        // 16-bit FLAC quantisation is the only loss; compare with a tolerance
        // slightly larger than one step of the 16-bit grid.
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
}
