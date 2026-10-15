//! Layer 3b — the software fallback.
//!
//! Two tiers, in this order:
//!
//! 1. [`builtin`] — codecs compiled into the binary. Pure Rust, no `dlopen`, no
//!    system dependency, works on every target including ones with no media
//!    framework at all.
//! 2. [`ffmpeg`] — `libavcodec` loaded at runtime through `libloading`. This is
//!    the general-purpose fallback: it can serve *every* codec AnyCodec routes,
//!    but only when the host actually has FFmpeg's shared libraries. Nothing is
//!    required at build time.
//!
//! Both tiers register at priorities `>= 100`, so they only ever run after every
//! platform framework has been tried. A caller that must not silently fall back
//! passes [`crate::engine::BackendPreference::NativeOnly`].

pub mod builtin;
pub mod ffmpeg;

/// Priority of a pure-Rust built-in codec.
pub(crate) const PRIORITY_BUILTIN: u8 = 100;
/// Priority of a runtime-loaded `libavcodec`.
pub(crate) const PRIORITY_FFMPEG: u8 = 150;

/// Registers every software fallback this build was compiled with.
pub fn register(registry: &mut crate::engine::Registry) {
    builtin::register(registry);
    ffmpeg::register(registry);
}

/// A one-line summary of what the software tier can do, for logs and the
/// example app.
pub fn summary() -> String {
    format!("builtin: {} | ffmpeg: {}", builtin::summary(), ffmpeg::summary())
}
