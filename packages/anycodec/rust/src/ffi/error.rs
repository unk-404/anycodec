//! Status codes and the per-thread last-error channel.
//!
//! The C ABI has no way to return a rich error, so AnyCodec uses the classic
//! two-channel scheme:
//!
//! * the function's return value is a [`STATUS_*`] code, and
//! * the detail lives in a **thread-local** last-error slot that the caller reads
//!   with [`anycodec_last_error_status`] / [`anycodec_last_error_message`].
//!
//! Thread-local is the right scope here: a Dart worker isolate drives one native
//! session at a time on one thread, and the Dart layer never reads the error from
//! a different isolate than the one that produced it.

use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::c_char;

use crate::error::{Error, ErrorKind};

/// The call succeeded.
pub const STATUS_OK: i32 = 0;
/// Used only by `*_receive`: an output object was produced.
pub const STATUS_GOT_OUTPUT: i32 = 1;
/// Used only by `*_receive`: more input is required before output appears.
pub const STATUS_NEED_MORE_INPUT: i32 = 0;

/// A caller-supplied argument was malformed.
pub const STATUS_INVALID_ARGUMENT: i32 = -1;
/// The codec or operation is not implemented by this build.
pub const STATUS_UNSUPPORTED: i32 = -2;
/// Implemented, but no backend is currently usable.
pub const STATUS_UNAVAILABLE: i32 = -3;
/// The platform API reported a failure.
pub const STATUS_BACKEND: i32 = -4;
/// The object is not in a state that permits the call.
pub const STATUS_STATE: i32 = -5;
/// Allocation failed.
pub const STATUS_OUT_OF_MEMORY: i32 = -6;
/// A file or stream operation failed.
pub const STATUS_IO: i32 = -7;
/// An internal invariant was violated, or a panic was caught. This is a bug.
pub const STATUS_INTERNAL: i32 = -8;

/// Maps an [`ErrorKind`] onto its ABI status code.
pub const fn status_of(kind: ErrorKind) -> i32 {
    kind.status()
}

/// The C ABI name for a status code, for logging from the Dart side.
pub const fn status_name(status: i32) -> &'static str {
    match status {
        STATUS_OK => "ok",
        STATUS_INVALID_ARGUMENT => "invalid_argument",
        STATUS_UNSUPPORTED => "unsupported",
        STATUS_UNAVAILABLE => "unavailable",
        STATUS_BACKEND => "backend",
        STATUS_STATE => "state",
        STATUS_OUT_OF_MEMORY => "out_of_memory",
        STATUS_IO => "io",
        STATUS_INTERNAL => "internal",
        _ => "unknown",
    }
}

thread_local! {
    static LAST_ERROR: RefCell<Option<StoredError>> = const { RefCell::new(None) };
    /// A second slot used to hand a dynamically built string to C. Kept separate
    /// from `LAST_ERROR` so that building a summary does not clobber the error
    /// channel.
    static RETURN_STRING: RefCell<CString> =
        RefCell::new(CString::new("").expect("empty literal has no NUL"));
}

/// Stores `value` in this thread's return-string slot and returns a pointer to it.
///
/// The pointer stays valid until the next call to this function on the same
/// thread. Embedded NUL bytes are replaced with spaces.
pub fn set_and_return_string(value: &str) -> *const c_char {
    let sanitized = value.replace('\0', " ");
    let replacement =
        CString::new(sanitized).unwrap_or_else(|_| CString::new("").expect("literal"));
    RETURN_STRING.with(|slot| {
        let mut stored = slot.borrow_mut();
        *stored = replacement;
        stored.as_ptr()
    })
}

struct StoredError {
    status: i32,
    message: CString,
}

/// Records `error` as this thread's last error.
pub fn set_error(error: &Error) {
    let message = CString::new(error.to_string())
        .unwrap_or_else(|_| CString::new("error message contained a NUL byte").expect("literal"));
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = Some(StoredError { status: error.status(), message });
    });
}

/// Records a caught panic as this thread's last error.
pub fn set_panic(payload: &(dyn std::any::Any + Send)) {
    let detail = if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_string()
    };
    let error = Error::internal(format!("panic in native code: {detail}"));
    set_error(&error);
}

/// Records a bare message with an explicit status.
pub fn set_status(status: i32, message: impl AsRef<str>) {
    let sanitized = message.as_ref().replace('\0', " ");
    let message = CString::new(sanitized).unwrap_or_else(|_| CString::new("error").expect("literal"));
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = Some(StoredError { status, message });
    });
}

/// Clears this thread's last error.
pub fn clear() {
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

/// The status code of this thread's last error, or [`STATUS_OK`] when there is none.
pub fn last_status() -> i32 {
    LAST_ERROR.with(|slot| slot.borrow().as_ref().map_or(STATUS_OK, |e| e.status))
}

/// A pointer to this thread's last error message, or NULL when there is none.
///
/// The pointer is valid until the next AnyCodec call on the same thread. Copy it
/// if you need to keep it.
pub fn last_message_ptr() -> *const c_char {
    LAST_ERROR.with(|slot| match slot.borrow().as_ref() {
        Some(stored) => stored.message.as_ptr(),
        None => std::ptr::null(),
    })
}

// ---------------------------------------------------------------------------
// Exported entry points
// ---------------------------------------------------------------------------

/// Returns the status code of the last AnyCodec call on this thread.
///
/// `0` means the last call succeeded. The value is one of the `ANYCODEC_STATUS_*`
/// constants.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_last_error_status() -> i32 {
    last_status()
}

/// Returns the message of the last AnyCodec call on this thread, or NULL.
///
/// The returned pointer is owned by AnyCodec and is valid until the next AnyCodec
/// call on the same thread. Never free it.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_last_error_message() -> *const c_char {
    last_message_ptr()
}

/// Clears the last error on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_clear_error() {
    clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_match_the_error_kinds() {
        assert_eq!(status_of(ErrorKind::InvalidArgument), STATUS_INVALID_ARGUMENT);
        assert_eq!(status_of(ErrorKind::Unsupported), STATUS_UNSUPPORTED);
        assert_eq!(status_of(ErrorKind::Unavailable), STATUS_UNAVAILABLE);
        assert_eq!(status_of(ErrorKind::Backend), STATUS_BACKEND);
        assert_eq!(status_of(ErrorKind::State), STATUS_STATE);
        assert_eq!(status_of(ErrorKind::OutOfMemory), STATUS_OUT_OF_MEMORY);
        assert_eq!(status_of(ErrorKind::Io), STATUS_IO);
        assert_eq!(status_of(ErrorKind::Internal), STATUS_INTERNAL);
    }

    #[test]
    fn last_error_round_trips() {
        clear();
        assert_eq!(anycodec_last_error_status(), STATUS_OK);
        assert!(anycodec_last_error_message().is_null());

        set_error(&Error::unavailable("no HEVC decoder"));
        assert_eq!(anycodec_last_error_status(), STATUS_UNAVAILABLE);
        let message = last_message_ptr();
        assert!(!message.is_null());
        // SAFETY: the pointer is valid until the next AnyCodec call on this thread.
        let text = unsafe { std::ffi::CStr::from_ptr(message) }.to_string_lossy().into_owned();
        assert!(text.contains("no HEVC decoder"), "{text}");

        clear();
        assert_eq!(anycodec_last_error_status(), STATUS_OK);
    }

    #[test]
    fn panic_payload_becomes_an_internal_error() {
        clear();
        set_panic(&"boom");
        assert_eq!(last_status(), STATUS_INTERNAL);
        let message = last_message_ptr();
        // SAFETY: as above.
        let text = unsafe { std::ffi::CStr::from_ptr(message) }.to_string_lossy().into_owned();
        assert!(text.contains("boom"));
    }

    #[test]
    fn status_names_are_stable() {
        assert_eq!(status_name(STATUS_UNAVAILABLE), "unavailable");
        assert_eq!(status_name(12345), "unknown");
    }
}
