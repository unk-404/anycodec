//! The diagnostic `passthrough` codec, exposed through its own entry points.
//!
//! The twelve production codecs go through the registry. This one deliberately
//! does not: it neither compresses nor decompresses, so registering it against a
//! real codec would make the capability report lie. Instead it has its own
//! constructors, and the Dart layer exposes it as `AnyCodecDiagnostics`.
//!
//! It exists so that timestamps, buffer ownership, isolate hand-off, flush
//! semantics and the whole FFI path can be validated on a machine with no media
//! framework and no FFmpeg installed.

use crate::media::codec::Codec;
use crate::soft::builtin::passthrough::{PassthroughDecoder, PassthroughEncoder};

use super::error::{set_error, STATUS_INVALID_ARGUMENT};
use super::guard;
use super::handles::{AnyCodecDecoder, AnyCodecEncoder, into_decoder_handle, into_encoder_handle};

/// Creates a verbatim store encoder for `codec`.
///
/// Returns NULL and sets the thread-local error when `codec` is not a known codec
/// id.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_passthrough_encoder_new(codec: u32) -> *mut AnyCodecEncoder {
    guard::guard_ptr(|| {
        let Some(codec) = Codec::from_raw(codec) else {
            set_error(&crate::error::Error::invalid_argument(format!(
                "anycodec_passthrough_encoder_new: unknown codec id {codec}"
            )));
            return std::ptr::null_mut();
        };
        super::error::clear();
        into_encoder_handle(Box::new(PassthroughEncoder::new(codec)))
    })
}

/// Creates a verbatim retrieve decoder for `codec`.
///
/// Returns NULL and sets the thread-local error when `codec` is not a known codec
/// id.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_passthrough_decoder_new(codec: u32) -> *mut AnyCodecDecoder {
    guard::guard_ptr(|| {
        let Some(codec) = Codec::from_raw(codec) else {
            set_error(&crate::error::Error::invalid_argument(format!(
                "anycodec_passthrough_decoder_new: unknown codec id {codec}"
            )));
            return std::ptr::null_mut();
        };
        super::error::clear();
        into_decoder_handle(Box::new(PassthroughDecoder::new(codec)))
    })
}

/// Returns the codec id the diagnostic codec advertises when it is created
/// through the registry-facing API.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_passthrough_codec_id() -> u32 {
    crate::soft::builtin::passthrough::PASSTHROUGH_CODEC.raw()
}

/// A status constant re-exported so C users do not have to remember it.
pub const ANYCODEC_PASSTHROUGH_INVALID_CODEC: i32 = STATUS_INVALID_ARGUMENT;

#[cfg(test)]
mod tests {
    use super::super::error;
    use super::super::types::ANYCODEC_CODEC_H264;
    use super::*;

    #[test]
    fn an_unknown_codec_is_rejected() {
        error::clear();
        assert!(anycodec_passthrough_encoder_new(0xbeef).is_null());
        assert_eq!(error::last_status(), STATUS_INVALID_ARGUMENT);
        assert!(anycodec_passthrough_decoder_new(0xbeef).is_null());
    }

    #[test]
    fn a_known_codec_produces_a_handle() {
        error::clear();
        let encoder = anycodec_passthrough_encoder_new(ANYCODEC_CODEC_H264);
        assert!(!encoder.is_null());
        // SAFETY: the handle came from the function above and is freed once.
        unsafe { super::super::encoder::anycodec_encoder_free(encoder) };

        let decoder = anycodec_passthrough_decoder_new(ANYCODEC_CODEC_H264);
        assert!(!decoder.is_null());
        // SAFETY: as above.
        unsafe { super::super::decoder::anycodec_decoder_free(decoder) };
    }

    #[test]
    fn the_diagnostic_codec_reports_the_rust_backend() {
        let encoder = anycodec_passthrough_encoder_new(ANYCODEC_CODEC_H264);
        let mut path = super::super::codec_path::AnyCodecCodecPath::default();
        // SAFETY: `encoder` is live and `path` is a valid local.
        unsafe {
            assert_eq!(
                super::super::encoder::anycodec_encoder_path(encoder, &mut path),
                0
            );
            assert_eq!(path.is_software, 1);
            assert!(path.detail_str().contains("passthrough"));
            super::super::encoder::anycodec_encoder_free(encoder);
        }
    }
}
