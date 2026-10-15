//! The `#[repr(C)]` vocabulary of the C ABI.
//!
//! Everything here is a plain-old-data struct with fixed-size fields. Enums are
//! represented as `u32` plus `ANYCODEC_*` constants, so the ABI never depends on
//! a compiler's enum layout choices.
//!
//! Numeric values deliberately match the Rust enums in [`crate::media`] and
//! [`crate::engine`], so conversions are `as u32` in both directions.

use std::os::raw::{c_char, c_uchar};

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

/// H.264 / AVC (ITU-T H.264, ISO-IEC 14496-10).
pub const ANYCODEC_CODEC_H264: u32 = 1;
/// H.265 / HEVC (ITU-T H.265, ISO-IEC 23008-2).
pub const ANYCODEC_CODEC_H265: u32 = 2;
/// H.266 / VVC (ITU-T H.266, ISO-IEC 23090-3).
pub const ANYCODEC_CODEC_H266: u32 = 3;
/// Google VP8.
pub const ANYCODEC_CODEC_VP8: u32 = 4;
/// Google VP9.
pub const ANYCODEC_CODEC_VP9: u32 = 5;
/// AOMedia AV1.
pub const ANYCODEC_CODEC_AV1: u32 = 6;
/// MPEG-1/2 Audio Layer III.
pub const ANYCODEC_CODEC_MP3: u32 = 0x101;
/// MPEG-4 Advanced Audio Coding.
pub const ANYCODEC_CODEC_AAC: u32 = 0x102;
/// Xiph Vorbis.
pub const ANYCODEC_CODEC_VORBIS: u32 = 0x103;
/// IETF Opus (RFC 6716).
pub const ANYCODEC_CODEC_OPUS: u32 = 0x104;
/// Xiph FLAC (lossless).
pub const ANYCODEC_CODEC_FLAC: u32 = 0x105;
/// Apple Lossless Audio Codec.
pub const ANYCODEC_CODEC_ALAC: u32 = 0x106;

// ---------------------------------------------------------------------------
// Pixel formats
// ---------------------------------------------------------------------------

/// 3-plane 4:2:0, 8-bit.
pub const ANYCODEC_PIXEL_FORMAT_I420: u32 = 1;
/// Semi-planar 4:2:0, 8-bit, U first.
pub const ANYCODEC_PIXEL_FORMAT_NV12: u32 = 2;
/// Semi-planar 4:2:0, 8-bit, V first.
pub const ANYCODEC_PIXEL_FORMAT_NV21: u32 = 3;
/// Packed RGBA, 8 bits per channel.
pub const ANYCODEC_PIXEL_FORMAT_RGBA8: u32 = 4;
/// Packed BGRA, 8 bits per channel.
pub const ANYCODEC_PIXEL_FORMAT_BGRA8: u32 = 5;
/// Packed 4:2:2 as Y0 U Y1 V.
pub const ANYCODEC_PIXEL_FORMAT_YUY2: u32 = 6;
/// 3-plane 4:2:0, 10-bit in 16-bit words.
pub const ANYCODEC_PIXEL_FORMAT_P010: u32 = 7;

// ---------------------------------------------------------------------------
// Sample formats
// ---------------------------------------------------------------------------

/// 32-bit float, interleaved.
pub const ANYCODEC_SAMPLE_FORMAT_F32: u32 = 1;
/// 16-bit signed little-endian, interleaved.
pub const ANYCODEC_SAMPLE_FORMAT_S16: u32 = 2;
/// 32-bit signed little-endian, interleaved.
pub const ANYCODEC_SAMPLE_FORMAT_S32: u32 = 3;
/// 8-bit unsigned, interleaved.
pub const ANYCODEC_SAMPLE_FORMAT_U8: u32 = 4;

// ---------------------------------------------------------------------------
// Channel layouts
// ---------------------------------------------------------------------------

/// 1 channel.
pub const ANYCODEC_CHANNEL_LAYOUT_MONO: u32 = 1;
/// 2 channels.
pub const ANYCODEC_CHANNEL_LAYOUT_STEREO: u32 = 2;
/// 3 channels.
pub const ANYCODEC_CHANNEL_LAYOUT_STEREO_21: u32 = 3;
/// 4 channels.
pub const ANYCODEC_CHANNEL_LAYOUT_QUAD: u32 = 4;
/// 6 channels.
pub const ANYCODEC_CHANNEL_LAYOUT_FIVE_ONE: u32 = 5;
/// 8 channels.
pub const ANYCODEC_CHANNEL_LAYOUT_SEVEN_ONE: u32 = 6;

// ---------------------------------------------------------------------------
// Backends
// ---------------------------------------------------------------------------

/// Android `AMediaCodec`.
pub const ANYCODEC_BACKEND_MEDIACODEC: u32 = 0;
/// Windows Media Foundation.
pub const ANYCODEC_BACKEND_MEDIAFOUNDATION: u32 = 1;
/// Apple VideoToolbox.
pub const ANYCODEC_BACKEND_VIDEOTOOLBOX: u32 = 2;
/// Apple AudioToolbox.
pub const ANYCODEC_BACKEND_AUDIOTOOLBOX: u32 = 3;
/// Linux GStreamer.
pub const ANYCODEC_BACKEND_GSTREAMER: u32 = 4;
/// Browser WebCodecs.
pub const ANYCODEC_BACKEND_WEBCODECS: u32 = 5;
/// Pure-Rust software codec.
pub const ANYCODEC_BACKEND_RUST: u32 = 6;
/// Runtime-loaded `libavcodec`.
pub const ANYCODEC_BACKEND_FFMPEG: u32 = 7;
/// Application-supplied backend.
pub const ANYCODEC_BACKEND_EXTERNAL: u32 = 8;

/// A platform media framework did the work.
pub const ANYCODEC_IMPLEMENTATION_NATIVE: u32 = 0;
/// A software codec did the work.
pub const ANYCODEC_IMPLEMENTATION_SOFTWARE: u32 = 1;

// ---------------------------------------------------------------------------
// Backend preferences
// ---------------------------------------------------------------------------

/// Platform frameworks first, then software fallbacks.
pub const ANYCODEC_PREFERENCE_AUTO: u32 = 0;
/// Only platform frameworks.
pub const ANYCODEC_PREFERENCE_NATIVE_ONLY: u32 = 1;
/// Only software codecs.
pub const ANYCODEC_PREFERENCE_SOFTWARE_ONLY: u32 = 2;
/// Only implementations known to run on dedicated hardware.
pub const ANYCODEC_PREFERENCE_REQUIRE_HARDWARE: u32 = 3;

// ---------------------------------------------------------------------------
// Encoder presets
// ---------------------------------------------------------------------------

/// Lowest latency.
pub const ANYCODEC_PRESET_SPEED: u32 = 0;
/// Platform default.
pub const ANYCODEC_PRESET_BALANCED: u32 = 1;
/// Smallest output.
pub const ANYCODEC_PRESET_QUALITY: u32 = 2;

// ---------------------------------------------------------------------------
// Packet flags
// ---------------------------------------------------------------------------

/// The packet can be decoded standalone.
pub const ANYCODEC_PACKET_FLAG_KEYFRAME: u32 = 1 << 0;
/// The packet carries codec configuration.
pub const ANYCODEC_PACKET_FLAG_HEADER: u32 = 1 << 1;
/// End of stream.
pub const ANYCODEC_PACKET_FLAG_EOS: u32 = 1 << 2;
/// Produced by a hardware pipeline.
pub const ANYCODEC_PACKET_FLAG_HARDWARE: u32 = 1 << 3;
/// Known to be damaged.
pub const ANYCODEC_PACKET_FLAG_CORRUPT: u32 = 1 << 4;

/// Length of [`AnyCodecBackendInfo::note`], including the NUL terminator.
pub const ANYCODEC_NOTE_CAPACITY: usize = 256;
/// Length of [`AnyCodecCodecPath::detail`], including the NUL terminator.
pub const ANYCODEC_DETAIL_CAPACITY: usize = 256;
/// Length of [`AnyCodecCodecPath::fallback_reason`], including the NUL terminator.
pub const ANYCODEC_REASON_CAPACITY: usize = 512;
/// Length of [`AnyCodecDeviceInfo::name`], including the NUL terminator.
pub const ANYCODEC_DEVICE_NAME_CAPACITY: usize = 128;
/// Length of [`AnyCodecDeviceInfo::api_version`] and `driver_version`.
pub const ANYCODEC_VERSION_CAPACITY: usize = 64;
/// Length of [`AnyCodecString::data`], including the NUL terminator.
pub const ANYCODEC_STRING_CAPACITY: usize = 512;

/// Copies `value` into a fixed-size, NUL-terminated C array, truncating if
/// necessary and always terminating.
pub fn write_cstring<const N: usize>(destination: &mut [c_char; N], value: &str) {
    if N == 0 {
        return;
    }
    let bytes = value.as_bytes();
    let limit = N - 1;
    for (slot, byte) in destination.iter_mut().zip(bytes.iter()).take(limit) {
        *slot = *byte as c_char;
    }
    for slot in destination.iter_mut().take(N).skip(bytes.len().min(limit)) {
        *slot = 0;
    }
}

/// Reads a NUL-terminated C string into a `String`.
///
/// # Safety
///
/// `pointer` must be NULL or point to a NUL-terminated string that stays valid
/// for the duration of the call.
pub unsafe fn read_cstring(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees `pointer` is NULL or a valid C string.
    unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy().into_owned()
}

/// Validates a `(pointer, length)` pair coming from C.
///
/// # Safety
///
/// `pointer` must be NULL or point to at least `length` readable bytes.
pub unsafe fn read_bytes<'a>(pointer: *const c_uchar, length: usize) -> Option<&'a [u8]> {
    if length == 0 {
        return Some(&[]);
    }
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees the pointed-to region is readable.
    Some(unsafe { std::slice::from_raw_parts(pointer, length) })
}

/// Everything needed to start an encoder, flattened for C.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AnyCodecEncoderConfig {
    /// One of the `ANYCODEC_CODEC_*` constants.
    pub codec: u32,
    /// Luma width in pixels; ignored for audio codecs.
    pub width: u32,
    /// Luma height in pixels; ignored for audio codecs.
    pub height: u32,
    /// One of the `ANYCODEC_PIXEL_FORMAT_*` constants.
    pub pixel_format: u32,
    /// Frames per second numerator.
    pub framerate_num: u32,
    /// Frames per second denominator.
    pub framerate_den: u32,
    /// Target keyframe distance in frames; `0` lets the backend choose.
    pub keyframe_interval: u32,
    /// Samples per second; ignored for video codecs.
    pub sample_rate: u32,
    /// One of the `ANYCODEC_CHANNEL_LAYOUT_*` constants.
    pub channels: u32,
    /// One of the `ANYCODEC_SAMPLE_FORMAT_*` constants.
    pub sample_format: u32,
    /// Requested encoded frame length in milliseconds; `0` lets the backend choose.
    pub frame_duration_ms: u32,
    /// Target bitrate in bits per second; `0` means "use `quality`".
    pub bitrate: u64,
    /// Quality hint in `0.0..=1.0`, or `0.0` for unset.
    pub quality: f32,
    /// One of the `ANYCODEC_PRESET_*` constants.
    pub preset: u32,
    /// Number of entries in `option_keys`/`option_values`.
    pub option_count: u32,
    /// Backend-specific option names; may be NULL when `option_count` is `0`.
    pub option_keys: *const *const c_char,
    /// Backend-specific option values; may be NULL when `option_count` is `0`.
    pub option_values: *const *const c_char,
}

impl Default for AnyCodecEncoderConfig {
    fn default() -> Self {
        Self {
            codec: ANYCODEC_CODEC_H264,
            width: 0,
            height: 0,
            pixel_format: ANYCODEC_PIXEL_FORMAT_I420,
            framerate_num: 30,
            framerate_den: 1,
            keyframe_interval: 0,
            sample_rate: 0,
            channels: ANYCODEC_CHANNEL_LAYOUT_STEREO,
            sample_format: ANYCODEC_SAMPLE_FORMAT_F32,
            frame_duration_ms: 0,
            bitrate: 0,
            quality: 0.0,
            preset: ANYCODEC_PRESET_BALANCED,
            option_count: 0,
            option_keys: std::ptr::null(),
            option_values: std::ptr::null(),
        }
    }
}

/// Everything needed to start a decoder, flattened for C.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AnyCodecDecoderConfig {
    /// One of the `ANYCODEC_CODEC_*` constants.
    pub codec: u32,
    /// Coded width hint.
    pub width: u32,
    /// Coded height hint.
    pub height: u32,
    /// Requested output pixel format, or `0` for the backend default.
    pub pixel_format: u32,
    /// Samples-per-second hint.
    pub sample_rate: u32,
    /// Requested channel layout, or `0` for the backend default.
    pub channels: u32,
    /// Requested output sample format, or `0` for the backend default.
    pub sample_format: u32,
    /// Non-zero when the bitstream uses Annex-B start codes.
    pub annex_b: u32,
    /// Codec configuration bytes (`avcC`, `hvcC`, `AudioSpecificConfig`, ...).
    pub extradata: *const c_uchar,
    /// Length of `extradata` in bytes.
    pub extradata_len: usize,
}

impl Default for AnyCodecDecoderConfig {
    fn default() -> Self {
        Self {
            codec: ANYCODEC_CODEC_H264,
            width: 0,
            height: 0,
            pixel_format: 0,
            sample_rate: 0,
            channels: 0,
            sample_format: 0,
            annex_b: 0,
            extradata: std::ptr::null(),
            extradata_len: 0,
        }
    }
}

/// A raw picture, borrowed for the duration of one call.
///
/// The pointers in `planes`/`strides` are **only read during the call**; AnyCodec
/// copies anything it needs to keep. The caller may reuse or free the buffers as
/// soon as the call returns.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AnyCodecVideoFrame {
    /// One of the `ANYCODEC_PIXEL_FORMAT_*` constants.
    pub pixel_format: u32,
    /// Luma width in pixels.
    pub width: u32,
    /// Luma height in pixels.
    pub height: u32,
    /// Number of valid entries in `planes` and `strides`.
    pub plane_count: u32,
    /// Non-zero when the picture can be decoded standalone.
    pub keyframe: u32,
    /// Presentation timestamp in the stream timebase.
    pub pts: i64,
    /// Duration in the stream timebase; `0` when unknown.
    pub duration: i64,
    /// Plane pointers.
    pub planes: *const *const c_uchar,
    /// Distance in bytes between consecutive rows of each plane.
    pub strides: *const usize,
}

/// A raw block of audio, borrowed for the duration of one call.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AnyCodecAudioFrame {
    /// One of the `ANYCODEC_SAMPLE_FORMAT_*` constants.
    pub sample_format: u32,
    /// Samples per second.
    pub sample_rate: u32,
    /// One of the `ANYCODEC_CHANNEL_LAYOUT_*` constants.
    pub channels: u32,
    /// Number of sample frames (one per channel).
    pub frames: u32,
    /// Presentation timestamp in the stream timebase.
    pub pts: i64,
    /// Interleaved sample bytes.
    pub data: *const c_uchar,
    /// Length of `data` in bytes.
    pub len: usize,
}

/// Layout of a decoded picture.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AnyCodecVideoFrameInfo {
    /// One of the `ANYCODEC_PIXEL_FORMAT_*` constants.
    pub pixel_format: u32,
    /// Luma width in pixels.
    pub width: u32,
    /// Luma height in pixels.
    pub height: u32,
    /// Number of readable planes.
    pub plane_count: u32,
    /// Presentation timestamp.
    pub pts: i64,
    /// Duration, `0` when unknown.
    pub duration: i64,
}

/// Layout of a decoded audio block.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AnyCodecAudioFrameInfo {
    /// One of the `ANYCODEC_SAMPLE_FORMAT_*` constants.
    pub sample_format: u32,
    /// Samples per second.
    pub sample_rate: u32,
    /// One of the `ANYCODEC_CHANNEL_LAYOUT_*` constants.
    pub channels: u32,
    /// Number of sample frames (one per channel).
    pub frames: u32,
    /// Presentation timestamp.
    pub pts: i64,
}

/// Semantic version of the native library.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AnyCodecVersion {
    /// The C ABI revision; the Dart layer refuses a mismatch.
    pub abi: u32,
    /// Major version.
    pub major: u32,
    /// Minor version.
    pub minor: u32,
    /// Patch version.
    pub patch: u32,
}

/// A NUL-terminated string sized for ABI transport.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AnyCodecString {
    /// UTF-8 bytes, NUL terminated and truncated to fit.
    pub data: [c_char; ANYCODEC_STRING_CAPACITY],
}

impl Default for AnyCodecString {
    fn default() -> Self {
        Self { data: [0; ANYCODEC_STRING_CAPACITY] }
    }
}

impl std::fmt::Debug for AnyCodecString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: `data` is always NUL terminated by `From<&str>`.
        let text = unsafe { std::ffi::CStr::from_ptr(self.data.as_ptr()) }.to_string_lossy();
        write!(f, "AnyCodecString({text:?})")
    }
}

impl From<&str> for AnyCodecString {
    fn from(value: &str) -> Self {
        let mut string = Self::default();
        write_cstring(&mut string.data, value);
        string
    }
}

impl AnyCodecString {
    /// The stored text, lossily decoded.
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        // SAFETY: `data` is always NUL terminated.
        unsafe { std::ffi::CStr::from_ptr(self.data.as_ptr()) }.to_string_lossy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::codec::Codec;
    use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};

    #[test]
    fn codec_constants_match_the_rust_enum() {
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_H264).unwrap().name(), "h264");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_H265).unwrap().name(), "h265");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_H266).unwrap().name(), "h266");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_VP8).unwrap().name(), "vp8");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_VP9).unwrap().name(), "vp9");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_AV1).unwrap().name(), "av1");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_MP3).unwrap().name(), "mp3");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_AAC).unwrap().name(), "aac");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_VORBIS).unwrap().name(), "vorbis");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_OPUS).unwrap().name(), "opus");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_FLAC).unwrap().name(), "flac");
        assert_eq!(Codec::from_raw(ANYCODEC_CODEC_ALAC).unwrap().name(), "alac");
    }

    #[test]
    fn format_constants_match_the_rust_enums() {
        for format in PixelFormat::ALL {
            assert!(PixelFormat::from_raw(format.raw()).is_some());
        }
        assert_eq!(PixelFormat::from_raw(ANYCODEC_PIXEL_FORMAT_P010).unwrap().name(), "p010");
        for format in SampleFormat::ALL {
            assert!(SampleFormat::from_raw(format.raw()).is_some());
        }
        for layout in ChannelLayout::ALL {
            assert!(ChannelLayout::from_raw(layout.raw()).is_some());
        }
        assert_eq!(ANYCODEC_CHANNEL_LAYOUT_SEVEN_ONE, 6);
    }

    #[test]
    fn cstrings_are_always_terminated() {
        let mut buffer = [0x7f as c_char; 8];
        write_cstring(&mut buffer, "abcdef");
        assert_eq!(buffer[6], 0);
        // SAFETY: buffer is NUL terminated.
        let text = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
        assert_eq!(text, "abcdef");

        let mut small = [0x7f as c_char; 4];
        write_cstring(&mut small, "longer than four");
        assert_eq!(small[3], 0);
    }

    #[test]
    fn anycodec_string_truncates_safely() {
        let long = "x".repeat(ANYCODEC_STRING_CAPACITY * 2);
        let string = AnyCodecString::from(long.as_str());
        assert_eq!(string.as_str().len(), ANYCODEC_STRING_CAPACITY - 1);
    }

    #[test]
    fn read_cstring_handles_null() {
        // SAFETY: NULL is explicitly allowed.
        assert_eq!(unsafe { read_cstring(std::ptr::null()) }, "");
    }
}
