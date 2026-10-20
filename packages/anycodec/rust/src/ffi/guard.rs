//! The panic firewall between Rust and C.
//!
//! Unwinding across an `extern "C"` boundary is undefined behaviour, so every
//! exported function runs inside one of these wrappers. A caught panic becomes
//! [`super::error::STATUS_INTERNAL`] plus a readable message on the thread-local
//! error slot, which is always better than aborting the host application.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::error;

/// Runs `body`, converting a panic into [`error::STATUS_INTERNAL`].
///
/// Returns `fallback` when a panic was caught.
pub fn guard<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            error::set_panic(payload.as_ref());
            fallback
        }
    }
}

/// Runs `body`, returning a status code; a panic becomes [`error::STATUS_INTERNAL`].
pub fn guard_status(body: impl FnOnce() -> i32) -> i32 {
    guard(error::STATUS_INTERNAL, body)
}

/// Runs `body`, returning a `*mut T`; a panic becomes a null pointer.
pub fn guard_ptr<T>(body: impl FnOnce() -> *mut T) -> *mut T {
    guard(std::ptr::null_mut(), body)
}

/// Runs `body`, returning a `*const c_char`; a panic becomes a null pointer.
pub fn guard_cstr(body: impl FnOnce() -> *const std::os::raw::c_char) -> *const std::os::raw::c_char {
    guard(std::ptr::null(), body)
}

/// Runs `body`, returning a value built by `Default` on panic.
pub fn guard_default<T: Default>(body: impl FnOnce() -> T) -> T {
    guard(T::default(), body)
}

/// Runs `body` for effect; a panic is swallowed after being recorded.
///
/// Only for `*_free` style functions, where there is nothing to report to and
/// leaking is preferable to aborting.
pub fn guard_void(body: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(body)).map_err(|payload| {
        error::set_panic(payload.as_ref());
    });
}

/// Turns a `Result` into a status code, or calls `on_ok` when it succeeded.
pub fn status_of<T>(result: crate::error::Result<T>, on_ok: impl FnOnce(T) -> i32) -> i32 {
    match result {
        Ok(value) => on_ok(value),
        Err(error) => {
            error::set_error(&error);
            error.status()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_becomes_a_status_instead_of_unwinding() {
        error::clear();
        let status = guard_status(|| panic!("deliberate"));
        assert_eq!(status, error::STATUS_INTERNAL);
        assert_eq!(error::last_status(), error::STATUS_INTERNAL);
    }

    #[test]
    fn a_panic_becomes_a_null_pointer() {
        error::clear();
        let pointer: *mut u8 = guard_ptr(|| panic!("deliberate"));
        assert!(pointer.is_null());
    }

    #[test]
    fn successes_pass_through() {
        error::clear();
        assert_eq!(guard_status(|| 7), 7);
        assert_eq!(guard_default(|| 3u8), 3);
    }

    #[test]
    fn result_to_status_reports_errors() {
        error::clear();
        let status = status_of::<()>(
            Err(crate::error::Error::unsupported("nope")),
            |_| error::STATUS_OK,
        );
        assert_eq!(status, error::STATUS_UNSUPPORTED);
    }
}
