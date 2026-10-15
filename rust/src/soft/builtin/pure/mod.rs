//! Tier 1b of the software fallback: **one real pure-Rust codec per family**.
//!
//! Where [`super::symphonia_dec`] is a single crate's worth of audio decoders
//! and [`super::flac_enc`] is a single encoder, this module is the full
//! software codec set: audio and video, decode *and* encode, each built on the
//! upstream crate that actually implements the format.
//!
//! Nothing here needs a C toolchain, cmake or nasm, so the whole tier is part of
//! every build on every target — there are no cargo features to switch it on
//! (see the note in `Cargo.toml`). Every codec registers at
//! [`PRIORITY_BUILTIN`](super::super::PRIORITY_BUILTIN), below every platform
//! framework.
//!
//! # The codec table
//!
//! [`CODECS`] is **total over all twelve codecs** [`Codec::ALL`] routes, and each
//! entry names the crate that serves each direction or `None` when this tier
//! cannot do it at all. That is what [`register`] registers and everything
//! [`decodable_codecs`] / [`encodable_codecs`] / [`summary`] report, so the
//! capability report cannot drift from the registrations.
//!
//! | Codec | Decode | Encode |
//! |-------|--------|--------|
//! | H.264 | — | — |
//! | H.265 | `rusty_h265` | — |
//! | H.266 | — | — |
//! | VP8 | — | — |
//! | VP9 | `rusty_vp9` | `rusty_vp9` |
//! | AV1 | — | — |
//! | MP3 | `rusty_mp3` | `rusty_mp3` |
//! | AAC | `rusty_aac` | `rusty_aac` |
//! | Vorbis | — | — |
//! | Opus | `opus-pure` | `opus-pure` |
//! | FLAC | `rusty_flac` | `rusty_flac` |
//! | ALAC | `alac` | — |
//!
//! H.264 has **no** entry in either direction on purpose. `rust_h264` — the only
//! pure-Rust H.264 decoder in the dependency set — builds its decoder, decoded
//! picture buffer, motion-vector predictor and slice contexts on `std::rc::Rc`,
//! so `rust_h264::decoder::Decoder` is `!Send`. [`crate::engine::Decoder`] is
//! `Send` (registry-created decoders are handed between threads and isolates),
//! and neither `unsafe` nor a per-decoder worker thread is a trade this tier is
//! willing to make for a path it cannot verify. The crate was therefore dropped
//! rather than registered as a capability that does not work.
//!
//! # Overlap with the other built-ins
//!
//! MP3, AAC, FLAC and ALAC already had a decoder here through
//! [`super::symphonia_dec`], and FLAC already had an encoder through
//! [`super::flac_enc`]. Those registrations are **kept**: they are working
//! codecs and the capability report is allowed to name more than one software
//! decoder for a codec. [`super::register`] registers this tier **first**, so
//! the registry tries these codecs before the symphonia ones and `flacenc`
//! remains exactly what it was already documented to be — the fallback FLAC
//! encoder.
//!
//! # Layouts
//!
//! * Audio in: whatever [`AudioFrame`] the caller pushes; it is converted to
//!   interleaved `f32` first (see [`interleaved_f32`]).
//! * Audio out: **interleaved [`SampleFormat::F32`]**, which is what every one
//!   of these crates produces natively and what [`super::symphonia_dec`] also
//!   produces, so a caller sees one audio layout whichever tier served it.
//! * Video out: 8-bit pictures become [`PixelFormat::I420`] and wider samples
//!   become [`PixelFormat::P010`], with each plane carrying its real stride.

use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::{Buffer, BufferBuilder};
use crate::media::codec::{AudioCodec, Codec, VideoCodec};
use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};
use crate::media::frame::{AudioFrame, Plane, VideoFrame};

pub mod aac;
pub mod alac;
pub mod flac;
pub mod h265;
pub mod mp3;
pub mod opus;
pub mod vp9;

/// Which direction a registration serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Producing compressed packets from raw frames.
    Encode,
    /// Producing raw frames from compressed packets.
    Decode,
}

impl Direction {
    /// Stable lower-case token, used in capability notes.
    pub const fn name(self) -> &'static str {
        match self {
            Direction::Encode => "encoder",
            Direction::Decode => "decoder",
        }
    }
}

/// One codec's support in this tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PureCodec {
    /// The codec.
    pub codec: Codec,
    /// The crate that decodes it, or `None` when this tier has no decoder.
    pub decoder: Option<&'static str>,
    /// The crate that encodes it, or `None` when this tier has no encoder.
    pub encoder: Option<&'static str>,
}

impl PureCodec {
    /// `true` when this tier can decode the codec.
    pub const fn is_decodable(&self) -> bool {
        self.decoder.is_some()
    }

    /// `true` when this tier can encode the codec.
    pub const fn is_encodable(&self) -> bool {
        self.encoder.is_some()
    }

    /// How to describe this entry in a `CodecPath::detail`.
    pub fn detail(&self, direction: Direction) -> String {
        let crate_name = match direction {
            Direction::Encode => self.encoder,
            Direction::Decode => self.decoder,
        };
        match crate_name {
            Some(name) => format!("rust: {name} {}", direction.name()),
            None => format!("rust: no {} for {}", direction.name(), self.codec),
        }
    }
}

/// The codec table: one entry per codec [`Codec::ALL`] routes, in that order.
///
/// A `None` is a statement that this tier genuinely cannot do it — never a
/// placeholder. Vorbis stays with [`super::symphonia_dec`]; H.264, H.266, VP8
/// and AV1 have no pure-Rust codec in this build at all.
pub(crate) const CODECS: [PureCodec; 12] = [
    PureCodec { codec: Codec::Video(VideoCodec::H264), decoder: None, encoder: None },
    PureCodec {
        codec: Codec::Video(VideoCodec::H265),
        decoder: Some("rusty_h265"),
        encoder: None,
    },
    PureCodec { codec: Codec::Video(VideoCodec::H266), decoder: None, encoder: None },
    PureCodec { codec: Codec::Video(VideoCodec::Vp8), decoder: None, encoder: None },
    PureCodec {
        codec: Codec::Video(VideoCodec::Vp9),
        decoder: Some("rusty_vp9"),
        encoder: Some("rusty_vp9"),
    },
    PureCodec { codec: Codec::Video(VideoCodec::Av1), decoder: None, encoder: None },
    PureCodec {
        codec: Codec::Audio(AudioCodec::Mp3),
        decoder: Some("rusty_mp3"),
        encoder: Some("rusty_mp3"),
    },
    PureCodec {
        codec: Codec::Audio(AudioCodec::Aac),
        decoder: Some("rusty_aac"),
        encoder: Some("rusty_aac"),
    },
    PureCodec { codec: Codec::Audio(AudioCodec::Vorbis), decoder: None, encoder: None },
    PureCodec {
        codec: Codec::Audio(AudioCodec::Opus),
        decoder: Some("opus-pure"),
        encoder: Some("opus-pure"),
    },
    PureCodec {
        codec: Codec::Audio(AudioCodec::Flac),
        decoder: Some("rusty_flac"),
        encoder: Some("rusty_flac"),
    },
    PureCodec {
        codec: Codec::Audio(AudioCodec::Alac),
        decoder: Some("alac"),
        encoder: None,
    },
];

/// The table entry for `codec`, or `None` when the codec is not even routed.
pub fn support(codec: Codec) -> Option<PureCodec> {
    CODECS.iter().copied().find(|entry| entry.codec == codec)
}

/// The crate that decodes `codec` in this tier, if any.
pub fn decoder_crate(codec: Codec) -> Option<&'static str> {
    support(codec).and_then(|entry| entry.decoder)
}

/// The crate that encodes `codec` in this tier, if any.
pub fn encoder_crate(codec: Codec) -> Option<&'static str> {
    support(codec).and_then(|entry| entry.encoder)
}

/// `true` when this tier has a real decoder for `codec`.
pub fn is_decodable(codec: Codec) -> bool {
    decoder_crate(codec).is_some()
}

/// `true` when this tier has a real encoder for `codec`.
pub fn is_encodable(codec: Codec) -> bool {
    encoder_crate(codec).is_some()
}

/// Every codec this tier decodes, in [`Codec::ALL`] order.
pub fn decodable_codecs() -> Vec<Codec> {
    CODECS.iter().filter(|entry| entry.is_decodable()).map(|entry| entry.codec).collect()
}

/// Every codec this tier encodes, in [`Codec::ALL`] order.
pub fn encodable_codecs() -> Vec<Codec> {
    CODECS.iter().filter(|entry| entry.is_encodable()).map(|entry| entry.codec).collect()
}

/// A one-line summary of what this tier covers.
pub fn summary() -> String {
    format!(
        "pure: decode[{}] encode[{}]",
        names(decodable_codecs()),
        names(encodable_codecs()),
    )
}

fn names(codecs: Vec<Codec>) -> String {
    codecs.iter().map(|codec| codec.name()).collect::<Vec<_>>().join(",")
}

/// Registers every codec this tier implements.
///
/// Each module registers exactly the codecs its entry in [`CODECS`] claims, at
/// [`PRIORITY_BUILTIN`](super::super::PRIORITY_BUILTIN), through a probe that
/// answers [`BackendAvailability::Available`] for that one codec and names the
/// crate when it is asked about any other.
pub fn register(registry: &mut Registry) {
    mp3::register(registry);
    aac::register(registry);
    flac::register(registry);
    opus::register(registry);
    vp9::register(registry);
    h265::register(registry);
    alac::register(registry);
}

/// The probe an honest registration exposes: `Available` for `codec` only.
///
/// The registry always probes an entry with the codec it was registered for, so
/// this is a guard against a future caller asking the wrong question rather than
/// a behaviour a caller can currently observe — which is exactly why it is worth
/// having.
pub(crate) fn probe(
    codec: Codec,
    crate_name: &'static str,
    direction: Direction,
) -> impl Fn(Codec) -> BackendAvailability + Send + Sync + 'static {
    move |asked: Codec| {
        if asked == codec {
            BackendAvailability::Available
        } else {
            BackendAvailability::Unavailable(format!(
                "the {crate_name} {} in this build serves {codec} only, not {asked}",
                direction.name()
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Shared conversions
// ---------------------------------------------------------------------------

/// Interleaves `samples` into an [`AudioFrame`] of [`SampleFormat::F32`].
///
/// `samples` is one channel-ordered sample per channel per frame; a short slice
/// is an error rather than a silent truncation.
pub(crate) fn audio_frame_from_interleaved_f32(
    sample_rate: u32,
    channels: ChannelLayout,
    frames: u32,
    samples: &[f32],
    pts: i64,
) -> Result<AudioFrame> {
    let expected = frames as usize * channels.channels() as usize;
    if samples.len() < expected {
        return Err(Error::invalid_argument(format!(
            "an interleaved {frames}-frame {} block needs {expected} samples, got {}",
            channels.name(),
            samples.len()
        )));
    }
    let mut builder = BufferBuilder::with_capacity(channels.byte_len(frames, SampleFormat::F32));
    for sample in &samples[..expected] {
        builder.push(&sample.to_le_bytes());
    }
    AudioFrame::new(
        SampleFormat::F32,
        sample_rate,
        channels,
        frames,
        builder.finish(),
        pts,
    )
}

/// Reads an [`AudioFrame`]'s interleaved payload as `f32`.
///
/// Every integer format is normalised onto `-1.0..=1.0`, so an encoder written
/// for `f32` input receives the same signal whatever the caller's sample format
/// was. The payload is taken as exactly `frames * channels` samples and no more,
/// so trailing padding cannot leak into the codec.
pub(crate) fn interleaved_f32(frame: &AudioFrame) -> Result<Vec<f32>> {
    let channels = frame.channels.channels() as usize;
    let count = frame.frames as usize * channels;
    let bytes = channels * frame.frames as usize * frame.format.bytes_per_sample();
    let payload = frame.samples.as_slice();
    if payload.len() < bytes {
        return Err(Error::invalid_argument(format!(
            "the {} frame claims {count} samples but carries only {} bytes",
            frame.format.name(),
            payload.len()
        )));
    }
    let payload = &payload[..bytes];
    let mut out = Vec::with_capacity(count);
    match frame.format {
        SampleFormat::F32 => {
            for chunk in payload.chunks_exact(4) {
                out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
        SampleFormat::S16 => {
            for chunk in payload.chunks_exact(2) {
                out.push(f32::from(i16::from_le_bytes([chunk[0], chunk[1]])) / 32_768.0);
            }
        }
        SampleFormat::S32 => {
            for chunk in payload.chunks_exact(4) {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out.push(value as f32 / 2_147_483_648.0);
            }
        }
        SampleFormat::U8 => {
            for byte in payload {
                out.push((f32::from(*byte) - 128.0) / 128.0);
            }
        }
    }
    Ok(out)
}

/// The channel layout for a plain channel count, with a named failure.
pub(crate) fn layout_for(codec: Codec, count: u32) -> Result<ChannelLayout> {
    ChannelLayout::from_channel_count(count).ok_or_else(|| {
        Error::unsupported(format!(
            "the {codec} output has {count} channels, which AnyCodec has no layout for"
        ))
    })
}

/// Builds an [`I420`](PixelFormat::I420) picture from tightly packed 8-bit
/// planes.
///
/// Each plane is one row of `w` bytes per row, no padding, and the lengths are
/// checked before the frame is built so a mismatched plane is an error rather
/// than a skewed picture.
pub(crate) fn i420_frame(
    width: u32,
    height: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    pts: i64,
) -> Result<VideoFrame> {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    expect_len("luma", y.len(), width as usize * height as usize)?;
    expect_len("chroma", u.len(), cw as usize * ch as usize)?;
    expect_len("chroma", v.len(), cw as usize * ch as usize)?;
    VideoFrame::new(
        PixelFormat::I420,
        width,
        height,
        vec![
            Plane::rows(Buffer::from_vec(y), width as usize)?,
            Plane::rows(Buffer::from_vec(u), cw as usize)?,
            Plane::rows(Buffer::from_vec(v), cw as usize)?,
        ],
        pts,
    )
}

/// Builds a [`P010`](PixelFormat::P010) picture from tightly packed
/// little-endian 16-bit planes.
pub(crate) fn p010_frame(
    width: u32,
    height: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    pts: i64,
) -> Result<VideoFrame> {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    expect_len("luma", y.len(), width as usize * height as usize * 2)?;
    expect_len("chroma", u.len(), cw as usize * ch as usize * 2)?;
    expect_len("chroma", v.len(), cw as usize * ch as usize * 2)?;
    VideoFrame::new(
        PixelFormat::P010,
        width,
        height,
        vec![
            Plane::rows(Buffer::from_vec(y), width as usize * 2)?,
            Plane::rows(Buffer::from_vec(u), cw as usize * 2)?,
            Plane::rows(Buffer::from_vec(v), cw as usize * 2)?,
        ],
        pts,
    )
}

fn expect_len(what: &str, actual: usize, expected: usize) -> Result<()> {
    if actual != expected {
        return Err(Error::backend(format!(
            "the decoder produced a {what} plane of {actual} bytes, expected {expected}"
        )));
    }
    Ok(())
}

/// Signal helpers shared by the codec round-trip tests.
///
/// A round trip through a *lossy* codec is not an equality, and the codec delay
/// is a property of the crate rather than something this tier should hard-code
/// into every test, so the comparison is "the smallest RMS error over every
/// plausible delay". That is a real check of the waveform, not of the length.
#[cfg(test)]
pub(crate) mod testing {
    /// A `f32` sine of `frames` samples at `hz`, at half scale.
    pub fn sine(frames: usize, hz: f32, sample_rate: f32) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                (index as f32 * hz * 2.0 * std::f32::consts::PI / sample_rate).sin() * 0.5
            })
            .collect()
    }

    /// Interleaves `left` and `right` into `channels` channels, zero-filling
    /// any channel past the second.
    pub fn interleave(left: &[f32], right: &[f32], channels: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(left.len() * channels);
        for index in 0..left.len() {
            out.push(left[index]);
            if channels > 1 {
                out.push(right[index]);
                // Channels past the second are silence, not a copy.
                out.resize(out.len() + channels - 2, 0.0);
            }
        }
        out
    }

    /// The smallest RMS error between `input` and `output` over every lag in
    /// `0..=max_lag`, i.e. the error once the codec's own delay is allowed for.
    pub fn best_rms(input: &[f32], output: &[f32], max_lag: usize) -> f32 {
        let mut best = f32::MAX;
        for lag in 0..=max_lag {
            if lag >= output.len() {
                break;
            }
            let length = input.len().min(output.len() - lag);
            if length * 2 < input.len() {
                break;
            }
            let mut sum = 0.0f64;
            for index in 0..length {
                let delta = f64::from(input[index] - output[lag + index]);
                sum += delta * delta;
            }
            let rms = (sum / length as f64).sqrt() as f32;
            if rms < best {
                best = rms;
            }
        }
        best
    }

    /// The mean absolute difference between two same-length byte planes.
    pub fn mean_abs_error(expected: &[u8], actual: &[u8]) -> f32 {
        let length = expected.len().min(actual.len());
        if length == 0 {
            return f32::MAX;
        }
        let mut sum = 0f64;
        for index in 0..length {
            sum += f64::from(expected[index].abs_diff(actual[index]));
        }
        (sum / length as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BackendKind, BackendPreference};
    use crate::media::codec::VideoCodec;
    use crate::media::config::{DecoderConfig, EncoderConfig};

    /// Codecs a *second* built-in implementation also serves, which is why the
    /// built-in tier reports two `rust` backends for each of them.
    const SYMPHONIA_DECODED: [AudioCodec; 5] = [
        AudioCodec::Mp3,
        AudioCodec::Aac,
        AudioCodec::Vorbis,
        AudioCodec::Flac,
        AudioCodec::Alac,
    ];
    /// The `flacenc` encoder, which sits behind `rusty_flac`.
    const FLACENC_ENCODED: [AudioCodec; 1] = [AudioCodec::Flac];

    /// A 34-byte `STREAMINFO` body with the given geometry.
    fn streaminfo(rate: u32, channels: u32, bits: u32) -> Vec<u8> {
        let mut body = vec![0u8; 34];
        body[10] = (rate >> 12) as u8;
        body[11] = (rate >> 4) as u8;
        body[12] = (((rate & 0x0F) as u8) << 4)
            | (((channels - 1) as u8) << 1)
            | (((bits - 1) >> 4) as u8);
        body[13] = (((bits - 1) & 0x0F) as u8) << 4;
        body
    }

    /// A decoder configuration this tier accepts for `codec`, or `None` when it
    /// has no decoder for it.
    fn decoder_config(codec: Codec) -> Option<DecoderConfig> {
        if !is_decodable(codec) {
            return None;
        }
        let config = match codec {
            Codec::Audio(AudioCodec::Mp3) => {
                DecoderConfig::new(codec).with_audio(44_100, ChannelLayout::Stereo)
            }
            Codec::Audio(AudioCodec::Aac) => {
                DecoderConfig::new(codec).with_audio(44_100, ChannelLayout::Stereo)
            }
            Codec::Audio(AudioCodec::Opus) => {
                DecoderConfig::new(codec).with_audio(48_000, ChannelLayout::Stereo)
            }
            Codec::Audio(AudioCodec::Flac) => DecoderConfig::new(codec)
                .with_extradata(Buffer::from_vec(streaminfo(44_100, 2, 16)))
                .with_audio(44_100, ChannelLayout::Stereo),
            Codec::Audio(AudioCodec::Alac) => DecoderConfig::new(codec)
                .with_extradata(Buffer::from_vec(crate::soft::builtin::pure::alac::cookie(
                    4_096, 16, 2, 44_100,
                )))
                .with_audio(44_100, ChannelLayout::Stereo),
            Codec::Video(VideoCodec::Vp9) => DecoderConfig::new(codec).with_size(32, 32),
            Codec::Video(VideoCodec::H265) => DecoderConfig::new(codec).with_size(32, 32),
            _ => return None,
        };
        Some(config)
    }

    /// An encoder configuration this tier accepts for `codec`, or `None`.
    fn encoder_config(codec: Codec) -> Option<EncoderConfig> {
        if !is_encodable(codec) {
            return None;
        }
        Some(match codec {
            Codec::Video(VideoCodec::Vp9) => {
                EncoderConfig::video(VideoCodec::Vp9, 32, 32).with_framerate(30, 1)
            }
            Codec::Audio(audio) => EncoderConfig::audio(
                audio,
                match audio {
                    AudioCodec::Opus => 48_000,
                    _ => 44_100,
                },
                ChannelLayout::Stereo,
            ),
            _ => return None,
        })
    }

    #[test]
    fn the_codec_table_is_total_over_all_twelve_codecs() {
        assert_eq!(CODECS.len(), Codec::ALL.len());
        for (entry, codec) in CODECS.iter().zip(Codec::ALL) {
            assert_eq!(entry.codec, codec, "the table must follow Codec::ALL order");
        }
        for codec in Codec::ALL {
            let entry = support(codec).expect("every codec must have a table entry");
            assert_eq!(entry.is_decodable(), is_decodable(codec), "{codec}");
            assert_eq!(entry.is_encodable(), is_encodable(codec), "{codec}");
            assert_eq!(entry.decoder, decoder_crate(codec), "{codec}");
            assert_eq!(entry.encoder, encoder_crate(codec), "{codec}");
        }
    }

    #[test]
    fn the_table_claims_exactly_the_crates_this_build_wires() {
        // Each claim is a real registration; nothing is claimed and left out.
        let mut registry = Registry::new();
        register(&mut registry);
        let reports = registry.capabilities();
        for entry in CODECS {
            let report = reports
                .iter()
                .find(|report| report.codec == entry.codec)
                .expect("every codec is reported");
            assert_eq!(report.decoders.len(), usize::from(entry.is_decodable()), "{}", entry.codec);
            assert_eq!(report.encoders.len(), usize::from(entry.is_encodable()), "{}", entry.codec);
            for backend in report.decoders.iter().chain(report.encoders.iter()) {
                assert_eq!(backend.backend, BackendKind::Rust);
                assert!(backend.available, "{} must be available", entry.codec);
            }
        }
        // The four codecs no pure-Rust crate here implements. H.264 is the
        // interesting one: the crate exists, but it is `!Send`.
        for codec in [
            Codec::Video(VideoCodec::H264),
            Codec::Video(VideoCodec::H266),
            Codec::Video(VideoCodec::Vp8),
            Codec::Video(VideoCodec::Av1),
        ] {
            let report = reports.iter().find(|report| report.codec == codec).unwrap();
            assert!(report.decoders.is_empty(), "{codec}");
            assert!(report.encoders.is_empty(), "{codec}");
        }
    }

    #[test]
    fn the_table_and_the_accessors_agree_on_directions() {
        assert_eq!(decodable_codecs().len(), CODECS.iter().filter(|e| e.is_decodable()).count());
        assert_eq!(encodable_codecs().len(), CODECS.iter().filter(|e| e.is_encodable()).count());
        // A codec can be claimed in one direction and not the other; check each
        // direction independently, including the crates that have no encoder.
        assert!(!is_decodable(Codec::Video(VideoCodec::H264)));
        assert!(!is_encodable(Codec::Video(VideoCodec::H264)));
        assert!(is_decodable(Codec::Video(VideoCodec::H265)));
        assert!(!is_encodable(Codec::Video(VideoCodec::H265)));
        assert!(is_decodable(Codec::Audio(AudioCodec::Alac)));
        assert!(!is_encodable(Codec::Audio(AudioCodec::Alac)));
        assert!(!is_decodable(Codec::Audio(AudioCodec::Vorbis)));
        assert!(is_decodable(Codec::Video(VideoCodec::Vp9)));
        assert!(is_encodable(Codec::Video(VideoCodec::Vp9)));
    }

    #[test]
    fn the_summary_names_every_claimed_codec() {
        let summary = summary();
        for codec in decodable_codecs().iter().chain(encodable_codecs().iter()) {
            assert!(summary.contains(codec.name()), "{codec} missing from {summary}");
        }
    }

    /// Every backend this tier registers is constructible, reports the software
    /// fallback, and names its crate in the path detail.
    #[test]
    fn every_registered_backend_reports_a_software_path_naming_its_crate() {
        let mut registry = Registry::new();
        register(&mut registry);
        for entry in CODECS {
            if let Some(config) = decoder_config(entry.codec) {
                let decoder = registry
                    .create_decoder(&config, BackendPreference::SoftwareOnly)
                    .unwrap_or_else(|error| panic!("{} decoder: {error}", entry.codec));
                let path = decoder.path();
                assert_eq!(path.backend, BackendKind::Rust, "{}", entry.codec);
                assert!(path.is_software(), "{}", entry.codec);
                assert!(!path.hardware_accelerated, "{}", entry.codec);
                assert!(path.detail.starts_with("rust: "), "{}", path.detail);
                assert!(path.detail.contains(entry.decoder.unwrap()), "{}", path.detail);
                assert_eq!(path.detail, entry.detail(Direction::Decode), "{}", entry.codec);
            }
            if let Some(config) = encoder_config(entry.codec) {
                let encoder = registry
                    .create_encoder(&config, BackendPreference::SoftwareOnly)
                    .unwrap_or_else(|error| panic!("{} encoder: {error}", entry.codec));
                let path = encoder.path();
                assert_eq!(path.backend, BackendKind::Rust, "{}", entry.codec);
                assert!(path.is_software(), "{}", entry.codec);
                assert!(!path.hardware_accelerated, "{}", entry.codec);
                assert!(path.detail.starts_with("rust: "), "{}", path.detail);
                // The encoder paths may carry extra detail (the FLAC one names
                // its bit depth), so containment is the assertion that matters.
                assert!(path.detail.contains(entry.encoder.unwrap()), "{}", path.detail);
            }
        }
    }

    /// With the whole built-in tier registered, the pure codecs are the ones the
    /// registry actually picks — the older implementations really are fallbacks.
    #[test]
    fn the_pure_codecs_win_over_the_older_built_ins() {
        let mut registry = Registry::new();
        crate::soft::builtin::register(&mut registry);
        for entry in CODECS {
            if let Some(config) = decoder_config(entry.codec) {
                let decoder = registry
                    .create_decoder(&config, BackendPreference::SoftwareOnly)
                    .unwrap_or_else(|error| panic!("{} decoder: {error}", entry.codec));
                assert!(
                    decoder.path().detail.contains(entry.decoder.unwrap()),
                    "{} was served by {} instead of {}",
                    entry.codec,
                    decoder.path().detail,
                    entry.decoder.unwrap()
                );
            }
            if let Some(config) = encoder_config(entry.codec) {
                let encoder = registry
                    .create_encoder(&config, BackendPreference::SoftwareOnly)
                    .unwrap_or_else(|error| panic!("{} encoder: {error}", entry.codec));
                assert!(
                    encoder.path().detail.contains(entry.encoder.unwrap()),
                    "{} was served by {} instead of {}",
                    entry.codec,
                    encoder.path().detail,
                    entry.encoder.unwrap()
                );
            }
        }
    }

    /// `super::super::register` adds exactly the expected backends for the
    /// expected codecs, and nothing at all for the rest.
    #[test]
    fn the_built_in_tier_registers_exactly_the_expected_backends() {
        let mut registry = Registry::new();
        crate::soft::builtin::register(&mut registry);
        let reports = registry.capabilities();
        for codec in Codec::ALL {
            let report = reports.iter().find(|report| report.codec == codec).unwrap();
            let audio = codec.as_audio();
            let second_decoder = audio.is_some_and(|a| SYMPHONIA_DECODED.contains(&a));
            let second_encoder = audio.is_some_and(|a| FLACENC_ENCODED.contains(&a));
            let expected_decoders = usize::from(is_decodable(codec)) + usize::from(second_decoder);
            let expected_encoders = usize::from(is_encodable(codec)) + usize::from(second_encoder);
            assert_eq!(report.decoders.len(), expected_decoders, "{codec} decoders");
            assert_eq!(report.encoders.len(), expected_encoders, "{codec} encoders");
            for backend in report.decoders.iter().chain(report.encoders.iter()) {
                assert_eq!(backend.backend, BackendKind::Rust, "{codec}");
                assert!(backend.available, "{codec} backend must be available");
                assert!(!backend.hardware, "{codec} is a software codec");
            }
        }
    }

    #[test]
    fn an_unsupported_codec_is_rejected_by_name_and_kind() {
        // `Box<dyn Decoder>` is not `Debug`, so `expect_err` cannot be used here.
        fn failure<T>(result: crate::error::Result<T>, what: &str) -> crate::error::Error {
            match result {
                Ok(_) => panic!("{what} must not be served by this build"),
                Err(error) => error,
            }
        }

        let mut registry = Registry::new();
        crate::soft::builtin::register(&mut registry);
        for codec in [Codec::Video(VideoCodec::H264), Codec::Video(VideoCodec::H266)] {
            let error = failure(
                registry.create_decoder(
                    &DecoderConfig::new(codec),
                    BackendPreference::SoftwareOnly,
                ),
                codec.name(),
            );
            assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
            assert!(error.message().contains(codec.name()), "{}", error.message());
        }

        let error = failure(
            registry.create_encoder(
                &EncoderConfig::video(VideoCodec::Vp8, 32, 32),
                BackendPreference::SoftwareOnly,
            ),
            "vp8",
        );
        assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
        assert!(error.message().contains("vp8"), "{}", error.message());
    }

    #[test]
    fn the_probe_answers_only_for_the_codec_it_serves() {
        let answer = probe(Codec::Audio(AudioCodec::Mp3), "rusty_mp3", Direction::Decode);
        assert!(answer(Codec::Audio(AudioCodec::Mp3)).is_available());
        let note = answer(Codec::Audio(AudioCodec::Opus));
        assert!(!note.is_available());
        let reason = note.reason().unwrap();
        assert!(reason.contains("rusty_mp3"), "{reason}");
        assert!(reason.contains("mp3"), "{reason}");
        assert!(reason.contains("opus"), "{reason}");
    }

    #[test]
    fn the_shared_audio_conversions_are_exact() {
        let frame =
            AudioFrame::from_f32_planar(48_000, ChannelLayout::Mono, 2, vec![vec![0.5, -0.5]], 9)
                .unwrap();
        assert_eq!(interleaved_f32(&frame).unwrap(), vec![0.5, -0.5]);

        let s16 = AudioFrame::new(
            SampleFormat::S16,
            48_000,
            ChannelLayout::Mono,
            2,
            Buffer::from_slice(&[0x00, 0x40, 0x00, 0xC0]),
            0,
        )
        .unwrap();
        assert_eq!(interleaved_f32(&s16).unwrap(), vec![0.5, -0.5]);

        let built =
            audio_frame_from_interleaved_f32(48_000, ChannelLayout::Stereo, 1, &[0.25, -0.25], 3)
                .unwrap();
        assert_eq!(built.format, SampleFormat::F32);
        assert_eq!(built.frames, 1);
        assert_eq!(built.pts, 3);
        assert_eq!(
            built.samples.as_slice(),
            [0.25f32.to_le_bytes(), (-0.25f32).to_le_bytes()].concat().as_slice()
        );
    }

    #[test]
    fn a_short_payload_is_an_error_not_a_silent_truncation() {
        let frame =
            AudioFrame::from_f32_planar(48_000, ChannelLayout::Mono, 4, vec![vec![0.0; 4]], 0)
                .unwrap();
        let error =
            audio_frame_from_interleaved_f32(48_000, ChannelLayout::Mono, 8, &[0.0; 4], 0)
                .unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(interleaved_f32(&frame).is_ok());
    }

    #[test]
    fn the_video_frame_helpers_reject_wrongly_sized_planes() {
        let error = i420_frame(4, 4, vec![0; 15], vec![0; 4], vec![0; 4], 0).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Backend);
        let error = p010_frame(4, 4, vec![0; 32], vec![0; 8], vec![0; 7], 0).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Backend);
        // And the well-formed case carries a real stride.
        let frame = i420_frame(4, 4, vec![0; 16], vec![1; 4], vec![2; 4], 0).unwrap();
        assert_eq!(frame.format, PixelFormat::I420);
        assert_eq!(frame.planes[0].stride(), 4);
        assert_eq!(frame.planes[1].stride(), 2);
    }

    #[test]
    fn an_unknown_channel_count_is_named() {
        let error = layout_for(Codec::Audio(AudioCodec::Mp3), 5).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
        assert!(error.message().contains('5'), "{}", error.message());
    }

    #[test]
    fn the_direction_tokens_are_stable() {
        assert_eq!(Direction::Encode.name(), "encoder");
        assert_eq!(Direction::Decode.name(), "decoder");
        let entry = support(Codec::Audio(AudioCodec::Mp3)).unwrap();
        assert_eq!(entry.detail(Direction::Encode), "rust: rusty_mp3 encoder");
        assert_eq!(entry.detail(Direction::Decode), "rust: rusty_mp3 decoder");
    }
}
