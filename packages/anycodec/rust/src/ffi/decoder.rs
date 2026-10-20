//! The decoder half of the C ABI.

use crate::engine::backend::BackendPreference;
use crate::engine::registry;
use crate::media::buffer::Buffer;
use crate::media::codec::Codec;
use crate::media::config::DecoderConfig;
use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};
use crate::media::frame::Frame;
use crate::media::packet::Packet;

use super::codec_path::AnyCodecCodecPath;
use super::error::{
    STATUS_GOT_OUTPUT, STATUS_INVALID_ARGUMENT, STATUS_NEED_MORE_INPUT, STATUS_OK, set_error,
};
use super::guard;
use super::handles::{
    AnyCodecDecoder, AnyCodecFrame, AnyCodecPacket, drop_decoder_handle, into_decoder_handle,
};
use super::types::{AnyCodecDecoderConfig, read_bytes};

/// Converts the flat C configuration into the Rust one, validating every enum.
fn config_from_c(config: &AnyCodecDecoderConfig) -> crate::error::Result<DecoderConfig> {
    use crate::error::Error;

    let codec = Codec::from_raw(config.codec)
        .ok_or_else(|| Error::invalid_argument(format!("unknown codec id {}", config.codec)))?;

    let pixel_format = if config.pixel_format == 0 {
        None
    } else {
        Some(PixelFormat::from_raw(config.pixel_format).ok_or_else(|| {
            Error::invalid_argument(format!("unknown pixel format {}", config.pixel_format))
        })?)
    };
    let sample_format = if config.sample_format == 0 {
        None
    } else {
        Some(SampleFormat::from_raw(config.sample_format).ok_or_else(|| {
            Error::invalid_argument(format!("unknown sample format {}", config.sample_format))
        })?)
    };
    let channels = if config.channels == 0 {
        None
    } else {
        Some(ChannelLayout::from_raw(config.channels).ok_or_else(|| {
            Error::invalid_argument(format!("unknown channel layout {}", config.channels))
        })?)
    };

    // SAFETY: the caller guarantees `extradata` points at `extradata_len`
    // readable bytes, or that the length is zero.
    let extradata = match unsafe { read_bytes(config.extradata, config.extradata_len) } {
        Some(bytes) => Buffer::from_slice(bytes),
        None => {
            return Err(Error::invalid_argument(
                "extradata is NULL but extradata_len is non-zero",
            ));
        }
    };

    Ok(DecoderConfig {
        codec,
        extradata,
        width: config.width,
        height: config.height,
        pixel_format,
        sample_rate: config.sample_rate,
        channels,
        sample_format,
        annex_b: config.annex_b != 0,
    })
}

/// Creates a decoder for `config`, honouring `preference`.
///
/// Returns NULL on failure and records the reason in the thread-local error.
/// Read [`anycodec_decoder_path`] on the result to find out whether a software
/// fallback was used.
///
/// # Safety
///
/// `config` must point to a readable [`AnyCodecDecoderConfig`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_decoder_new(
    config: *const AnyCodecDecoderConfig,
    preference: u32,
) -> *mut AnyCodecDecoder {
    guard::guard_ptr(|| {
        if config.is_null() {
            set_error(&crate::error::Error::invalid_argument("anycodec_decoder_new: config is NULL"));
            return std::ptr::null_mut();
        }
        let Some(preference) = BackendPreference::from_raw(preference) else {
            set_error(&crate::error::Error::invalid_argument(format!(
                "anycodec_decoder_new: unknown preference {preference}"
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
        match registry().create_decoder(&config, preference) {
            Ok(decoder) => {
                log::debug!("anycodec: created decoder {}", decoder.path());
                super::error::clear();
                into_decoder_handle(decoder)
            }
            Err(error) => {
                set_error(&error);
                std::ptr::null_mut()
            }
        }
    })
}

/// Releases a decoder.
///
/// # Safety
///
/// `decoder` must be NULL or an unfreed handle returned by AnyCodec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_decoder_free(decoder: *mut AnyCodecDecoder) {
    guard::guard_void(|| {
        // SAFETY: the caller guarantees the handle is unfreed.
        unsafe { drop_decoder_handle(decoder) };
    });
}

/// Returns the decoder's codec id, or `0` when `decoder` is NULL.
///
/// # Safety
///
/// `decoder` must be NULL or a live decoder handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_decoder_codec(decoder: *const AnyCodecDecoder) -> u32 {
    guard::guard(0, || {
        if decoder.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `decoder` is live.
        unsafe { (*decoder).inner.codec().raw() }
    })
}

/// Writes how the decoder is doing its work into `out`.
///
/// # Safety
///
/// `decoder` must be a live decoder handle and `out` a writable
/// [`AnyCodecCodecPath`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_decoder_path(
    decoder: *const AnyCodecDecoder,
    out: *mut AnyCodecCodecPath,
) -> i32 {
    guard::guard_status(|| {
        if decoder.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_decoder_path: decoder or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees both pointers are valid.
        let path = unsafe { (*decoder).inner.path() };
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = AnyCodecCodecPath::from(&path) };
        super::error::clear();
        STATUS_OK
    })
}

/// Submits one compressed packet.
///
/// Header packets (`ANYCODEC_PACKET_FLAG_HEADER`) and end-of-stream markers
/// (`ANYCODEC_PACKET_FLAG_EOS`) are both delivered through this function.
///
/// # Safety
///
/// `decoder` must be a live decoder handle and `packet` a live packet handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_decoder_push(
    decoder: *mut AnyCodecDecoder,
    packet: *const AnyCodecPacket,
) -> i32 {
    guard::guard_status(|| {
        if decoder.is_null() || packet.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_decoder_push: decoder or packet is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees both handles are live.
        let packet = unsafe { (*packet).inner.clone() };
        // SAFETY: `decoder` is live.
        match unsafe { (*decoder).inner.push(packet) } {
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

/// Drains one finished frame.
///
/// Returns `ANYCODEC_STATUS_GOT_OUTPUT` (`1`) with a caller-owned frame in `*out`,
/// `ANYCODEC_STATUS_NEED_MORE_INPUT` (`0`) when more packets are required, or a
/// negative status on failure.
///
/// # Safety
///
/// `decoder` must be a live decoder handle and `out` a writable pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_decoder_receive(
    decoder: *mut AnyCodecDecoder,
    out: *mut *mut AnyCodecFrame,
) -> i32 {
    guard::guard_status(|| {
        if decoder.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_decoder_receive: decoder or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out` is writable.
        unsafe { *out = std::ptr::null_mut() };
        // SAFETY: `decoder` is live.
        let handle = unsafe { &mut *decoder };
        let frame = match handle.pending.pop_front() {
            Some(frame) => Some(frame),
            None => match handle.inner.receive() {
                Ok(frame) => frame,
                Err(error) => {
                    set_error(&error);
                    return error.status();
                }
            },
        };
        super::error::clear();
        match frame {
            Some(frame) => {
                // SAFETY: `out` is writable.
                unsafe { *out = Box::into_raw(Box::new(AnyCodecFrame { inner: frame })) };
                STATUS_GOT_OUTPUT
            }
            None => STATUS_NEED_MORE_INPUT,
        }
    })
}

/// Signals end of stream to the decoder.
///
/// Drain the remaining frames with [`anycodec_decoder_receive`] until it returns
/// `ANYCODEC_STATUS_NEED_MORE_INPUT`.
///
/// # Safety
///
/// `decoder` must be a live decoder handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_decoder_flush(decoder: *mut AnyCodecDecoder) -> i32 {
    guard::guard_status(|| {
        if decoder.is_null() {
            set_error(&crate::error::Error::invalid_argument("anycodec_decoder_flush: decoder is NULL"));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `decoder` is live.
        let handle = unsafe { &mut *decoder };
        match handle.inner.flush() {
            Ok(frames) => {
                handle.pending.extend(frames);
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

/// Convenience: builds a packet handle from raw bytes and a free function so the
/// caller does not have to keep both APIs in mind. Equivalent to
/// [`super::media::anycodec_packet_new`] followed by [`anycodec_decoder_push`].
///
/// # Safety
///
/// `decoder` must be a live decoder handle and `data` must point at `len`
/// readable bytes (or be NULL when `len` is `0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_decoder_push_bytes(
    decoder: *mut AnyCodecDecoder,
    data: *const std::os::raw::c_uchar,
    len: usize,
    pts: i64,
    flags: u32,
) -> i32 {
    guard::guard_status(|| {
        if decoder.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_decoder_push_bytes: decoder is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees readability, and the decoder's codec is
        // reused so the packet type always matches.
        let codec = unsafe { (*decoder).inner.codec() };
        // SAFETY: as documented.
        let Some(bytes) = (unsafe { read_bytes(data, len) }) else {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_decoder_push_bytes: data is NULL but len is non-zero",
            ));
            return STATUS_INVALID_ARGUMENT;
        };
        let packet = Packet::new(codec, Buffer::from_slice(bytes), pts, flags);
        // SAFETY: `decoder` is live.
        match unsafe { (*decoder).inner.push(packet) } {
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

/// Type alias kept so `Frame` stays part of this module's documentation surface.
#[allow(dead_code)]
type DecodedFrame = Frame;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::error;
    use crate::ffi::types::*;

    #[test]
    fn create_with_null_config_fails_cleanly() {
        error::clear();
        // SAFETY: NULL is explicitly handled.
        let decoder = unsafe { anycodec_decoder_new(std::ptr::null(), ANYCODEC_PREFERENCE_AUTO) };
        assert!(decoder.is_null());
        assert_eq!(error::last_status(), STATUS_INVALID_ARGUMENT);
    }

    #[test]
    fn create_with_an_unknown_codec_fails_cleanly() {
        error::clear();
        let config = AnyCodecDecoderConfig { codec: 4242, ..Default::default() };
        // SAFETY: `config` is a valid local.
        let decoder = unsafe { anycodec_decoder_new(&config, ANYCODEC_PREFERENCE_AUTO) };
        assert!(decoder.is_null());
        assert_eq!(error::last_status(), STATUS_INVALID_ARGUMENT);
    }

    #[test]
    fn null_accessors_are_safe() {
        // SAFETY: NULL is explicitly handled by every accessor.
        unsafe {
            assert_eq!(anycodec_decoder_codec(std::ptr::null()), 0);
            anycodec_decoder_free(std::ptr::null_mut());
            assert_eq!(anycodec_decoder_flush(std::ptr::null_mut()), STATUS_INVALID_ARGUMENT);
            assert_eq!(
                anycodec_decoder_push(std::ptr::null_mut(), std::ptr::null()),
                STATUS_INVALID_ARGUMENT
            );
        }
    }

    #[test]
    fn extradata_is_read_into_a_buffer() {
        let extradata = [1u8, 2, 3, 4];
        let config = AnyCodecDecoderConfig {
            codec: ANYCODEC_CODEC_H264,
            extradata: extradata.as_ptr(),
            extradata_len: extradata.len(),
            annex_b: 1,
            ..Default::default()
        };
        let converted = config_from_c(&config).unwrap();
        assert_eq!(converted.extradata.as_slice(), &extradata);
        assert!(converted.annex_b);
    }

    #[test]
    fn null_extradata_with_a_length_is_rejected() {
        let config = AnyCodecDecoderConfig {
            codec: ANYCODEC_CODEC_H264,
            extradata: std::ptr::null(),
            extradata_len: 8,
            ..Default::default()
        };
        assert!(config_from_c(&config).is_err());
    }

    #[test]
    fn zero_means_backend_default() {
        let config = AnyCodecDecoderConfig { codec: ANYCODEC_CODEC_AAC, ..Default::default() };
        let converted = config_from_c(&config).unwrap();
        assert!(converted.pixel_format.is_none());
        assert!(converted.sample_format.is_none());
        assert!(converted.channels.is_none());
    }
}
