//! Windows platform backend.
//!
//! Drives Media Foundation, the media framework Windows ships with: H.264
//! decoding is inbox everywhere, HEVC/VP9/AV1 arrive with the optional "Video
//! Extensions" store packages, and AAC/MP3/FLAC/ALAC support varies by Windows
//! build. None of that is guessable, so the capability report is built from real
//! `MFTEnumEx` enumeration (see `media_foundation::probe_component`).
//!
//! ## Hardware acceleration, honestly
//!
//! Media Foundation's hardware codecs are *asynchronous* MFTs, which need
//! `MF_TRANSFORM_ASYNC_UNLOCK` plus an `IMFMediaEventGenerator` pump, and the
//! D3D-backed ones additionally need a DXGI device manager. AnyCodec does not
//! build that pipeline, so
//! [`CodecPath::hardware_accelerated`](crate::engine::CodecPath::hardware_accelerated)
//! is `false` for every session from this backend and `detail` says "synchronous
//! MFT". Reporting `true` because Media Foundation *could* use the GPU would be
//! exactly the kind of unverifiable claim the codec path exists to prevent.

pub mod media_foundation;

use crate::engine::{HwDeviceInfo, Registry};

/// Registers every codec Media Foundation can serve on this machine.
pub fn register(registry: &mut Registry) {
    media_foundation::register(registry);
}

/// `true` when Media Foundation started.
#[must_use]
pub fn available() -> bool {
    media_foundation::framework_available()
}

/// Probes the Media Foundation codec categories.
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    media_foundation::probe_devices()
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probing_never_panics() {
        let devices = probe_devices();
        assert!(devices.len() <= 4, "implausible device count: {}", devices.len());
    }

    #[test]
    fn registration_produces_a_complete_report() {
        use crate::media::codec::Codec;
        let mut registry = Registry::new();
        register(&mut registry);
        assert_eq!(registry.capabilities().len(), Codec::ALL.len());
    }

    #[test]
    fn every_codec_answers_the_probe() {
        use crate::engine::{BackendAvailability, CodecKind};
        use crate::media::codec::Codec;
        for codec in Codec::ALL {
            for kind in CodecKind::ALL {
                // `Available`, or a reason: never silence and never a panic.
                match media_foundation::probe_component(codec, kind) {
                    BackendAvailability::Available => {}
                    BackendAvailability::Unavailable(reason) => {
                        assert!(!reason.is_empty(), "{codec} {kind:?}");
                    }
                }
            }
        }
    }
}
