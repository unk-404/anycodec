//! Android platform backend.
//!
//! Drives the NDK's `AMediaCodec` through the `ndk` crate's memory-safe wrapper
//! (`ndk::media::MediaCodec`) instead of re-declaring the NDK C API. What this
//! module implements itself is the part that makes AnyCodec what it is: which
//! codecs to route, how a raw [`Frame`](crate::media::Frame) is laid out for a
//! flexible-YUV encoder, how decoded buffers are turned back into frames, and how
//! hardware versus software is reported.
//!
//! ## What Android can and cannot do
//!
//! | Codec | Encoder | Decoder |
//! |-------|---------|---------|
//! | H.264, H.265 | device dependent (`c2.*` / `OMX.*`) | device dependent |
//! | H.266 | no Android encoder exists | decoder only where the vendor added one |
//! | VP8, VP9, AV1 | device dependent | device dependent |
//! | AAC | yes (`audio/mp4a-latm`) | yes |
//! | MP3, Vorbis, Opus, FLAC | rarely | usually |
//! | ALAC | no | some devices |
//!
//! So every registration is decided by actually asking MediaCodec for a codec of
//! that MIME type, and the capability report names the component Android handed
//! back. There is no static table claiming support a device may not have.

pub mod media_codec;

use crate::engine::{BackendAvailability, CodecKind, HwCodecSupport, HwDeviceInfo, Registry};
use crate::media::codec::Codec;

/// Registers every codec MediaCodec can actually serve on this device.
pub fn register(registry: &mut Registry) {
    media_codec::register(registry);
}

/// `true` when `libmediandk` resolved and MediaCodec accepts a trivial query.
#[must_use]
pub fn available() -> bool {
    media_codec::framework_available()
}

/// Probes the device's codec components.
///
/// Creates a throwaway `AMediaCodec` per codec and direction, which is the only
/// way Android exposes "does this device have it". Blocking; call off the UI
/// thread.
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    media_codec::probe_devices()
}

/// The probe shared by every registration in this module.
///
/// Puts the decision in one place: a codec is available exactly when
/// `AMediaCodec_createEncoderByType` / `createDecoderByType` returns a component.
pub(crate) fn probe(codec: Codec, kind: CodecKind) -> BackendAvailability {
    media_codec::probe_component(codec, kind)
}

/// What Android reports about one codec.
pub(crate) fn support_for(codec: Codec) -> HwCodecSupport {
    HwCodecSupport {
        codec,
        decode: media_codec::component_name(codec, CodecKind::Decoder).is_some(),
        encode: media_codec::component_name(codec, CodecKind::Encoder).is_some(),
        max_width: 0,
        max_height: 0,
        max_bit_depth: 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::codec::MediaKind;

    #[test]
    fn probing_never_panics() {
        let devices = probe_devices();
        assert!(devices.len() < 64, "implausible device count: {}", devices.len());
    }

    #[test]
    fn registration_never_panics() {
        // On an emulator with no codecs every lookup fails; `register` must
        // still produce a complete, well-formed report.
        let mut registry = Registry::new();
        register(&mut registry);
        assert_eq!(registry.capabilities().len(), Codec::ALL.len());
    }

    #[test]
    fn the_probe_explains_itself() {
        for codec in Codec::ALL {
            for kind in CodecKind::ALL {
                if let BackendAvailability::Unavailable(reason) = probe(codec, kind) {
                    assert!(reason.contains(codec.name()), "{codec}: {reason}");
                    assert!(reason.len() > 20, "{codec}: {reason}");
                }
            }
        }
    }

    #[test]
    fn support_is_derived_from_the_component_names() {
        let support = support_for(Codec::Video(crate::media::codec::VideoCodec::H264));
        assert_eq!(support.codec.name(), "h264");
        assert_eq!(support.codec.kind(), MediaKind::Video);
    }
}
