//! Tier 1 of the software fallback: codecs compiled into the binary.
//!
//! These are pure Rust and depend on nothing outside the process, so they work
//! even on a device with no media framework at all — a browser-less web worker, a
//! stripped-down Linux container, a Windows machine without the HEVC extensions.
//!
//! * [`pure`] — the real codec set: pure-Rust **decode and encode** for MP3,
//!   AAC, FLAC, Opus, VP9, H.264, H.265 and ALAC, one crate per family. This is
//!   the tier that makes the software fallback a codec set rather than a
//!   single-format safety net.
//! * [`symphonia_dec`] — a second pure-Rust **decoder** set for MP3, AAC,
//!   Vorbis, FLAC and ALAC. Vorbis lives only here.
//! * [`flac_enc`] — the `flacenc` pure-Rust **FLAC encoder**, kept as the
//!   fallback behind [`pure::flac`]'s `rusty_flac` encoder.
//! * [`passthrough`] — a **diagnostic** codec. It stores and retrieves frames
//!   verbatim so timestamps, buffer ownership, isolate hand-off and flush
//!   semantics can be validated with no codec involved. It is deliberately *not*
//!   registered against any of the twelve production codecs.
//!
//! # Overlapping implementations
//!
//! MP3, AAC, FLAC and ALAC are decoded by two different pure-Rust crates in this
//! build, and FLAC is encoded by two. That is deliberate: both are real
//! implementations, the capability report is honest to name both, and the
//! registry tries [`pure`] first so the richer codec wins and the older one
//! becomes a genuine fallback.

use crate::engine::Registry;
use crate::media::codec::Codec;

pub mod flac_enc;
pub mod passthrough;
pub mod pure;
pub mod symphonia_dec;

/// Registers every built-in codec, best first.
///
/// The diagnostic codec is intentionally not registered here; reach it through
/// its dedicated C ABI entry points (`anycodec_passthrough_encoder_new`).
///
/// Order matters: entries at the same priority are tried in registration order,
/// so [`pure`] is registered before [`symphonia_dec`] and [`flac_enc`] to make
/// the newer codecs primary and the older ones the fallback.
pub fn register(registry: &mut Registry) {
    pure::register(registry);
    symphonia_dec::register(registry);
    flac_enc::register(registry);
}

/// The codecs the built-in tier can decode, in [`Codec::ALL`] order.
pub fn decodable_codecs() -> Vec<Codec> {
    let mut codecs = pure::decodable_codecs();
    for codec in symphonia_dec::registered_codecs() {
        if !codecs.contains(&codec) {
            codecs.push(codec);
        }
    }
    in_codec_order(&mut codecs);
    codecs
}

/// The codecs the built-in tier can encode, in [`Codec::ALL`] order.
pub fn encodable_codecs() -> Vec<Codec> {
    let mut codecs = pure::encodable_codecs();
    for codec in flac_enc::registered_codecs() {
        if !codecs.contains(&codec) {
            codecs.push(codec);
        }
    }
    in_codec_order(&mut codecs);
    codecs
}

/// Sorts `codecs` into [`Codec::ALL`] order.
///
/// A codec that is not in [`Codec::ALL`] cannot exist, but sorting a stray one
/// to the end is cheaper than an `unwrap` in a function the capability report
/// calls.
fn in_codec_order(codecs: &mut [Codec]) {
    codecs.sort_by_key(|codec| {
        Codec::ALL.iter().position(|known| known == codec).unwrap_or(usize::MAX)
    });
}

/// A one-line summary of the built-in tier.
pub fn summary() -> String {
    format!(
        "{} decode[{}] encode[{}]",
        pure::summary(),
        decodable_codecs().iter().map(|codec| codec.name()).collect::<Vec<_>>().join(","),
        encodable_codecs().iter().map(|codec| codec.name()).collect::<Vec<_>>().join(","),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_names_every_codec_the_tier_serves() {
        let summary = summary();
        for codec in decodable_codecs() {
            assert!(summary.contains(codec.name()), "{codec} missing from {summary}");
        }
        for codec in encodable_codecs() {
            assert!(summary.contains(codec.name()), "{codec} missing from {summary}");
        }
    }

    #[test]
    fn the_codec_lists_are_deduplicated_and_ordered() {
        for codecs in [decodable_codecs(), encodable_codecs()] {
            let mut sorted = codecs.clone();
            in_codec_order(&mut sorted);
            assert_eq!(codecs, sorted, "the lists must follow Codec::ALL order");
            for (index, codec) in codecs.iter().enumerate() {
                assert!(!codecs[index + 1..].contains(codec), "{codec} is listed twice");
            }
        }
    }

    #[test]
    fn vorbis_is_decodable_but_not_encodable() {
        let vorbis = Codec::Audio(crate::media::codec::AudioCodec::Vorbis);
        assert!(decodable_codecs().contains(&vorbis));
        assert!(!encodable_codecs().contains(&vorbis));
    }

    #[test]
    fn nothing_claims_the_codecs_this_build_has_no_codec_for() {
        for codec in [
            Codec::Video(crate::media::codec::VideoCodec::H264),
            Codec::Video(crate::media::codec::VideoCodec::H266),
            Codec::Video(crate::media::codec::VideoCodec::Vp8),
            Codec::Video(crate::media::codec::VideoCodec::Av1),
        ] {
            assert!(!decodable_codecs().contains(&codec), "{codec} must not be claimed");
            assert!(!encodable_codecs().contains(&codec), "{codec} must not be claimed");
        }
    }
}
