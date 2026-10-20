//! The `libavcodec` software fallback, loaded at runtime.
//!
//! Nothing here is a build-time dependency: `libavcodec` is resolved with
//! `libloading` when the first software codec is requested, and every failure to
//! find or initialise it becomes a specific [`crate::error::Error::unavailable`]
//! message that ends up in `CodecPath::fallback_reason`.

use crate::engine::backend::BackendKind;
use crate::engine::{BackendAvailability, CodecKind, Registry};
use crate::error::Error;
use crate::media::codec::Codec;
use crate::media::config::{DecoderConfig, EncoderConfig};

pub mod loader;

/// Priority of the FFmpeg tier: below every built-in, above nothing else.
pub const PRIORITY: u8 = super::PRIORITY_FFMPEG;

/// Registers a `libavcodec`-backed entry for every codec AnyCodec routes.
///
/// Registration always succeeds and the entries are always *listed*, because the
/// capability report has to be able to say "there is a general software fallback
/// here, and this is why it is not usable right now". Each entry's probe and
/// factory carry exactly the reason [`unavailable_reason`] produced, so a caller
/// reading `CodecPath::fallback_reason` gets the real story rather than a silent
/// gap.
pub fn register(registry: &mut Registry) {
    let reason = unavailable_reason();
    log::debug!("anycodec: ffmpeg tier: {reason}");

    for codec in Codec::ALL {
        // One `String` per closure: two closures each need their own copy.
        let encoder_probe_reason = reason.clone();
        let encoder_factory_reason = reason.clone();
        registry.register_encoder(
            codec,
            BackendKind::Ffmpeg,
            PRIORITY,
            move |_config: &EncoderConfig| Err(Error::unavailable(encoder_factory_reason.clone())),
            move |_codec: Codec| BackendAvailability::Unavailable(encoder_probe_reason.clone()),
        );

        let decoder_probe_reason = reason.clone();
        let decoder_factory_reason = reason.clone();
        registry.register_decoder(
            codec,
            BackendKind::Ffmpeg,
            PRIORITY,
            move |_config: &DecoderConfig| Err(Error::unavailable(decoder_factory_reason.clone())),
            move |_codec: Codec| BackendAvailability::Unavailable(decoder_probe_reason.clone()),
        );
    }
}

/// Why the `libavcodec` tier cannot serve a codec right now.
///
/// Distinguishes the two cases honestly: a missing library, versus a library that
/// was found but has no session wrapper in this build. Collapsing them would make
/// the message useless.
///
/// Kept to a single short line because the capability report shows it once per
/// codec *per direction*. To make this tier work, install the FFmpeg shared
/// libraries (`libavcodec`) next to the application, or on `PATH`.
#[must_use]
pub fn unavailable_reason() -> String {
    match loader::probe() {
        Ok(info) => format!(
            "libavcodec is present ({}) but this build has no session wrapper for it yet; \
             use a platform backend or the built-in pure-Rust codecs",
            info.describe()
        ),
        Err(reason) => reason,
    }
}

/// A one-line summary of the FFmpeg tier.
pub fn summary() -> String {
    unavailable_reason()
}

/// The codecs the FFmpeg tier is *designed* to serve, which is all of them.
#[must_use]
pub fn covered_codecs() -> Vec<Codec> {
    Codec::ALL.to_vec()
}

/// The directions the FFmpeg tier is designed to serve.
#[must_use]
pub fn covered_kinds() -> [CodecKind; 2] {
    CodecKind::ALL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_lists_every_codec_with_a_reason() {
        let mut registry = Registry::new();
        register(&mut registry);
        let reports = registry.capabilities();
        for report in &reports {
            for entries in [&report.encoders, &report.decoders] {
                let ffmpeg = entries
                    .iter()
                    .find(|entry| entry.backend == BackendKind::Ffmpeg)
                    .expect("the ffmpeg tier must be listed for every codec");
                assert!(!ffmpeg.available, "the session wrapper is not implemented yet");
                assert!(ffmpeg.note.len() > 40, "the reason must be actionable: {}", ffmpeg.note);
                assert!(
                    ffmpeg.note.contains("libavcodec"),
                    "the reason must name the library: {}",
                    ffmpeg.note
                );
            }
        }
    }

    #[test]
    fn the_reason_distinguishes_missing_from_unimplemented() {
        let reason = unavailable_reason();
        assert!(
            reason.contains("not found") || reason.contains("no session wrapper"),
            "unexpected reason: {reason}"
        );
    }

    #[test]
    fn the_tier_is_designed_to_cover_all_twelve() {
        assert_eq!(covered_codecs().len(), 12);
        assert_eq!(covered_kinds().len(), 2);
    }

    #[test]
    fn a_factory_failure_is_an_unavailable_error_not_a_panic() {
        let mut registry = Registry::new();
        register(&mut registry);
        // `Box<dyn Encoder>` is not `Debug`, so `.err().expect(..)` rather than
        // `.expect_err(..)`.
        let error = registry
            .create_encoder(
                &EncoderConfig::video(crate::media::codec::VideoCodec::H264, 64, 64),
                crate::engine::BackendPreference::SoftwareOnly,
            )
            .err()
            .expect("the ffmpeg tier cannot build a session yet");
        assert_eq!(error.kind(), crate::error::ErrorKind::Unavailable);
        assert!(error.message().contains("libavcodec"), "{}", error.message());
    }
}
