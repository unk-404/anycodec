//! # AnyCodec
//!
//! One audio/video codec engine that prefers the platform's own media framework
//! and always keeps a software fallback available.
//!
//! ## Layers
//!
//! The crate is deliberately layered so that each concern can be read, tested
//! and replaced on its own:
//!
//! | Layer | Module | Responsibility |
//! |-------|--------|----------------|
//! | 0 | [`error`] | The one error type every layer speaks. |
//! | 1 | [`media`] | **Memory-safe model.** Owned, reference-counted, immutable media types: [`media::Buffer`], [`media::Frame`], [`media::Packet`], codec/format identifiers and codec configuration. No `unsafe`, no raw pointers, no lifetimes. |
//! | 2 | [`engine`] | **Backend abstraction.** [`engine::Encoder`] / [`engine::Decoder`] traits, [`engine::CodecPath`] (the "am I on software?" report), and the [`engine::registry`] that picks a backend. |
//! | 3 | [`native`] | **Platform frameworks.** MediaCodec, Media Foundation, VideoToolbox, AudioToolbox, GStreamer. |
//! | 3 | [`soft`] | **Software fallback.** Pure-Rust codecs and a runtime-loaded `libavcodec`. |
//! | 4 | [`ffi`] | **C ABI.** `#[repr(C)]` structs, opaque `Box`-ed handles, panic guards. This is the only module that exports `extern "C"`. |
//!
//! Data only ever flows down the layers as values the lower layer owns; no
//! pointer allocated above the FFI boundary is ever freed below it.
//!
//! ## Which backend did my encode use?
//!
//! Every live encoder/decoder answers [`engine::Encoder::path`] /
//! [`engine::Decoder::path`] with an [`engine::CodecPath`]. When the encode ran
//! on a software fallback, [`engine::CodecPath::is_software`] is `true` and
//! [`engine::CodecPath::fallback_reason`] explains which platform backend was
//! skipped and why.

#![deny(unsafe_op_in_unsafe_fn)]
// `expect_err` needs the *Ok* type to be `Debug`, and several `Result`s in this
// crate carry `Box<dyn Encoder>` / `Box<dyn Decoder>`, which are not. The tests
// therefore assert on `.err().expect(..)`, which is exactly what clippy's
// `err_expect` lint dislikes. Replacing it with `expect_err` is not possible
// without hand-writing `Debug` impls for the trait objects.
#![allow(clippy::err_expect)]
#![warn(missing_debug_implementations)]

pub mod error;
pub mod media;

pub mod engine;

pub mod native;
pub mod soft;

pub mod ffi;

/// The crate version, as reported by the C ABI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The C ABI revision. Bumped whenever the layout or the meaning of an exported
/// symbol changes; the Dart layer refuses to bind to a mismatching major.
/// The C ABI revision. Bumped whenever the layout or the meaning of an exported
/// symbol changes; the Dart layer refuses to bind to a mismatching major.
///
/// Deliberately prefixed `ANYCODEC_` so that it lands in the C header in the ABI
/// namespace instead of as a bare `ABI_VERSION` that a consumer might mistake for
/// something generic.
pub const ANYCODEC_ABI_VERSION: u32 = 1;

/// A one-line-per-codec human readable capability summary. Handy in logs and in
/// the example app.
pub fn capability_summary() -> String {
    let reports = engine::registry().capabilities();
    let mut out = String::new();
    for report in reports {
        out.push_str(&format!(
            "{:<7} {:<6} encoders=[{}] decoders=[{}]\n",
            report.codec.name(),
            if report.codec.is_video() { "video" } else { "audio" },
            render_backends(&report.encoders),
            render_backends(&report.decoders),
        ));
    }
    out
}

fn render_backends(backends: &[engine::BackendInfo]) -> String {
    backends
        .iter()
        .map(|b| {
            format!(
                "{}:{}{}",
                b.backend.name(),
                if b.available { "yes" } else { "no" },
                if b.hardware { "/hw" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_populated() {
        assert!(!VERSION.is_empty());
        assert_eq!(ANYCODEC_ABI_VERSION, 1);
    }

    #[test]
    fn capability_summary_covers_every_codec() {
        let summary = capability_summary();
        for codec in media::Codec::ALL {
            assert!(summary.contains(codec.name()), "missing {codec}");
        }
    }
}
