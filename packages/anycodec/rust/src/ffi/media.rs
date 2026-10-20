//! Packet, frame and buffer ownership across the C boundary.
//!
//! Every type here follows the same rule: the pointer returned by a `*_new` or a
//! `*_receive` is **caller-owned** and must be released with the matching
//! `*_free`. Pointers handed *out* of an object (`anycodec_packet_data`, ...) are
//! **borrowed** and stay valid only while the owning handle lives.

use std::os::raw::{c_char, c_uchar};

use crate::media::buffer::Buffer;
use crate::media::codec::Codec;
use crate::media::frame::Frame;
use crate::media::packet::Packet;

use super::error::{STATUS_INVALID_ARGUMENT, STATUS_OK, set_error};
use super::guard;
use super::handles::{AnyCodecBuffer, AnyCodecFrame, AnyCodecPacket};
use super::types::{AnyCodecAudioFrameInfo, AnyCodecVideoFrameInfo, read_bytes};

// ---------------------------------------------------------------------------
// Buffers
// ---------------------------------------------------------------------------

/// Releases a buffer returned by AnyCodec.
///
/// # Safety
///
/// `buffer` must be NULL or a handle returned by AnyCodec and not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_buffer_free(buffer: *mut AnyCodecBuffer) {
    guard::guard_void(|| {
        if !buffer.is_null() {
            // SAFETY: the caller guarantees the handle came from `Box::into_raw`.
            drop(unsafe { Box::from_raw(buffer) });
        }
    });
}

/// Returns a borrowed pointer to the buffer's bytes, valid while `buffer` lives.
///
/// # Safety
///
/// `buffer` must be NULL or a live AnyCodec buffer handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_buffer_data(buffer: *const AnyCodecBuffer) -> *const c_uchar {
    guard::guard(std::ptr::null(), || {
        if buffer.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the caller guarantees `buffer` is live.
        unsafe { (*buffer).inner.as_ptr() }
    })
}

/// Returns the number of bytes in `buffer`, or `0` when `buffer` is NULL.
///
/// # Safety
///
/// `buffer` must be NULL or a live AnyCodec buffer handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_buffer_len(buffer: *const AnyCodecBuffer) -> usize {
    guard::guard(0, || {
        if buffer.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `buffer` is live.
        unsafe { (*buffer).inner.len() }
    })
}

// ---------------------------------------------------------------------------
// Packets
// ---------------------------------------------------------------------------

/// Creates a packet by copying `len` bytes from `data`.
///
/// Returns NULL and sets the thread-local error on failure (unknown codec, NULL
/// data with a non-zero length).
///
/// # Safety
///
/// `data` must be NULL or point to at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_new(
    codec: u32,
    data: *const c_uchar,
    len: usize,
    pts: i64,
    dts: i64,
    duration: i64,
    flags: u32,
) -> *mut AnyCodecPacket {
    guard::guard_ptr(|| {
        let Some(codec) = Codec::from_raw(codec) else {
            set_error(&crate::error::Error::invalid_argument(format!(
                "anycodec_packet_new: unknown codec id {codec}"
            )));
            return std::ptr::null_mut();
        };
        // SAFETY: the caller guarantees readability.
        let Some(bytes) = (unsafe { read_bytes(data, len) }) else {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_packet_new: data is NULL but len is non-zero",
            ));
            return std::ptr::null_mut();
        };
        let packet = Packet::new(codec, Buffer::from_slice(bytes), pts, flags)
            .with_dts(dts)
            .with_duration(duration);
        super::error::clear();
        Box::into_raw(Box::new(AnyCodecPacket { inner: packet }))
    })
}

/// Releases a packet.
///
/// # Safety
///
/// `packet` must be NULL or an unfreed AnyCodec packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_free(packet: *mut AnyCodecPacket) {
    guard::guard_void(|| {
        if !packet.is_null() {
            // SAFETY: the caller guarantees the handle came from `Box::into_raw`.
            drop(unsafe { Box::from_raw(packet) });
        }
    });
}

/// Returns the packet's codec id, or `0` when `packet` is NULL.
///
/// # Safety
///
/// `packet` must be NULL or a live AnyCodec packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_codec(packet: *const AnyCodecPacket) -> u32 {
    guard::guard(0, || {
        if packet.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `packet` is live.
        unsafe { (*packet).inner.codec.raw() }
    })
}

/// Returns a borrowed pointer to the packet payload, valid while `packet` lives.
///
/// # Safety
///
/// `packet` must be NULL or a live AnyCodec packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_data(packet: *const AnyCodecPacket) -> *const c_uchar {
    guard::guard(std::ptr::null(), || {
        if packet.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the caller guarantees `packet` is live.
        unsafe { (*packet).inner.data.as_ptr() }
    })
}

/// Returns the packet payload length, or `0` when `packet` is NULL.
///
/// # Safety
///
/// `packet` must be NULL or a live AnyCodec packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_len(packet: *const AnyCodecPacket) -> usize {
    guard::guard(0, || {
        if packet.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `packet` is live.
        unsafe { (*packet).inner.len() }
    })
}

/// Returns the packet's presentation timestamp.
///
/// # Safety
///
/// `packet` must be NULL or a live AnyCodec packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_pts(packet: *const AnyCodecPacket) -> i64 {
    guard::guard(0, || {
        if packet.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `packet` is live.
        unsafe { (*packet).inner.pts }
    })
}

/// Returns the packet's decode timestamp.
///
/// # Safety
///
/// `packet` must be NULL or a live AnyCodec packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_dts(packet: *const AnyCodecPacket) -> i64 {
    guard::guard(0, || {
        if packet.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `packet` is live.
        unsafe { (*packet).inner.dts }
    })
}

/// Returns the packet's duration, `0` when unknown.
///
/// # Safety
///
/// `packet` must be NULL or a live AnyCodec packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_duration(packet: *const AnyCodecPacket) -> i64 {
    guard::guard(0, || {
        if packet.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `packet` is live.
        unsafe { (*packet).inner.duration }
    })
}

/// Returns the packet's flags: a bitmask of `ANYCODEC_PACKET_FLAG_*`.
///
/// # Safety
///
/// `packet` must be NULL or a live AnyCodec packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_packet_flags(packet: *const AnyCodecPacket) -> u32 {
    guard::guard(0, || {
        if packet.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `packet` is live.
        unsafe { (*packet).inner.flags }
    })
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// Releases a frame.
///
/// # Safety
///
/// `frame` must be NULL or an unfreed AnyCodec frame handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_free(frame: *mut AnyCodecFrame) {
    guard::guard_void(|| {
        if !frame.is_null() {
            // SAFETY: the caller guarantees the handle came from `Box::into_raw`.
            drop(unsafe { Box::from_raw(frame) });
        }
    });
}

/// Returns `1` when the frame holds a picture and `0` otherwise.
///
/// # Safety
///
/// `frame` must be NULL or a live AnyCodec frame handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_is_video(frame: *const AnyCodecFrame) -> u32 {
    guard::guard(0, || {
        if frame.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `frame` is live.
        u32::from(unsafe { (*frame).inner.is_video() })
    })
}

/// Writes the picture layout of `frame` into `out`.
///
/// Returns `0` on success, `ANYCODEC_STATUS_INVALID_ARGUMENT` when a pointer is
/// NULL or the frame is not a picture.
///
/// # Safety
///
/// `frame` must be a live AnyCodec frame handle and `out` a writable
/// [`AnyCodecVideoFrameInfo`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_video_info(
    frame: *const AnyCodecFrame,
    out: *mut AnyCodecVideoFrameInfo,
) -> i32 {
    guard::guard_status(|| {
        if frame.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_frame_video_info: frame or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees both pointers are valid.
        let Frame::Video(video) = (unsafe { &(*frame).inner }) else {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_frame_video_info: frame is not a picture",
            ));
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: `out` is non-null and writable.
        unsafe {
            *out = AnyCodecVideoFrameInfo {
                pixel_format: video.format.raw(),
                width: video.width,
                height: video.height,
                plane_count: video.planes.len() as u32,
                pts: video.pts,
                duration: video.duration,
            };
        }
        super::error::clear();
        STATUS_OK
    })
}

/// Returns a borrowed pointer to plane `index` of a picture frame.
///
/// Returns NULL when `frame` is NULL, is not a picture, or `index` is out of
/// range. The pointer is valid while `frame` lives.
///
/// # Safety
///
/// `frame` must be NULL or a live AnyCodec frame handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_video_plane(
    frame: *const AnyCodecFrame,
    index: u32,
) -> *const c_uchar {
    guard::guard(std::ptr::null(), || {
        if frame.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the caller guarantees `frame` is live.
        let Frame::Video(video) = (unsafe { &(*frame).inner }) else {
            return std::ptr::null();
        };
        video.planes.get(index as usize).map_or(std::ptr::null(), |plane| plane.data().as_ptr())
    })
}

/// Returns the byte length of plane `index`, or `0` when it does not exist.
///
/// # Safety
///
/// `frame` must be NULL or a live AnyCodec frame handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_video_plane_len(
    frame: *const AnyCodecFrame,
    index: u32,
) -> usize {
    guard::guard(0, || {
        if frame.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `frame` is live.
        let Frame::Video(video) = (unsafe { &(*frame).inner }) else {
            return 0;
        };
        video.planes.get(index as usize).map_or(0, |plane| plane.len())
    })
}

/// Returns the row stride of plane `index`, or `0` when it does not exist.
///
/// # Safety
///
/// `frame` must be NULL or a live AnyCodec frame handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_video_stride(
    frame: *const AnyCodecFrame,
    index: u32,
) -> usize {
    guard::guard(0, || {
        if frame.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `frame` is live.
        let Frame::Video(video) = (unsafe { &(*frame).inner }) else {
            return 0;
        };
        video.planes.get(index as usize).map_or(0, |plane| plane.stride())
    })
}

/// Writes the audio layout of `frame` into `out`.
///
/// Returns `0` on success, `ANYCODEC_STATUS_INVALID_ARGUMENT` on a NULL pointer
/// or when the frame is not audio.
///
/// # Safety
///
/// `frame` must be a live AnyCodec frame handle and `out` a writable
/// [`AnyCodecAudioFrameInfo`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_audio_info(
    frame: *const AnyCodecFrame,
    out: *mut AnyCodecAudioFrameInfo,
) -> i32 {
    guard::guard_status(|| {
        if frame.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_frame_audio_info: frame or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees both pointers are valid.
        let Frame::Audio(audio) = (unsafe { &(*frame).inner }) else {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_frame_audio_info: frame is not audio",
            ));
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: `out` is non-null and writable.
        unsafe {
            *out = AnyCodecAudioFrameInfo {
                sample_format: audio.format.raw(),
                sample_rate: audio.sample_rate,
                channels: audio.channels.raw(),
                frames: audio.frames,
                pts: audio.pts,
            };
        }
        super::error::clear();
        STATUS_OK
    })
}

/// Returns a borrowed pointer to interleaved audio samples, valid while `frame` lives.
///
/// # Safety
///
/// `frame` must be NULL or a live AnyCodec frame handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_audio_data(frame: *const AnyCodecFrame) -> *const c_uchar {
    guard::guard(std::ptr::null(), || {
        if frame.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the caller guarantees `frame` is live.
        match unsafe { &(*frame).inner } {
            Frame::Audio(audio) => audio.samples.as_ptr(),
            Frame::Video(_) => std::ptr::null(),
        }
    })
}

/// Returns the audio payload length in bytes, or `0` when the frame is not audio.
///
/// # Safety
///
/// `frame` must be NULL or a live AnyCodec frame handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_audio_len(frame: *const AnyCodecFrame) -> usize {
    guard::guard(0, || {
        if frame.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `frame` is live.
        match unsafe { &(*frame).inner } {
            Frame::Audio(audio) => audio.samples.len(),
            Frame::Video(_) => 0,
        }
    })
}

/// Returns the frame's presentation timestamp.
///
/// # Safety
///
/// `frame` must be NULL or a live AnyCodec frame handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_frame_pts(frame: *const AnyCodecFrame) -> i64 {
    guard::guard(0, || {
        if frame.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `frame` is live.
        unsafe { (*frame).inner.pts() }
    })
}

/// A borrowed reference to a C string, kept for API symmetry. Never used to
/// transfer ownership.
pub type AnyCodecCStr = *const c_char;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::codec::VideoCodec;
    use crate::media::format::PixelFormat;
    use crate::media::frame::{Plane, VideoFrame};

    #[test]
    fn packet_round_trip_through_c() {
        let payload = [1u8, 2, 3, 4, 5];
        // SAFETY: `payload` is valid for `payload.len()` bytes.
        let packet = unsafe {
            anycodec_packet_new(
                super::super::types::ANYCODEC_CODEC_H264,
                payload.as_ptr(),
                payload.len(),
                11,
                9,
                3,
                super::super::types::ANYCODEC_PACKET_FLAG_KEYFRAME,
            )
        };
        assert!(!packet.is_null());
        // SAFETY: `packet` is a live handle produced above.
        unsafe {
            assert_eq!(anycodec_packet_len(packet), 5);
            assert_eq!(anycodec_packet_pts(packet), 11);
            assert_eq!(anycodec_packet_dts(packet), 9);
            assert_eq!(anycodec_packet_duration(packet), 3);
            assert_eq!(
                anycodec_packet_flags(packet) & super::super::types::ANYCODEC_PACKET_FLAG_KEYFRAME,
                1
            );
            assert_eq!(
                anycodec_packet_codec(packet),
                super::super::types::ANYCODEC_CODEC_H264
            );
            let data = anycodec_packet_data(packet);
            assert_eq!(std::slice::from_raw_parts(data, 5), &payload);
            anycodec_packet_free(packet);
        }
    }

    #[test]
    fn packet_new_rejects_an_unknown_codec_and_null_data() {
        super::super::error::clear();
        // SAFETY: NULL data with a non-zero length is explicitly handled.
        let bad_codec = unsafe { anycodec_packet_new(9999, std::ptr::null(), 0, 0, 0, 0, 0) };
        assert!(bad_codec.is_null());
        assert_eq!(super::super::error::last_status(), STATUS_INVALID_ARGUMENT);

        // SAFETY: NULL data with a non-zero length is explicitly handled.
        let bad_data = unsafe {
            anycodec_packet_new(super::super::types::ANYCODEC_CODEC_H264, std::ptr::null(), 4, 0, 0, 0, 0)
        };
        assert!(bad_data.is_null());
        assert_eq!(super::super::error::last_status(), STATUS_INVALID_ARGUMENT);
    }

    #[test]
    fn frame_accessors_are_null_safe() {
        // SAFETY: NULL is explicitly handled everywhere.
        unsafe {
            assert!(anycodec_frame_video_plane(std::ptr::null(), 0).is_null());
            assert_eq!(anycodec_frame_video_plane_len(std::ptr::null(), 0), 0);
            assert_eq!(anycodec_frame_video_stride(std::ptr::null(), 0), 0);
            assert_eq!(anycodec_frame_is_video(std::ptr::null()), 0);
            assert_eq!(anycodec_frame_pts(std::ptr::null()), 0);
            assert!(anycodec_frame_audio_data(std::ptr::null()).is_null());
            anycodec_frame_free(std::ptr::null_mut());
            anycodec_packet_free(std::ptr::null_mut());
            anycodec_buffer_free(std::ptr::null_mut());
        }
    }

    #[test]
    fn video_frame_info_matches_the_frame() {
        let y = Plane::packed(Buffer::from_slice(&[0u8; 16])).unwrap();
        let u = Plane::packed(Buffer::from_slice(&[0u8; 4])).unwrap();
        let v = Plane::packed(Buffer::from_slice(&[0u8; 4])).unwrap();
        let frame = Frame::Video(
            VideoFrame::new(PixelFormat::I420, 4, 4, vec![y, u, v], 5).unwrap(),
        );
        let handle = Box::into_raw(Box::new(AnyCodecFrame { inner: frame }));
        let mut info = AnyCodecVideoFrameInfo::default();
        // SAFETY: `handle` is live and `info` is a valid local.
        unsafe {
            assert_eq!(anycodec_frame_video_info(handle, &mut info), 0);
            assert_eq!(info.width, 4);
            assert_eq!(info.height, 4);
            assert_eq!(info.pixel_format, super::super::types::ANYCODEC_PIXEL_FORMAT_I420);
            assert_eq!(info.plane_count, 3);
            assert_eq!(info.pts, 5);
            assert_eq!(anycodec_frame_video_plane_len(handle, 0), 16);
            assert_eq!(anycodec_frame_video_plane_len(handle, 9), 0);
            assert_eq!(anycodec_frame_is_video(handle), 1);
            anycodec_frame_free(handle);
        }
        let _ = VideoCodec::H264;
    }
}
