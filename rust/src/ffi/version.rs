//! Version and ABI identification.

use std::ffi::CStr;
use std::os::raw::c_char;

use super::types::AnyCodecVersion;

const VERSION_BYTES: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

static VERSION_CSTR: &CStr =
    // SAFETY: the byte string is a literal with exactly one trailing NUL, so the
    // invariant `from_bytes_with_nul_unchecked` requires holds by construction.
    unsafe { CStr::from_bytes_with_nul_unchecked(VERSION_BYTES) };

/// Returns the C ABI revision this library was built against.
///
/// The Dart layer compares this against the revision it was generated for and
/// refuses to proceed on a mismatch, which turns "mysterious memory corruption"
/// into a clear error message.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_abi_version() -> u32 {
    crate::ANYCODEC_ABI_VERSION
}

/// Writes the library version into `out`.
///
/// Returns `0` on success and `ANYCODEC_STATUS_INVALID_ARGUMENT` when `out` is NULL.
///
/// # Safety
///
/// `out` must be NULL or point to a writable [`AnyCodecVersion`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_version(out: *mut AnyCodecVersion) -> i32 {
    super::guard::guard_status(|| {
        if out.is_null() {
            let error = crate::error::Error::invalid_argument("anycodec_version: out is NULL");
            super::error::set_error(&error);
            return error.status();
        }
        let mut parts = [0u32; 3];
        for (slot, component) in parts.iter_mut().zip(VERSION_BYTES.split(|b| *b == b'.')) {
            *slot = std::str::from_utf8(component).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        }
        // SAFETY: `out` is non-null and the caller guarantees it is writable.
        unsafe {
            *out = AnyCodecVersion {
                abi: crate::ANYCODEC_ABI_VERSION,
                major: parts[0],
                minor: parts[1],
                patch: parts[2],
            };
        }
        super::error::clear();
        super::error::STATUS_OK
    })
}

/// Returns a pointer to a NUL-terminated version string.
///
/// The string is a compile-time constant: it is never freed and never changes.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_version_string() -> *const c_char {
    VERSION_CSTR.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_is_exposed() {
        assert_eq!(anycodec_abi_version(), crate::ANYCODEC_ABI_VERSION);
    }

    #[test]
    fn version_string_is_a_static_c_string() {
        let pointer = anycodec_version_string();
        assert!(!pointer.is_null());
        // SAFETY: the pointer is a static NUL-terminated string.
        let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
        assert_eq!(text, crate::VERSION);
    }

    #[test]
    fn version_out_parameter_is_written() {
        let mut version = AnyCodecVersion::default();
        // SAFETY: `version` is a valid local.
        let status = unsafe { anycodec_version(&mut version) };
        assert_eq!(status, 0);
        assert_eq!(version.abi, crate::ANYCODEC_ABI_VERSION);
        assert_eq!(version.major, 0, "the version starts at 0.x.y");
    }

    #[test]
    fn null_out_parameter_is_rejected() {
        // SAFETY: NULL is explicitly handled.
        let status = unsafe { anycodec_version(std::ptr::null_mut()) };
        assert_eq!(status, super::super::error::STATUS_INVALID_ARGUMENT);
    }
}
