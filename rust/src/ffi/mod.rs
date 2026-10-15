//! Layer 4 鈥?the C ABI.
//!
//! This is the only module in the crate that exports `extern "C"`, and it is the
//! only module that owns raw pointers. Its whole job is to convert between the
//! flat `#[repr(C)]` world on one side and the owned, reference-counted values of
//! [`crate::media`] and the trait objects of [`crate::engine`] on the other.
//!
//! ## Rules this layer guarantees
//!
//! * **No panic escapes.** Every exported function is wrapped in
//!   [`guard::guard`], which catches unwinding panics and turns them into
//!   [`error::STATUS_INTERNAL`].
//! * **No null dereference.** Every pointer argument is validated before use.
//! * **One owner per allocation.** Every `*_new` has a matching `*_free`; every
//!   pointer returned through an out-parameter is documented as caller-owned.
//! * **No Rust allocation crosses to the caller.** Strings are copied into a
//!   caller-provided buffer, or returned as thread-local pointers that stay valid
//!   until the next AnyCodec call on the same thread.
//! * **ABI stability.** [`crate::ANYCODEC_ABI_VERSION`] is checked by the Dart layer; the
//!   layout of every `#[repr(C)]` type in [`types`] is frozen.

pub mod caps;
pub mod codec_path;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod guard;
pub mod handles;
pub mod media;
pub mod passthrough;
pub mod types;
pub mod version;

pub use caps::*;
pub use codec_path::*;
pub use decoder::*;
pub use encoder::*;
pub use error::*;
pub use handles::*;
pub use media::*;
pub use passthrough::*;
pub use types::*;
pub use version::*;

#[cfg(test)]
mod header_tests {
    /// The generated header, baked in at compile time.
    ///
    /// `rust/include/anycodec.h` is checked in (so that consumers and the ffigen
    /// binding build do not need cbindgen), which means it can drift from the
    /// Rust that produces it. These tests make that impossible to miss.
    const HEADER: &str = include_str!("../../include/anycodec.h");

    /// Every `#define` in the header.
    fn defines() -> Vec<&'static str> {
        HEADER
            .lines()
            .filter_map(|line| line.strip_prefix("#define "))
            .filter_map(|rest| rest.split_whitespace().next())
            .collect()
    }

    #[test]
    fn the_header_defines_only_abi_symbols() {
        // cbindgen emits *every* `pub const` in the crate and honours neither
        // `include` nor `exclude` for constants, so a new public constant
        // anywhere in the crate lands in the C ABI unless it is `pub(crate)`.
        // This is the tripwire for that.
        for name in defines() {
            assert_eq!(name, name.trim());
            assert!(
                name == "ANYCODEC_H" || name.starts_with("ANYCODEC_") || name.starts_with("STATUS_"),
                "`{name}` leaked into the C header; internal constants must be `pub(crate)`"
            );
        }
    }

    #[test]
    fn the_header_defines_nothing_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for name in defines() {
            assert!(seen.insert(name), "`{name}` is defined twice in the header");
        }
        assert!(!seen.is_empty(), "the header has no defines at all");
    }

    #[test]
    fn the_header_exposes_the_whole_surface() {
        for needed in [
            "anycodec_abi_version",
            "anycodec_encoder_new",
            "anycodec_encoder_push_video",
            "anycodec_encoder_push_audio",
            "anycodec_encoder_receive",
            "anycodec_decoder_new",
            "anycodec_decoder_receive",
            "anycodec_frame_video_info",
            "anycodec_frame_audio_info",
            "anycodec_capabilities",
            "anycodec_capability_backends",
            "anycodec_probe_devices",
            "anycodec_passthrough_encoder_new",
            "anycodec_last_error_message",
        ] {
            assert!(HEADER.contains(needed), "the header is missing `{needed}`");
        }
    }

    #[test]
    fn the_header_is_c_and_cpp_safe() {
        assert!(HEADER.contains("extern \"C\""), "the header must be usable from C++");
        assert!(HEADER.contains("#include <stdint.h>"), "fixed-width types are required");
        assert!(HEADER.contains("ANYCODEC_H"), "the include guard is missing");
    }
}
