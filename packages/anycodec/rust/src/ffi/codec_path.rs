//! The C representation of [`crate::engine::CodecPath`]: the answer to
//! "did I get hardware, a platform framework, or a software fallback?".

use crate::engine::backend::CodecPath;

use super::types::{
    ANYCODEC_DETAIL_CAPACITY, ANYCODEC_REASON_CAPACITY, write_cstring,
};

/// How a live encoder or decoder is doing its work.
///
/// This is the single struct a caller needs in order to know whether an operation
/// landed on a software fallback: check [`is_software`](Self::is_software) or
/// compare `implementation` against `ANYCODEC_IMPLEMENTATION_SOFTWARE`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AnyCodecCodecPath {
    /// One of the `ANYCODEC_BACKEND_*` constants.
    pub backend: u32,
    /// `ANYCODEC_IMPLEMENTATION_NATIVE` or `ANYCODEC_IMPLEMENTATION_SOFTWARE`.
    pub implementation: u32,
    /// `1` only when the backend is known to run on dedicated hardware.
    pub hardware_accelerated: u32,
    /// `1` when a software fallback is doing the work.
    pub is_software: u32,
    /// The concrete object in use, e.g. `"VTCompressionSession h264"`.
    pub detail: [std::os::raw::c_char; ANYCODEC_DETAIL_CAPACITY],
    /// Why a higher-priority backend was skipped. Empty when nothing was skipped.
    pub fallback_reason: [std::os::raw::c_char; ANYCODEC_REASON_CAPACITY],
}

impl Default for AnyCodecCodecPath {
    fn default() -> Self {
        Self {
            backend: 0,
            implementation: 0,
            hardware_accelerated: 0,
            is_software: 0,
            detail: [0; ANYCODEC_DETAIL_CAPACITY],
            fallback_reason: [0; ANYCODEC_REASON_CAPACITY],
        }
    }
}

impl std::fmt::Debug for AnyCodecCodecPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyCodecCodecPath")
            .field("backend", &self.backend)
            .field("implementation", &self.implementation)
            .field("hardware_accelerated", &self.hardware_accelerated)
            .field("is_software", &self.is_software)
            .field("detail", &self.detail_str())
            .field("fallback_reason", &self.fallback_reason_str())
            .finish()
    }
}

impl AnyCodecCodecPath {
    /// The `detail` field as a string.
    pub fn detail_str(&self) -> std::borrow::Cow<'_, str> {
        // SAFETY: the array is always NUL terminated by `From<&CodecPath>`.
        unsafe { std::ffi::CStr::from_ptr(self.detail.as_ptr()) }.to_string_lossy()
    }

    /// The `fallback_reason` field as a string.
    pub fn fallback_reason_str(&self) -> std::borrow::Cow<'_, str> {
        // SAFETY: the array is always NUL terminated by `From<&CodecPath>`.
        unsafe { std::ffi::CStr::from_ptr(self.fallback_reason.as_ptr()) }.to_string_lossy()
    }
}

impl From<&CodecPath> for AnyCodecCodecPath {
    fn from(path: &CodecPath) -> Self {
        let mut out = Self {
            backend: path.backend.raw(),
            implementation: path.implementation.raw(),
            hardware_accelerated: u32::from(path.hardware_accelerated),
            is_software: u32::from(path.is_software()),
            ..Default::default()
        };
        write_cstring(&mut out.detail, &path.detail);
        write_cstring(
            &mut out.fallback_reason,
            path.fallback_reason.as_deref().unwrap_or(""),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::backend::BackendKind;
    use super::super::types::{ANYCODEC_BACKEND_RUST, ANYCODEC_IMPLEMENTATION_SOFTWARE};

    #[test]
    fn software_path_is_visible_from_c() {
        let path = CodecPath::new(BackendKind::Rust, false, "rust: flac encoder")
            .with_fallback_reason("no Media Foundation FLAC encoder");
        let c = AnyCodecCodecPath::from(&path);
        assert_eq!(c.backend, ANYCODEC_BACKEND_RUST);
        assert_eq!(c.implementation, ANYCODEC_IMPLEMENTATION_SOFTWARE);
        assert_eq!(c.is_software, 1);
        assert_eq!(c.hardware_accelerated, 0);
        assert_eq!(c.detail_str(), "rust: flac encoder");
        assert!(c.fallback_reason_str().contains("Media Foundation"));
    }

    #[test]
    fn native_path_is_visible_from_c() {
        let path = CodecPath::new(BackendKind::VideoToolbox, true, "VTCompressionSession h264");
        let c = AnyCodecCodecPath::from(&path);
        assert_eq!(c.is_software, 0);
        assert_eq!(c.hardware_accelerated, 1);
        assert_eq!(c.fallback_reason_str(), "");
    }
}
