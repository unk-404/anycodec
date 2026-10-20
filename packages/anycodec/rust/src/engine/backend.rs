//! Backend identity, the "which implementation actually did the work" report,
//! and the policy that decides which backend a caller is willing to accept.
//!
//! The [`Encoder`](crate::engine::Encoder) / [`Decoder`](crate::engine::Decoder)
//! contracts live in [`crate::engine::traits`]; this module only describes
//! *identity* and *selection*.

/// Which subsystem performed the work.
///
/// The distinction that matters to callers is [`Implementation::Native`] versus
/// [`Implementation::Software`]: a software implementation is a *fallback*, so
/// callers that care about power/latency can refuse it. See [`CodecPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BackendKind {
    /// Android `AMediaCodec` (NDK C API).
    MediaCodec,
    /// Windows Media Foundation `IMFTransform` topologies.
    MediaFoundation,
    /// Apple VideoToolbox (`VTCompressionSession` / `VTDecompressionSession`).
    VideoToolbox,
    /// Apple AudioToolbox (`AudioConverter`).
    AudioToolbox,
    /// Linux GStreamer (`GstElement` pipelines).
    GStreamer,
    /// Browser WebCodecs (`VideoEncoder` / `AudioDecoder`, ...).
    ///
    /// Declared here for completeness of the capability report; the web
    /// implementation itself lives in the Dart layer.
    WebCodecs,
    /// Pure-Rust codecs compiled into `anycodec-soft`.
    Rust,
    /// `libavcodec` loaded at runtime through `dlopen`/`LoadLibrary`.
    Ffmpeg,
    /// An implementation registered by the embedding application.
    External,
}

impl BackendKind {
    /// Every backend, in report order.
    pub const ALL: [BackendKind; 9] = [
        BackendKind::MediaCodec,
        BackendKind::MediaFoundation,
        BackendKind::VideoToolbox,
        BackendKind::AudioToolbox,
        BackendKind::GStreamer,
        BackendKind::WebCodecs,
        BackendKind::Rust,
        BackendKind::Ffmpeg,
        BackendKind::External,
    ];

    /// Stable lower-case token.
    pub const fn name(self) -> &'static str {
        match self {
            BackendKind::MediaCodec => "mediacodec",
            BackendKind::MediaFoundation => "mediafoundation",
            BackendKind::VideoToolbox => "videotoolbox",
            BackendKind::AudioToolbox => "audiotoolbox",
            BackendKind::GStreamer => "gstreamer",
            BackendKind::WebCodecs => "webcodecs",
            BackendKind::Rust => "rust",
            BackendKind::Ffmpeg => "ffmpeg",
            BackendKind::External => "external",
        }
    }

    /// The codec path this backend implies.
    pub const fn implementation(self) -> Implementation {
        match self {
            BackendKind::Rust | BackendKind::Ffmpeg => Implementation::Software,
            _ => Implementation::Native,
        }
    }

    /// `true` when the backend is a platform media framework.
    pub const fn is_platform(self) -> bool {
        matches!(
            self,
            BackendKind::MediaCodec
                | BackendKind::MediaFoundation
                | BackendKind::VideoToolbox
                | BackendKind::AudioToolbox
                | BackendKind::GStreamer
                | BackendKind::WebCodecs
        )
    }

    /// `true` when the backend is a software fallback.
    pub const fn is_software(self) -> bool {
        matches!(self.implementation(), Implementation::Software)
    }

    /// `true` when the backend can run on dedicated hardware.
    ///
    /// This describes the backend's *capability*, not a particular session; the
    /// per-session truth is [`CodecPath::hardware_accelerated`].
    pub const fn is_hardware_capable(self) -> bool {
        matches!(
            self,
            BackendKind::MediaCodec
                | BackendKind::MediaFoundation
                | BackendKind::VideoToolbox
                | BackendKind::GStreamer
                | BackendKind::WebCodecs
        )
    }

    /// The `u32` bit used in capability bitmasks.
    pub const fn mask(self) -> u32 {
        1 << (self as u32)
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        self as u32
    }

    /// Parses a C ABI backend value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            0 => BackendKind::MediaCodec,
            1 => BackendKind::MediaFoundation,
            2 => BackendKind::VideoToolbox,
            3 => BackendKind::AudioToolbox,
            4 => BackendKind::GStreamer,
            5 => BackendKind::WebCodecs,
            6 => BackendKind::Rust,
            7 => BackendKind::Ffmpeg,
            8 => BackendKind::External,
            _ => return None,
        })
    }
}

/// Whether the work was done by a platform framework or by a software codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Implementation {
    /// A platform media framework (MediaCodec, Media Foundation, ...).
    Native,
    /// A software codec (pure Rust, or a dynamically loaded `libavcodec`).
    Software,
}

impl Implementation {
    /// Stable lower-case token.
    pub const fn name(self) -> &'static str {
        match self {
            Implementation::Native => "native",
            Implementation::Software => "software",
        }
    }

    /// `true` for [`Implementation::Software`]; i.e. "this is the fallback path".
    pub const fn is_software(self) -> bool {
        matches!(self, Implementation::Software)
    }

    /// `true` for [`Implementation::Native`].
    pub const fn is_native(self) -> bool {
        matches!(self, Implementation::Native)
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            Implementation::Native => 0,
            Implementation::Software => 1,
        }
    }
}

/// Describes exactly how a live encoder/decoder is doing its work.
///
/// This is the answer to "is this a software encode, and if so why?".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecPath {
    /// The subsystem performing the work.
    pub backend: BackendKind,
    /// Native framework or software fallback.
    pub implementation: Implementation,
    /// `true` only when the backend is *known* to be running on dedicated
    /// hardware (a GPU/ISP block). A platform framework that happens to use
    /// hardware internally reports `false` rather than guessing.
    pub hardware_accelerated: bool,
    /// Free-form, stable enough for logs and diagnostics: e.g.
    /// `"mediafoundation: CMSH264DecoderMFT"`.
    pub detail: String,
    /// Why the higher-priority backend was not used, when one was skipped.
    pub fallback_reason: Option<String>,
}

impl CodecPath {
    /// Builds a path with no fallback reason.
    pub fn new(
        backend: BackendKind,
        hardware_accelerated: bool,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            implementation: backend.implementation(),
            hardware_accelerated,
            detail: detail.into(),
            fallback_reason: None,
        }
    }

    /// Records why a higher-priority backend was skipped.
    #[must_use]
    pub fn with_fallback_reason(mut self, reason: impl Into<String>) -> Self {
        self.fallback_reason = Some(reason.into());
        self
    }

    /// Overrides the hardware flag.
    #[must_use]
    pub fn with_hardware(mut self, hardware: bool) -> Self {
        self.hardware_accelerated = hardware;
        self
    }

    /// `true` when a software fallback is doing the work.
    ///
    /// This is the single check an application needs in order to know whether it
    /// ended up on the fallback path.
    pub const fn is_software(&self) -> bool {
        matches!(self.implementation, Implementation::Software)
    }

    /// `true` when a platform framework is doing the work.
    pub const fn is_native(&self) -> bool {
        matches!(self.implementation, Implementation::Native)
    }
}

/// A short single-line rendering of a [`CodecPath`], used in errors and logs.
impl std::fmt::Display for CodecPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}{})",
            self.backend.name(),
            if self.is_software() { "software" } else { "native" },
            if self.hardware_accelerated { ", hardware" } else { "" }
        )?;
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

/// Which backends a caller is willing to accept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BackendPreference {
    /// Try platform frameworks first, then software fallbacks.
    #[default]
    Auto,
    /// Only platform frameworks; fail rather than fall back to software.
    NativeOnly,
    /// Only software codecs; never touch a platform framework.
    SoftwareOnly,
    /// Only implementations that are known to run on dedicated hardware.
    RequireHardware,
}

impl BackendPreference {
    /// `true` when a backend advertising `hardware` may be used.
    pub const fn accepts(self, native: bool, software: bool, hardware: bool) -> bool {
        match self {
            BackendPreference::Auto => true,
            BackendPreference::NativeOnly => native,
            BackendPreference::SoftwareOnly => software,
            BackendPreference::RequireHardware => hardware,
        }
    }

    /// Stable lower-case token.
    pub const fn name(self) -> &'static str {
        match self {
            BackendPreference::Auto => "auto",
            BackendPreference::NativeOnly => "native_only",
            BackendPreference::SoftwareOnly => "software_only",
            BackendPreference::RequireHardware => "require_hardware",
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            BackendPreference::Auto => 0,
            BackendPreference::NativeOnly => 1,
            BackendPreference::SoftwareOnly => 2,
            BackendPreference::RequireHardware => 3,
        }
    }

    /// Parses a C ABI preference value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            0 => BackendPreference::Auto,
            1 => BackendPreference::NativeOnly,
            2 => BackendPreference::SoftwareOnly,
            3 => BackendPreference::RequireHardware,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_raw_roundtrip() {
        for backend in BackendKind::ALL {
            assert_eq!(BackendKind::from_raw(backend.raw()), Some(backend));
        }
        assert_eq!(BackendKind::from_raw(9), None);
    }

    #[test]
    fn software_backends_are_classified() {
        assert!(BackendKind::Rust.is_software());
        assert!(BackendKind::Ffmpeg.is_software());
        assert!(!BackendKind::MediaFoundation.is_software());
        assert!(BackendKind::MediaFoundation.is_platform());
        assert!(!BackendKind::External.is_platform());
    }

    #[test]
    fn path_reports_software() {
        let path = CodecPath::new(BackendKind::Rust, false, "flac encoder")
            .with_fallback_reason("no Media Foundation FLAC encoder");
        assert!(path.is_software());
        assert!(!path.is_native());
        assert_eq!(path.fallback_reason.as_deref(), Some("no Media Foundation FLAC encoder"));
        assert!(path.to_string().contains("rust (software)"));
    }

    #[test]
    fn preference_raw_roundtrip() {
        for value in 0..4u32 {
            let preference = BackendPreference::from_raw(value).unwrap();
            assert_eq!(preference.raw(), value);
        }
        assert_eq!(BackendPreference::from_raw(4), None);
    }

    #[test]
    fn preference_filters_backends() {
        assert!(BackendPreference::Auto.accepts(true, true, false));
        assert!(!BackendPreference::NativeOnly.accepts(false, true, false));
        assert!(!BackendPreference::SoftwareOnly.accepts(true, false, false));
        assert!(BackendPreference::RequireHardware.accepts(true, false, true));
        assert!(!BackendPreference::RequireHardware.accepts(true, false, false));
    }
}
