//! Apple platform backend.
//!
//! Two frameworks, one module per framework, exactly as [`crate::native`]'s
//! contract requires:
//!
//! | Framework | Module | Drives |
//! |-----------|--------|--------|
//! | VideoToolbox | [`video_toolbox`] | `VTCompressionSession` / `VTDecompressionSession` |
//! | AudioToolbox | [`audio_toolbox`] | `AudioConverter` |
//!
//! Everything the two share lives here: the `Codec` → framework-identifier
//! tables, the `OSStatus` decoder, and the `CMTime` timebase.
//!
//! ## What this backend is built on
//!
//! The bindings come from the `objc2` family of crates (`objc2-core-foundation`,
//! `objc2-core-media`, `objc2-core-video`, `objc2-video-toolbox`,
//! `objc2-audio-toolbox`). This module declares **no** `extern` block of its own,
//! and it does not use `oxideav-videotoolbox` / `oxideav-audiotoolbox` or any
//! other OxideAV crate.
//!
//! ## Hardware acceleration, honestly
//!
//! * **Encoding.** `VTCompressionSessionCreate` may quietly pick a software
//!   encoder. The only truthful answer is the one the live session gives:
//!   [`video_toolbox`] reads
//!   `kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder` back off
//!   the session and reports exactly what it says — including "the encoder did
//!   not answer", which is reported as `false` with the reason in
//!   `CodecPath::detail`.
//! * **Decoding.** There is no per-session equivalent, so the system-wide
//!   `VTIsHardwareDecodeSupported` is used and named as the source in `detail`.
//! * **Audio.** AudioToolbox's `AudioConverter` is a CPU DSP path. Every audio
//!   session therefore reports `hardware_accelerated == false` and says so.
//!
//! ## Timebase
//!
//! `Packet::pts` / `Frame::pts` are raw integers in a caller-chosen timebase,
//! while CoreMedia wants a rational [`CMTime`]. Every `CMTime` this backend
//! creates uses [`TIME_SCALE`] as its timescale, so the mapping
//! `milliseconds → CMTime → milliseconds` round-trips exactly. VideoToolbox
//! hands the presentation timestamp back on the decoded sample buffer, so an
//! invertible mapping is not a nicety — it is how a decoded frame keeps its
//! timestamp.
//!
//! [`CMTime`]: objc2_core_media::CMTime

pub mod audio_toolbox;
pub mod pixel_buffer;
pub mod video_toolbox;

use objc2_core_audio_types::AudioFormatID;
use objc2_core_media::{CMTime, CMVideoCodecType};

use crate::engine::{HwDeviceInfo, Registry};
use crate::media::codec::{AudioCodec, Codec, VideoCodec};

/// The timescale every `CMTime` in this backend is expressed in.
///
/// Milliseconds: `Frame::pts` and `Packet::pts` are treated as milliseconds, and
/// a timestamp read back off a decoded `CMSampleBuffer` becomes milliseconds
/// again. See the module documentation.
pub(crate) const TIME_SCALE: i32 = 1_000;

/// Wraps a millisecond timestamp as a CoreMedia time.
///
/// Never fails: `CMTimeMake` is a pure constructor and cannot report an error.
#[must_use]
pub fn time_from_ms(milliseconds: i64) -> CMTime {
    // SAFETY: `CMTimeMake` takes two integers by value, touches no memory, and
    // has no documented failure mode for a positive timescale.
    unsafe { CMTime::new(milliseconds, TIME_SCALE) }
}

/// Converts a CoreMedia time back into this backend's millisecond timebase.
///
/// A time that is not numeric (invalid, indefinite or infinite) becomes `0`,
/// which is the only thing an unknown timestamp can honestly become.
#[must_use]
pub fn ms_from_time(time: CMTime) -> i64 {
    // SAFETY: `CMTimeGetSeconds` reads the struct it is handed, by value.
    let seconds = unsafe { time.seconds() };
    if seconds.is_finite() {
        (seconds * f64::from(TIME_SCALE)).round() as i64
    } else {
        0
    }
}

/// Renders an error code the way Apple's headers document it: a signed
/// `OSStatus` whose four bytes are usually a four character code.
///
/// Bytes that are not printable ASCII become `.`, so the result is always
/// exactly four characters long and can never inject a control character into a
/// log line.
#[must_use]
pub fn four_char_code(value: i32) -> String {
    let bytes = (value as u32).to_be_bytes();
    bytes
        .iter()
        .map(|byte| if byte.is_ascii_graphic() || *byte == b' ' { *byte as char } else { '.' })
        .collect()
}

/// Turns an `OSStatus` into a specific, readable sentence.
///
/// VideoToolbox, AudioToolbox and CoreVideo all report failures as `OSStatus`
/// values: some are named constants in the headers, the rest are four character
/// codes (`'!pri'`, `'fmt?'`, ...). A generic "the call failed" is useless in a
/// bug report, so known codes get named text and everything else keeps its
/// numeric value *and* its four character rendering.
///
/// Codes whose names would collide (`kAudioFormatUnsupportedPropertyError` and
/// `kAudioConverterErr_PropertyNotSupported` are both `'prop'`) are named once,
/// under the AudioFormat name, because that is the family a caller can act on.
#[must_use]
pub fn os_status_text(status: i32) -> String {
    let named = match status {
        0 => Some("no error"),

        // ---- VideoToolbox ----
        objc2_video_toolbox::kVTPropertyNotSupportedErr => {
            Some("VideoToolbox does not support this property")
        }
        objc2_video_toolbox::kVTPropertyReadOnlyErr => Some("the VideoToolbox property is read-only"),
        objc2_video_toolbox::kVTParameterErr => Some("VideoToolbox rejected a parameter"),
        objc2_video_toolbox::kVTInvalidSessionErr => {
            Some("the VideoToolbox session is invalid: it was invalidated, or the codec changed")
        }
        objc2_video_toolbox::kVTAllocationFailedErr => {
            Some("VideoToolbox could not allocate the memory it needs")
        }
        objc2_video_toolbox::kVTCouldNotFindVideoDecoderErr => {
            Some("VideoToolbox found no decoder for this codec type on this machine")
        }
        objc2_video_toolbox::kVTCouldNotCreateInstanceErr => {
            Some("VideoToolbox could not instantiate the codec")
        }
        objc2_video_toolbox::kVTCouldNotFindVideoEncoderErr => {
            Some("VideoToolbox found no encoder for this codec type on this machine")
        }
        objc2_video_toolbox::kVTVideoDecoderBadDataErr => {
            Some("the VideoToolbox decoder rejected the bitstream as malformed")
        }
        objc2_video_toolbox::kVTVideoDecoderUnsupportedDataFormatErr => {
            Some("the VideoToolbox decoder does not support this data format")
        }
        objc2_video_toolbox::kVTVideoDecoderMalfunctionErr => {
            Some("the VideoToolbox decoder malfunctioned")
        }
        objc2_video_toolbox::kVTVideoEncoderMalfunctionErr => {
            Some("the VideoToolbox encoder malfunctioned")
        }
        objc2_video_toolbox::kVTVideoDecoderNotAvailableNowErr => {
            Some("the VideoToolbox decoder is not available right now")
        }
        objc2_video_toolbox::kVTVideoEncoderNotAvailableNowErr => {
            Some("the VideoToolbox encoder is not available right now")
        }
        objc2_video_toolbox::kVTFormatDescriptionChangeNotSupportedErr => {
            Some("the decoder cannot follow this format description change")
        }
        objc2_video_toolbox::kVTVideoDecoderAuthorizationErr => {
            Some("this process is not authorized to use the VideoToolbox decoder")
        }
        objc2_video_toolbox::kVTVideoEncoderAuthorizationErr => {
            Some("this process is not authorized to use the VideoToolbox encoder")
        }
        objc2_video_toolbox::kVTVideoDecoderRemovedErr => {
            Some("the VideoToolbox decoder was removed from the system")
        }
        objc2_video_toolbox::kVTSessionMalfunctionErr => {
            Some("the VideoToolbox session malfunctioned")
        }
        objc2_video_toolbox::kVTVideoDecoderReferenceMissingErr => {
            Some("a reference frame the decoder needs is missing")
        }
        objc2_video_toolbox::kVTVideoDecoderCallbackMessagingErr => {
            Some("the decoder could not deliver a frame to its output callback")
        }
        objc2_video_toolbox::kVTExtensionDisabledErr => {
            Some("a VideoToolbox extension this codec needs is disabled")
        }

        // ---- AudioToolbox: the AudioFormat / AudioConverter error families ----
        objc2_audio_toolbox::kAudioFormatUnsupportedDataFormatError => {
            Some("AudioToolbox does not support this data format")
        }
        objc2_audio_toolbox::kAudioFormatUnknownFormatError => {
            Some("AudioToolbox does not know this audio format")
        }
        objc2_audio_toolbox::kAudioFormatUnsupportedPropertyError => {
            Some("AudioToolbox does not support this property")
        }
        objc2_audio_toolbox::kAudioFormatBadPropertySizeError => {
            Some("AudioToolbox was handed a property buffer of the wrong size")
        }
        objc2_audio_toolbox::kAudioFormatBadSpecifierSizeError => {
            Some("AudioToolbox was handed a specifier of the wrong size")
        }
        objc2_audio_toolbox::kAudioFormatUnspecifiedError => {
            Some("AudioToolbox reported an unspecified failure")
        }
        objc2_audio_toolbox::kAudioConverterErr_OperationNotSupported => {
            Some("the AudioConverter does not support this operation")
        }
        objc2_audio_toolbox::kAudioConverterErr_InvalidInputSize => {
            Some("the AudioConverter was handed an invalid input size")
        }
        objc2_audio_toolbox::kAudioConverterErr_InvalidOutputSize => {
            Some("the AudioConverter was handed an invalid output size")
        }
        objc2_audio_toolbox::kAudioConverterErr_RequiresPacketDescriptionsError => {
            Some("the AudioConverter needs packet descriptions for this format")
        }
        objc2_audio_toolbox::kAudioConverterErr_InputSampleRateOutOfRange => {
            Some("the AudioConverter cannot work at this input sample rate")
        }
        objc2_audio_toolbox::kAudioConverterErr_OutputSampleRateOutOfRange => {
            Some("the AudioConverter cannot work at this output sample rate")
        }

        // ---- CoreVideo ----
        objc2_core_video::kCVReturnInvalidPixelFormat => {
            Some("CoreVideo does not support this pixel format")
        }
        objc2_core_video::kCVReturnInvalidSize => Some("CoreVideo rejected this buffer size"),
        objc2_core_video::kCVReturnAllocationFailed => {
            Some("CoreVideo could not allocate the pixel buffer")
        }
        objc2_core_video::kCVReturnInvalidPixelBufferAttributes => {
            Some("CoreVideo rejected the pixel buffer attributes")
        }

        // ---- Carbon-era codes every Core* framework still returns ----
        -50 => Some("invalid parameter"),
        -4 => Some("unimplemented"),
        _ => None,
    };
    match named {
        Some(text) => text.to_string(),
        None => {
            // Only claim a four character rendering when the code really is four
            // printable bytes; a negative Core* code is mostly `0xff` and would
            // otherwise be rendered as a meaningless `'....'`.
            let bytes = (status as u32).to_be_bytes();
            if bytes.iter().all(u8::is_ascii_graphic) {
                format!("OSStatus {status} ('{}')", four_char_code(status))
            } else {
                format!("OSStatus {status}")
            }
        }
    }
}

/// The `CMVideoCodecType` of ITU-T H.266 / VVC.
///
/// CoreMedia never declared a constant for VVC in the SDK these bindings were
/// generated from (`objc2-core-media` 0.3), so the registered ISO/IEC 14496-15
/// sample-entry four character code `'vvc1'` is spelled out here.
pub(crate) const CM_VIDEO_CODEC_TYPE_VVC: CMVideoCodecType = 0x7676_6331;

/// The CoreMedia codec identifier for `codec`, or `None` when CoreMedia has no
/// identifier for it at all.
///
/// Total over all twelve [`Codec`] values, in the sense that every input gets an
/// answer: VP8 and every audio codec have no `CMVideoCodecType`, and `None` is
/// the honest answer rather than an invented identifier.
#[must_use]
pub fn video_codec_type(codec: Codec) -> Option<CMVideoCodecType> {
    Some(match codec {
        Codec::Video(VideoCodec::H264) => objc2_core_media::kCMVideoCodecType_H264,
        Codec::Video(VideoCodec::H265) => objc2_core_media::kCMVideoCodecType_HEVC,
        Codec::Video(VideoCodec::H266) => CM_VIDEO_CODEC_TYPE_VVC,
        // CoreMedia declared no VP8 identifier, and VideoToolbox has no VP8 codec.
        Codec::Video(VideoCodec::Vp8) => return None,
        Codec::Video(VideoCodec::Vp9) => objc2_core_media::kCMVideoCodecType_VP9,
        Codec::Video(VideoCodec::Av1) => objc2_core_media::kCMVideoCodecType_AV1,
        // Audio is AudioToolbox's job; see `audio_format_id`.
        Codec::Audio(_) => return None,
    })
}

/// The `AudioFormatID` for `codec`, or `None` when CoreAudioTypes has no
/// identifier for it.
///
/// Total over all twelve [`Codec`] values. AudioToolbox ships no Vorbis, Opus or
/// FLAC `AudioConverter`, and every picture codec is VideoToolbox's job, so those
/// return `None` and this module does not register them.
#[must_use]
pub fn audio_format_id(codec: Codec) -> Option<AudioFormatID> {
    Some(match codec {
        Codec::Audio(AudioCodec::Mp3) => objc2_core_audio_types::kAudioFormatMPEGLayer3,
        Codec::Audio(AudioCodec::Aac) => objc2_core_audio_types::kAudioFormatMPEG4AAC,
        Codec::Audio(AudioCodec::Vorbis | AudioCodec::Opus | AudioCodec::Flac) => return None,
        Codec::Audio(AudioCodec::Alac) => objc2_core_audio_types::kAudioFormatAppleLossless,
        Codec::Video(_) => return None,
    })
}

/// Registers every codec VideoToolbox and AudioToolbox can actually serve on
/// this machine.
///
/// Registration is decided by the same probes the registry later runs, so the
/// capability report cannot drift away from reality. A framework that cannot be
/// used at all registers nothing and is reported as unusable.
pub fn register(registry: &mut Registry) {
    video_toolbox::register(registry);
    audio_toolbox::register(registry);
}

/// `true` when at least one of VideoToolbox and AudioToolbox can be used in this
/// process.
///
/// Never panics: a framework that cannot be reached makes its half return
/// `false`.
#[must_use]
pub fn available() -> bool {
    video_toolbox::available() || audio_toolbox::available()
}

/// Probes the Apple media engines on demand.
///
/// Never called during [`register`]; the caller asks for this explicitly and it
/// may touch the frameworks. A framework that is not usable contributes no
/// device record at all.
#[must_use]
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    let mut devices = video_toolbox::probe_devices();
    devices.extend(audio_toolbox::probe_devices());
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_video_codec_table_is_total_over_all_twelve_codecs() {
        let mut mapped = 0usize;
        for codec in Codec::ALL {
            match video_codec_type(codec) {
                Some(value) => {
                    mapped += 1;
                    assert!(codec.is_video(), "{codec} is not a picture codec but mapped to {value:#x}");
                    assert_ne!(value, 0, "{codec} mapped to the null codec type");
                }
                None => assert!(
                    !codec.is_video() || codec == Codec::Video(VideoCodec::Vp8),
                    "{codec} must have a CoreMedia codec type"
                ),
            }
        }
        // H.264, HEVC, VVC, VP9 and AV1: the five picture codecs CoreMedia names.
        assert_eq!(mapped, 5);
    }

    #[test]
    fn the_video_codec_table_holds_the_documented_four_character_codes() {
        assert_eq!(video_codec_type(Codec::Video(VideoCodec::H264)), Some(0x6176_6331)); // 'avc1'
        assert_eq!(video_codec_type(Codec::Video(VideoCodec::H265)), Some(0x6876_6331)); // 'hvc1'
        assert_eq!(video_codec_type(Codec::Video(VideoCodec::Vp9)), Some(0x7670_3039)); // 'vp09'
        assert_eq!(video_codec_type(Codec::Video(VideoCodec::Av1)), Some(0x6176_3031)); // 'av01'
        assert_eq!(video_codec_type(Codec::Video(VideoCodec::H266)), Some(0x7676_6331)); // 'vvc1'
        assert_eq!(video_codec_type(Codec::Video(VideoCodec::Vp8)), None);
    }

    #[test]
    fn the_audio_format_table_is_total_over_all_twelve_codecs() {
        assert_eq!(audio_format_id(Codec::Audio(AudioCodec::Mp3)), Some(0x2e6d_7033)); // '.mp3'
        assert_eq!(audio_format_id(Codec::Audio(AudioCodec::Aac)), Some(0x6161_6320)); // 'aac '
        assert_eq!(audio_format_id(Codec::Audio(AudioCodec::Alac)), Some(0x616c_6163)); // 'alac'
        for codec in Codec::ALL {
            let known = audio_format_id(codec).is_some();
            let expected =
                matches!(codec, Codec::Audio(AudioCodec::Mp3 | AudioCodec::Aac | AudioCodec::Alac));
            assert_eq!(known, expected, "{codec} audio-format mapping");
        }
    }

    #[test]
    fn the_two_tables_never_claim_the_same_codec() {
        for codec in Codec::ALL {
            assert!(
                !(video_codec_type(codec).is_some() && audio_format_id(codec).is_some()),
                "{codec} is claimed by both frameworks"
            );
        }
    }

    #[test]
    fn the_four_char_code_trick_is_readable() {
        // `'!pri'`, the code the platform contract uses as its example.
        assert_eq!(four_char_code(0x2170_7269), "!pri");
        assert_eq!(four_char_code(0x666d_743f), "fmt?");
        assert_eq!(four_char_code(0), "....");
        // Non-printable bytes never leak control characters into a log line.
        assert_eq!(four_char_code(1), "....");
        assert_eq!(four_char_code(-1), "....");
    }

    #[test]
    fn known_os_status_codes_get_named_text() {
        assert_eq!(os_status_text(0), "no error");
        let no_decoder = os_status_text(objc2_video_toolbox::kVTCouldNotFindVideoDecoderErr);
        assert!(no_decoder.contains("no decoder for this codec type"), "{no_decoder}");
        let bad_data = os_status_text(objc2_video_toolbox::kVTVideoDecoderBadDataErr);
        assert!(bad_data.contains("malformed"), "{bad_data}");
        let unsupported = os_status_text(objc2_audio_toolbox::kAudioFormatUnsupportedDataFormatError);
        assert!(unsupported.contains("does not support this data format"), "{unsupported}");
        assert!(os_status_text(-50).contains("invalid parameter"));
        assert!(os_status_text(objc2_core_video::kCVReturnInvalidPixelFormat).contains("pixel format"));
    }

    #[test]
    fn unknown_os_status_codes_stay_specific() {
        // An unnamed four character code keeps both its number and its rendering.
        assert_eq!(os_status_text(0x2170_7269), "OSStatus 561017449 ('!pri')");
        // A code with no printable bytes keeps only its number.
        assert_eq!(os_status_text(-1), "OSStatus -1");
        // Every answer is non-empty, so an error message can never be blank.
        for status in [-12_906, -50, -4, 0, 1, 0x666d_743f, i32::MIN, i32::MAX] {
            assert!(!os_status_text(status).is_empty(), "{status}");
        }
    }

    #[test]
    fn times_round_trip_through_the_coremedia_timebase() {
        for milliseconds in [0i64, 1, 33, 1_000, 1_234_567, -1_000] {
            assert_eq!(
                ms_from_time(time_from_ms(milliseconds)),
                milliseconds,
                "ms {milliseconds} must survive the CMTime round trip"
            );
        }
        assert_eq!(TIME_SCALE, 1_000);
    }

    #[test]
    fn a_non_numeric_time_becomes_zero_rather_than_a_panic() {
        // SAFETY: reading a `pub static` constant that CoreMedia initialises.
        let invalid = unsafe { objc2_core_media::kCMTimeInvalid };
        assert_eq!(ms_from_time(invalid), 0);
    }

    #[test]
    fn available_is_a_bool_and_never_panics() {
        // The point of this test is that a machine with no usable media engine
        // still gets an answer instead of an abort.
        let value = available();
        assert!(value || !value);
    }

    #[test]
    fn probing_never_panics() {
        let devices = probe_devices();
        assert!(devices.len() <= 8, "implausible device count: {}", devices.len());
    }
}
