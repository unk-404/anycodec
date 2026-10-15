//! The encoder half of the C ABI.

use crate::engine::backend::BackendPreference;
use crate::engine::registry;
use crate::media::buffer::Buffer;
use crate::media::codec::Codec;
use crate::media::config::{EncoderConfig, Preset};
use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};
use crate::media::frame::{AudioFrame, Frame, Plane, VideoFrame};

use super::codec_path::AnyCodecCodecPath;
use super::error::{
    STATUS_INVALID_ARGUMENT, STATUS_NEED_MORE_INPUT, STATUS_GOT_OUTPUT, STATUS_OK, set_error,
};
use super::guard;
use super::handles::{AnyCodecBuffer, AnyCodecEncoder, AnyCodecPacket, drop_encoder_handle, into_encoder_handle};
use super::types::{AnyCodecAudioFrame, AnyCodecEncoderConfig, AnyCodecVideoFrame, read_cstring};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Converts the flat C configuration into the Rust one, validating every enum.
fn config_from_c(config: &AnyCodecEncoderConfig) -> crate::error::Result<EncoderConfig> {
    use crate::error::Error;

    let codec = Codec::from_raw(config.codec)
        .ok_or_else(|| Error::invalid_argument(format!("unknown codec id {}", config.codec)))?;
    let pixel_format = PixelFormat::from_raw(config.pixel_format)
        .ok_or_else(|| Error::invalid_argument(format!("unknown pixel format {}", config.pixel_format)))?;
    let sample_format = SampleFormat::from_raw(config.sample_format)
        .ok_or_else(|| Error::invalid_argument(format!("unknown sample format {}", config.sample_format)))?;
    let channels = ChannelLayout::from_raw(config.channels)
        .ok_or_else(|| Error::invalid_argument(format!("unknown channel layout {}", config.channels)))?;
    let preset = Preset::from_raw(config.preset)
        .ok_or_else(|| Error::invalid_argument(format!("unknown preset {}", config.preset)))?;

    let mut options = Vec::new();
    if config.option_count > 0 {
        if config.option_keys.is_null() || config.option_values.is_null() {
            return Err(Error::invalid_argument(
                "option_count is non-zero but option_keys/option_values is NULL",
            ));
        }
        for index in 0..config.option_count as usize {
            // SAFETY: the caller guarantees both arrays hold `option_count`
            // readable pointers, each NULL or a valid C string.
            let key = unsafe { read_cstring(*config.option_keys.add(index)) };
            // SAFETY: as above.
            let value = unsafe { read_cstring(*config.option_values.add(index)) };
            options.push((key, value));
        }
    }

    Ok(EncoderConfig {
        codec,
        width: config.width,
        height: config.height,
        pixel_format,
        framerate_num: config.framerate_num,
        framerate_den: config.framerate_den.max(1),
        keyframe_interval: config.keyframe_interval,
        sample_rate: config.sample_rate,
        channels,
        sample_format,
        frame_duration_ms: config.frame_duration_ms,
        bitrate: config.bitrate,
        quality: config.quality,
        preset,
        options,
    })
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// Copies a C picture description into an owned [`VideoFrame`].
///
/// # Safety
///
/// `frame` must point to a readable [`AnyCodecVideoFrame`] whose `planes`/`strides`
/// arrays hold `plane_count` readable entries pointing at readable plane data.
unsafe fn video_frame_from_c(frame: &AnyCodecVideoFrame) -> crate::error::Result<VideoFrame> {
    use crate::error::Error;

    let format = PixelFormat::from_raw(frame.pixel_format).ok_or_else(|| {
        Error::invalid_argument(format!("unknown pixel format {}", frame.pixel_format))
    })?;
    let count = frame.plane_count as usize;
    if count != format.plane_count() {
        return Err(Error::invalid_argument(format!(
            "{} needs {} planes but plane_count is {count}",
            format.name(),
            format.plane_count()
        )));
    }
    if frame.planes.is_null() || frame.strides.is_null() {
        return Err(Error::invalid_argument("planes/strides is NULL but plane_count is non-zero"));
    }

    let mut planes = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: the caller guarantees the arrays hold `plane_count` entries.
        let stride = unsafe { *frame.strides.add(index) };
        // SAFETY: as above.
        let data = unsafe { *frame.planes.add(index) };
        let (_, height) = format.plane_size(frame.width, frame.height, index)?;
        let row_bytes = format.default_stride(frame.width, index)?;
        if data.is_null() {
            return Err(Error::invalid_argument(format!("plane {index} is NULL")));
        }
        let stride = stride.max(row_bytes);
        let total = stride
            .checked_mul(height as usize)
            .ok_or_else(|| Error::invalid_argument("plane geometry overflows usize"))?;
        // SAFETY: the caller guarantees `data` points at `stride * height` readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(data, total) };
        planes.push(Plane::new(Buffer::from_slice(bytes), stride)?);
    }

    let mut video = VideoFrame::new(format, frame.width, frame.height, planes, frame.pts)?;
    video.duration = frame.duration;
    video.keyframe = frame.keyframe != 0;
    Ok(video)
}

/// Copies a C audio description into an owned [`AudioFrame`].
///
/// # Safety
///
/// `frame` must point to a readable [`AnyCodecAudioFrame`] whose `data` field
/// points at `len` readable bytes.
unsafe fn audio_frame_from_c(frame: &AnyCodecAudioFrame) -> crate::error::Result<AudioFrame> {
    use crate::error::Error;

    let format = SampleFormat::from_raw(frame.sample_format).ok_or_else(|| {
        Error::invalid_argument(format!("unknown sample format {}", frame.sample_format))
    })?;
    let channels = ChannelLayout::from_raw(frame.channels).ok_or_else(|| {
        Error::invalid_argument(format!("unknown channel layout {}", frame.channels))
    })?;
    let expected = channels.byte_len(frame.frames, format);
    if frame.len < expected {
        return Err(Error::invalid_argument(format!(
            "{} frame of {} x {} needs {expected} bytes but len is {}",
            format.name(),
            frame.frames,
            channels.name(),
            frame.len
        )));
    }
    if frame.data.is_null() && frame.len > 0 {
        return Err(Error::invalid_argument("data is NULL but len is non-zero"));
    }
    // SAFETY: the caller guarantees `data` points at `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(frame.data, frame.len) };
    AudioFrame::new(
        format,
        frame.sample_rate,
        channels,
        frame.frames,
        Buffer::from_slice(&bytes[..expected]),
        frame.pts,
    )
}

// ---------------------------------------------------------------------------
// Exported entry points
// ---------------------------------------------------------------------------

/// Creates an encoder for `config`, honouring `preference`.
///
/// `preference` is one of the `ANYCODEC_PREFERENCE_*` constants; pass
/// `ANYCODEC_PREFERENCE_AUTO` to get "platform framework first, software
/// fallback after".
///
/// Returns NULL on failure and records the reason in the thread-local error.
/// Read [`anycodec_encoder_path`](super::encoder::anycodec_encoder_path) on the
/// result to find out whether a software fallback was used.
///
/// # Safety
///
/// `config` must point to a readable [`AnyCodecEncoderConfig`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_new(
    config: *const AnyCodecEncoderConfig,
    preference: u32,
) -> *mut AnyCodecEncoder {
    guard::guard_ptr(|| {
        if config.is_null() {
            set_error(&crate::error::Error::invalid_argument("anycodec_encoder_new: config is NULL"));
            return std::ptr::null_mut();
        }
        let Some(preference) = BackendPreference::from_raw(preference) else {
            set_error(&crate::error::Error::invalid_argument(format!(
                "anycodec_encoder_new: unknown preference {preference}"
            )));
            return std::ptr::null_mut();
        };
        // SAFETY: the caller guarantees `config` is readable.
        let config = match config_from_c(unsafe { &*config }) {
            Ok(config) => config,
            Err(error) => {
                set_error(&error);
                return std::ptr::null_mut();
            }
        };
        match registry().create_encoder(&config, preference) {
            Ok(encoder) => {
                log::debug!("anycodec: created encoder {}", encoder.path());
                super::error::clear();
                into_encoder_handle(encoder)
            }
            Err(error) => {
                set_error(&error);
                std::ptr::null_mut()
            }
        }
    })
}

/// Releases an encoder.
///
/// # Safety
///
/// `encoder` must be NULL or an unfreed handle returned by AnyCodec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_free(encoder: *mut AnyCodecEncoder) {
    guard::guard_void(|| {
        // SAFETY: the caller guarantees the handle is unfreed.
        unsafe { drop_encoder_handle(encoder) };
    });
}

/// Returns the encoder's codec id, or `0` when `encoder` is NULL.
///
/// # Safety
///
/// `encoder` must be NULL or a live encoder handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_codec(encoder: *const AnyCodecEncoder) -> u32 {
    guard::guard(0, || {
        if encoder.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `encoder` is live.
        unsafe { (*encoder).inner.codec().raw() }
    })
}

/// Writes how the encoder is doing its work into `out`.
///
/// Check `out->is_software`: when it is `1`, the encode is running on a software
/// fallback and `out->fallback_reason` says which platform backend was skipped.
///
/// Returns `0` on success, `ANYCODEC_STATUS_INVALID_ARGUMENT` on a NULL pointer.
///
/// # Safety
///
/// `encoder` must be a live encoder handle and `out` a writable
/// [`AnyCodecCodecPath`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_path(
    encoder: *const AnyCodecEncoder,
    out: *mut AnyCodecCodecPath,
) -> i32 {
    guard::guard_status(|| {
        if encoder.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_encoder_path: encoder or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees both pointers are valid.
        let path = unsafe { (*encoder).inner.path() };
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = AnyCodecCodecPath::from(&path) };
        super::error::clear();
        STATUS_OK
    })
}

/// Submits one raw picture, copying the plane data into the session.
///
/// Returns `0` on success. Nothing is returned synchronously; drain the encoder
/// with [`anycodec_encoder_receive`].
///
/// # Safety
///
/// `encoder` must be a live encoder handle and `frame` a readable
/// [`AnyCodecVideoFrame`] with valid plane pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_push_video(
    encoder: *mut AnyCodecEncoder,
    frame: *const AnyCodecVideoFrame,
) -> i32 {
    guard::guard_status(|| {
        if encoder.is_null() || frame.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_encoder_push_video: encoder or frame is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees both pointers are valid.
        let frame = match unsafe { video_frame_from_c(&*frame) } {
            Ok(frame) => frame,
            Err(error) => {
                set_error(&error);
                return error.status();
            }
        };
        // SAFETY: `encoder` is live.
        match unsafe { (*encoder).inner.push(Frame::Video(frame)) } {
            Ok(()) => {
                super::error::clear();
                STATUS_OK
            }
            Err(error) => {
                set_error(&error);
                error.status()
            }
        }
    })
}

/// Submits one block of raw audio, copying the samples into the session.
///
/// # Safety
///
/// `encoder` must be a live encoder handle and `frame` a readable
/// [`AnyCodecAudioFrame`] whose `data` field points at `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_push_audio(
    encoder: *mut AnyCodecEncoder,
    frame: *const AnyCodecAudioFrame,
) -> i32 {
    guard::guard_status(|| {
        if encoder.is_null() || frame.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_encoder_push_audio: encoder or frame is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees both pointers are valid.
        let frame = match unsafe { audio_frame_from_c(&*frame) } {
            Ok(frame) => frame,
            Err(error) => {
                set_error(&error);
                return error.status();
            }
        };
        // SAFETY: `encoder` is live.
        match unsafe { (*encoder).inner.push(Frame::Audio(frame)) } {
            Ok(()) => {
                super::error::clear();
                STATUS_OK
            }
            Err(error) => {
                set_error(&error);
                error.status()
            }
        }
    })
}

/// Drains one finished packet.
///
/// Returns `ANYCODEC_STATUS_GOT_OUTPUT` (`1`) and stores a caller-owned packet in
/// `*out`; `ANYCODEC_STATUS_NEED_MORE_INPUT` (`0`) when more frames are required
/// (leaving `*out` NULL); or a negative status on failure.
///
/// Packets produced by a previous [`anycodec_encoder_flush`] are delivered first.
///
/// # Safety
///
/// `encoder` must be a live encoder handle and `out` a writable pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_receive(
    encoder: *mut AnyCodecEncoder,
    out: *mut *mut AnyCodecPacket,
) -> i32 {
    guard::guard_status(|| {
        if encoder.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_encoder_receive: encoder or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out` is non-null and writable, per the caller's contract.
        unsafe { *out = std::ptr::null_mut() };
        // SAFETY: `encoder` is live.
        let handle = unsafe { &mut *encoder };
        let packet = match handle.pending.pop_front() {
            Some(packet) => Some(packet),
            None => match handle.inner.receive() {
                Ok(packet) => packet,
                Err(error) => {
                    set_error(&error);
                    return error.status();
                }
            },
        };
        super::error::clear();
        match packet {
            Some(packet) => {
                // SAFETY: `out` is writable.
                unsafe { *out = Box::into_raw(Box::new(AnyCodecPacket { inner: packet })) };
                STATUS_GOT_OUTPUT
            }
            None => STATUS_NEED_MORE_INPUT,
        }
    })
}

/// Signals end of stream to the encoder.
///
/// Drain the remaining packets with [`anycodec_encoder_receive`] until it returns
/// `ANYCODEC_STATUS_NEED_MORE_INPUT`.
///
/// # Safety
///
/// `encoder` must be a live encoder handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_flush(encoder: *mut AnyCodecEncoder) -> i32 {
    guard::guard_status(|| {
        if encoder.is_null() {
            set_error(&crate::error::Error::invalid_argument("anycodec_encoder_flush: encoder is NULL"));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `encoder` is live.
        let handle = unsafe { &mut *encoder };
        match handle.inner.flush() {
            Ok(packets) => {
                handle.pending.extend(packets);
                super::error::clear();
                STATUS_OK
            }
            Err(error) => {
                set_error(&error);
                error.status()
            }
        }
    })
}

/// Changes the encoder's target bitrate.
///
/// May return `ANYCODEC_STATUS_UNSUPPORTED` on backends that cannot retune live.
///
/// # Safety
///
/// `encoder` must be a live encoder handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_set_bitrate(
    encoder: *mut AnyCodecEncoder,
    bitrate: u64,
) -> i32 {
    guard::guard_status(|| {
        if encoder.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_encoder_set_bitrate: encoder is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `encoder` is live.
        match unsafe { (*encoder).inner.set_bitrate(bitrate) } {
            Ok(()) => {
                super::error::clear();
                STATUS_OK
            }
            Err(error) => {
                set_error(&error);
                error.status()
            }
        }
    })
}

/// Asks the encoder to emit a keyframe at the next opportunity.
///
/// # Safety
///
/// `encoder` must be a live encoder handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_request_keyframe(encoder: *mut AnyCodecEncoder) -> i32 {
    guard::guard_status(|| {
        if encoder.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_encoder_request_keyframe: encoder is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `encoder` is live.
        match unsafe { (*encoder).inner.request_keyframe() } {
            Ok(()) => {
                super::error::clear();
                STATUS_OK
            }
            Err(error) => {
                set_error(&error);
                error.status()
            }
        }
    })
}

/// Hands out the encoder's codec configuration bytes, if it has produced any.
///
/// Returns `1` and a caller-owned buffer, or `0` and no buffer.
///
/// # Safety
///
/// `encoder` must be a live encoder handle and `out` a writable pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_encoder_extradata(
    encoder: *const AnyCodecEncoder,
    out: *mut *mut AnyCodecBuffer,
) -> i32 {
    guard::guard_status(|| {
        if encoder.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_encoder_extradata: encoder or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out` is writable.
        unsafe { *out = std::ptr::null_mut() };
        // SAFETY: `encoder` is live.
        match unsafe { (*encoder).inner.extradata() } {
            Some(buffer) => {
                super::error::clear();
                // SAFETY: `out` is writable.
                unsafe { *out = Box::into_raw(Box::new(AnyCodecBuffer { inner: buffer })) };
                STATUS_GOT_OUTPUT
            }
            None => {
                super::error::clear();
                STATUS_NEED_MORE_INPUT
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::error;
    use crate::ffi::types::*;

    fn h264_config() -> AnyCodecEncoderConfig {
        AnyCodecEncoderConfig {
            codec: ANYCODEC_CODEC_H264,
            width: 64,
            height: 64,
            pixel_format: ANYCODEC_PIXEL_FORMAT_I420,
            framerate_num: 30,
            framerate_den: 1,
            bitrate: 1_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn create_with_null_config_fails_cleanly() {
        error::clear();
        // SAFETY: NULL is explicitly handled.
        let encoder = unsafe { anycodec_encoder_new(std::ptr::null(), ANYCODEC_PREFERENCE_AUTO) };
        assert!(encoder.is_null());
        assert_eq!(error::last_status(), STATUS_INVALID_ARGUMENT);
    }

    #[test]
    fn create_with_an_unknown_codec_fails_cleanly() {
        error::clear();
        let mut config = h264_config();
        config.codec = 0xdead;
        // SAFETY: `config` is a valid local.
        let encoder = unsafe { anycodec_encoder_new(&config, ANYCODEC_PREFERENCE_AUTO) };
        assert!(encoder.is_null());
        assert_eq!(error::last_status(), STATUS_INVALID_ARGUMENT);
    }

    #[test]
    fn create_with_an_unknown_preference_fails_cleanly() {
        error::clear();
        let config = h264_config();
        // SAFETY: `config` is a valid local.
        let encoder = unsafe { anycodec_encoder_new(&config, 99) };
        assert!(encoder.is_null());
        assert_eq!(error::last_status(), STATUS_INVALID_ARGUMENT);
    }

    #[test]
    fn null_accessors_are_safe() {
        // SAFETY: NULL is explicitly handled by every accessor.
        unsafe {
            assert_eq!(anycodec_encoder_codec(std::ptr::null()), 0);
            anycodec_encoder_free(std::ptr::null_mut());
            assert_eq!(anycodec_encoder_flush(std::ptr::null_mut()), STATUS_INVALID_ARGUMENT);
            assert_eq!(
                anycodec_encoder_set_bitrate(std::ptr::null_mut(), 1),
                STATUS_INVALID_ARGUMENT
            );
        }
    }

    #[test]
    fn options_are_read_from_parallel_arrays() {
        let key = std::ffi::CString::new("profile").unwrap();
        let value = std::ffi::CString::new("high").unwrap();
        let keys = [key.as_ptr()];
        let values = [value.as_ptr()];
        let config = AnyCodecEncoderConfig {
            option_count: 1,
            option_keys: keys.as_ptr(),
            option_values: values.as_ptr(),
            ..h264_config()
        };
        let converted = config_from_c(&config).unwrap();
        assert_eq!(converted.option("profile"), Some("high"));
    }

    #[test]
    fn missing_option_arrays_are_rejected() {
        let config = AnyCodecEncoderConfig { option_count: 2, ..h264_config() };
        assert!(config_from_c(&config).is_err());
    }
}
