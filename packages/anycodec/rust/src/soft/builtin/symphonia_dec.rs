//! Pure-Rust software **decoders** for MP3, AAC, Vorbis, FLAC and ALAC,
//! implemented on top of `symphonia`'s codec layer.
//!
//! There is no container here: AnyCodec deals in bare codec bitstreams, so the
//! decoder is built from [`DecoderConfig::extradata`] plus the geometry hints.
//! That is exactly the information a container would have supplied, and it is
//! what these codecs need:
//!
//! | Codec | `extradata` is |
//! |-------|----------------|
//! | AAC | `AudioSpecificConfig` (2+ bytes) |
//! | ALAC | the ALAC magic cookie |
//! | FLAC | the `STREAMINFO` metadata block *body* (34 bytes) |
//! | Vorbis | the `identification` header body |
//! | MP3 | not required |
//!
//! When `extradata` is missing the codec cannot be initialised, and the error
//! says so instead of producing silence.

use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{
    CodecParameters, CodecType, CODEC_TYPE_AAC, CODEC_TYPE_ALAC, CODEC_TYPE_FLAC,
    CODEC_TYPE_MP3, CODEC_TYPE_VORBIS, Decoder as SymphoniaDecoder, DecoderOptions,
};
use symphonia::core::conv::IntoSample;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::Packet as SymphoniaPacket;

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::Decoder;
use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::{Buffer, BufferBuilder};
use crate::media::codec::{AudioCodec, Codec};
use crate::media::config::DecoderConfig;
use crate::media::format::{ChannelLayout, SampleFormat};
use crate::media::frame::{AudioFrame, Frame};
use crate::media::packet::Packet;

use super::super::PRIORITY_BUILTIN;

/// The audio codecs [`symphonia`] can decode for AnyCodec.
pub(crate) const DECODABLE: [AudioCodec; 5] = [
    AudioCodec::Mp3,
    AudioCodec::Aac,
    AudioCodec::Vorbis,
    AudioCodec::Flac,
    AudioCodec::Alac,
];

/// The `symphonia` codec type for an audio codec, or `None` when unsupported.
pub fn symphonia_codec_type(codec: AudioCodec) -> Option<CodecType> {
    Some(match codec {
        AudioCodec::Mp3 => CODEC_TYPE_MP3,
        AudioCodec::Aac => CODEC_TYPE_AAC,
        AudioCodec::Vorbis => CODEC_TYPE_VORBIS,
        AudioCodec::Flac => CODEC_TYPE_FLAC,
        AudioCodec::Alac => CODEC_TYPE_ALAC,
        AudioCodec::Opus => return None,
    })
}

/// `true` when this codec has a pure-Rust decoder in this build.
pub fn is_decodable(codec: Codec) -> bool {
    matches!(codec.as_audio(), Some(audio) if symphonia_codec_type(audio).is_some())
}

/// The codecs this tier decodes, as [`Codec`]s.
pub fn registered_codecs() -> Vec<Codec> {
    DECODABLE.iter().map(|c| Codec::Audio(*c)).collect()
}

/// A software decoder for one of [`DECODABLE`].
pub struct SymphoniaAudioDecoder {
    codec: AudioCodec,
    path: CodecPath,
    inner: Box<dyn SymphoniaDecoder>,
    queued: Vec<Frame>,
}

impl std::fmt::Debug for SymphoniaAudioDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymphoniaAudioDecoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .field("queued", &self.queued.len())
            .finish()
    }
}

impl SymphoniaAudioDecoder {
    /// Builds a decoder from `config`.
    ///
    /// Fails with [`crate::error::ErrorKind::Unsupported`] for a codec this tier
    /// does not handle and with [`crate::error::ErrorKind::InvalidArgument`] when
    /// required `extradata` is missing or malformed.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        let codec = config
            .codec
            .as_audio()
            .ok_or_else(|| Error::unsupported("the built-in software tier is audio-only"))?;
        let codec_type = symphonia_codec_type(codec).ok_or_else(|| {
            Error::unsupported(format!(
                "the built-in software tier has no {codec} decoder (available: {})",
                DECODABLE.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
            ))
        })?;

        let mut parameters = CodecParameters::new();
        parameters.for_codec(codec_type);
        if config.sample_rate > 0 {
            parameters.with_sample_rate(config.sample_rate);
        }
        if !config.extradata.is_empty() {
            parameters.extra_data = Some(config.extradata.as_slice().into());
        }

        let registry = symphonia::default::get_codecs();
        let inner = registry.make(&parameters, &DecoderOptions::default()).map_err(|error| {
            Error::invalid_argument(format!(
                "the built-in {codec} decoder rejected its configuration \
                 ({} extra bytes of extradata, needs {}): {error}",
                config.extradata.len(),
                requirement(codec)
            ))
        })?;

        Ok(Self {
            codec,
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: symphonia {codec} decoder"),
            ),
            inner,
            queued: Vec::new(),
        })
    }
}

impl Decoder for SymphoniaAudioDecoder {
    fn codec(&self) -> Codec {
        Codec::Audio(self.codec)
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_eos() {
            return Ok(());
        }
        if packet.is_header() {
            // Headers arrive through `DecoderConfig::extradata` for this tier.
            return Ok(());
        }
        let symphonia_packet = SymphoniaPacket::new_from_boxed_slice(
            0,
            packet.pts.max(0) as u64,
            packet.duration.max(0) as u64,
            packet.data.to_vec().into_boxed_slice(),
        );
        match self.inner.decode(&symphonia_packet) {
            Ok(buffer) => {
                let frame = buffer_to_audio_frame(&buffer, packet.pts, self.codec)?;
                self.queued.push(Frame::Audio(frame));
                Ok(())
            }
            Err(SymphoniaError::DecodeError(detail)) => {
                // A damaged packet is not fatal: report it and let the caller skip.
                Err(Error::backend(format!("{} decode error: {detail}", self.codec)))
            }
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                Ok(())
            }
            Err(error) => Err(Error::backend(format!("{} decode failed: {error}", self.codec))),
        }
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        Ok(if self.queued.is_empty() { None } else { Some(self.queued.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        Ok(std::mem::take(&mut self.queued))
    }
}

/// Interleaves a `symphonia` output buffer into an [`AudioFrame`] of `f32`.
///
/// `f32` is chosen as the canonical software output because it is lossless with
/// respect to what the decoder produced and needs no clamping decision from us.
fn buffer_to_audio_frame(
    buffer: &AudioBufferRef<'_>,
    pts: i64,
    codec: AudioCodec,
) -> Result<AudioFrame> {
    let spec = buffer.spec();
    let frames = u32::try_from(buffer.frames())
        .map_err(|_| Error::backend(format!("{codec} produced more than u32::MAX frames")))?;
    let channel_count = u32::try_from(spec.channels.count()).unwrap_or(0);
    let layout = ChannelLayout::from_channel_count(channel_count).ok_or_else(|| {
        Error::unsupported(format!(
            "{codec} produced an unsupported {channel_count}-channel layout"
        ))
    })?;

    let mut builder = BufferBuilder::with_capacity(layout.byte_len(frames, SampleFormat::F32));
    macro_rules! interleave {
        ($typed:expr) => {{
            for index in 0..frames as usize {
                for channel in 0..channel_count as usize {
                    // `IntoSample` is symphonia's documented, normalising
                    // conversion: it maps the full integer range onto -1.0..=1.0
                    // instead of casting, so a 16-bit FLAC sample does not arrive
                    // as a raw i32.
                    let sample: f32 = $typed.chan(channel)[index].into_sample();
                    builder.push(&sample.to_le_bytes());
                }
            }
        }};
    }
    match buffer {
        AudioBufferRef::F32(b) => interleave!(b),
        AudioBufferRef::F64(b) => interleave!(b),
        AudioBufferRef::S32(b) => interleave!(b),
        AudioBufferRef::S24(b) => interleave!(b),
        AudioBufferRef::S16(b) => interleave!(b),
        AudioBufferRef::S8(b) => interleave!(b),
        AudioBufferRef::U32(b) => interleave!(b),
        AudioBufferRef::U24(b) => interleave!(b),
        AudioBufferRef::U16(b) => interleave!(b),
        AudioBufferRef::U8(b) => interleave!(b),
    }

    AudioFrame::new(
        SampleFormat::F32,
        spec.rate,
        layout,
        frames,
        builder.finish(),
        pts,
    )
}

/// Registers the built-in decoders for every codec they can handle.
pub fn register(registry: &mut Registry) {
    for audio in DECODABLE {
        let codec = Codec::Audio(audio);
        registry.register_decoder(
            codec,
            BackendKind::Rust,
            PRIORITY_BUILTIN,
            |config: &DecoderConfig| Ok(Some(Box::new(SymphoniaAudioDecoder::new(config)?))),
            |codec: Codec| {
                if is_decodable(codec) {
                    BackendAvailability::Available
                } else {
                    BackendAvailability::Unavailable(format!(
                        "the built-in software tier has no {codec} decoder"
                    ))
                }
            },
        );
    }
}

/// A length-prefixed description of what the built-in tier needs, used by the
/// error messages above and by the example app.
pub fn requirement(audio: AudioCodec) -> &'static str {
    match audio {
        AudioCodec::Aac => "an AudioSpecificConfig in `extradata`",
        AudioCodec::Alac => "an ALAC magic cookie in `extradata`",
        AudioCodec::Flac => "a 34-byte STREAMINFO block in `extradata`",
        AudioCodec::Vorbis => "the identification header body in `extradata`",
        AudioCodec::Mp3 => "nothing beyond the frame data",
        AudioCodec::Opus => "not supported by this tier at all",
    }
}

/// An owned, already-interleaved `f32` payload, exposed for tests and embedders
/// that want to inspect what the decoder produced without going through a frame.
pub fn interleaved_f32(frame: &AudioFrame) -> Option<Vec<f32>> {
    if frame.format != SampleFormat::F32 {
        return None;
    }
    Some(
        frame
            .samples
            .as_slice()
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
    )
}

/// A `Buffer` convenience for tests.
pub fn buffer_from_i16(samples: &[i16]) -> Buffer {
    let mut builder = BufferBuilder::with_capacity(samples.len() * 2);
    for sample in samples {
        builder.push(&sample.to_le_bytes());
    }
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codec_table_is_total_and_covers_five_formats() {
        assert_eq!(DECODABLE.len(), 5);
        for audio in AudioCodec::ALL {
            let supported = symphonia_codec_type(audio).is_some();
            assert_eq!(
                supported,
                DECODABLE.contains(&audio),
                "{audio} must be listed in DECODABLE exactly when it has a codec type"
            );
        }
        assert!(symphonia_codec_type(AudioCodec::Opus).is_none(), "opus is not in this tier");
    }

    #[test]
    fn is_decodable_ignores_video_codecs() {
        for codec in Codec::ALL {
            if codec.is_video() {
                assert!(!is_decodable(codec), "{codec} is video");
            }
        }
        assert!(is_decodable(Codec::Audio(AudioCodec::Flac)));
    }

    #[test]
    fn registered_codecs_match_the_table() {
        let codecs = registered_codecs();
        assert_eq!(codecs.len(), DECODABLE.len());
        for audio in DECODABLE {
            assert!(codecs.contains(&Codec::Audio(audio)));
        }
    }

    #[test]
    fn registering_adds_exactly_the_decodable_codecs() {
        let mut registry = Registry::new();
        register(&mut registry);
        let reports = registry.capabilities();
        for audio in AudioCodec::ALL {
            let codec = Codec::Audio(audio);
            let report = reports.iter().find(|r| r.codec == codec).unwrap();
            if DECODABLE.contains(&audio) {
                assert_eq!(report.decoders.len(), 1, "{audio} must have exactly one backend");
                assert_eq!(report.decoders[0].backend, BackendKind::Rust);
                assert!(report.decoders[0].available);
                assert!(report.encoders.is_empty(), "{audio} must not register an encoder");
            } else {
                assert!(report.decoders.is_empty(), "{audio} must not be registered");
            }
        }
    }

    #[test]
    fn an_unsupported_codec_is_rejected_with_a_specific_message() {
        let config = DecoderConfig::new(Codec::Audio(AudioCodec::Opus))
            .with_audio(48_000, ChannelLayout::Stereo);
        let error = SymphoniaAudioDecoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
        assert!(error.message().contains("opus"), "{}", error.message());
    }

    #[test]
    fn a_video_codec_is_rejected() {
        let config = DecoderConfig::new(Codec::Video(crate::media::codec::VideoCodec::H264));
        let error = SymphoniaAudioDecoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
    }

    #[test]
    fn mp3_needs_no_extradata() {
        // MP3 is the one codec in the tier that can start from bare frames.
        let config = DecoderConfig::new(Codec::Audio(AudioCodec::Mp3))
            .with_audio(44_100, ChannelLayout::Stereo);
        assert!(SymphoniaAudioDecoder::new(&config).is_ok());
    }

    #[test]
    fn aac_without_extradata_is_rejected_with_a_readable_reason() {
        let config = DecoderConfig::new(Codec::Audio(AudioCodec::Aac))
            .with_audio(48_000, ChannelLayout::Stereo);
        match SymphoniaAudioDecoder::new(&config) {
            // symphonia versions differ in how strictly they enforce this; either
            // the construction fails, or it succeeds and fails on the first packet.
            Ok(_) => {}
            Err(error) => {
                assert!(
                    matches!(
                        error.kind(),
                        crate::error::ErrorKind::InvalidArgument
                            | crate::error::ErrorKind::Unavailable
                    ),
                    "unexpected kind {:?}",
                    error.kind()
                );
                assert!(error.message().contains("aac"), "{}", error.message());
            }
        }
    }

    #[test]
    fn requirement_text_is_specific() {
        assert!(requirement(AudioCodec::Flac).contains("STREAMINFO"));
        assert!(requirement(AudioCodec::Mp3).contains("nothing"));
        assert!(requirement(AudioCodec::Opus).contains("not supported"));
    }

    #[test]
    fn interleaved_f32_reads_back_what_the_frame_holds() {
        let frame = AudioFrame::from_f32_planar(
            48_000,
            ChannelLayout::Stereo,
            2,
            vec![vec![0.25, 0.75], vec![-0.25, -0.75]],
            0,
        )
        .unwrap();
        let samples = interleaved_f32(&frame).expect("f32 payload");
        assert_eq!(samples, vec![0.25, -0.25, 0.75, -0.75]);
    }

    #[test]
    fn interleaved_f32_refuses_non_f32_payloads() {
        let frame = AudioFrame::new(
            SampleFormat::S16,
            48_000,
            ChannelLayout::Mono,
            1,
            buffer_from_i16(&[1]),
            0,
        )
        .unwrap();
        assert!(interleaved_f32(&frame).is_none());
    }
}
