//! On-demand hardware device probing.
//!
//! Deliberately *not* part of registration: enumerating MediaCodec, a
//! VideoToolbox media engine, or a VA-API display costs milliseconds and may
//! touch hardware, so it only runs when a caller explicitly asks
//! ([`crate::ffi::anycodec_probe_devices`]).
//!
//! The shape of this report is borrowed from OxideAV's `HwDeviceInfo`: one
//! record per hardware engine, one entry per codec the engine supports. AnyCodec
//! does not depend on any OxideAV crate, but their capability-reporting model is
//! a good one and is reused here as an API *idea* only.

use crate::media::codec::Codec;

/// One codec a hardware engine can handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HwCodecSupport {
    /// The codec.
    pub codec: Codec,
    /// `true` when the engine can decode it.
    pub decode: bool,
    /// `true` when the engine can encode it.
    pub encode: bool,
    /// Largest supported luma width, `0` when unknown.
    pub max_width: u32,
    /// Largest supported luma height, `0` when unknown.
    pub max_height: u32,
    /// Bits per component, `0` when unknown.
    pub max_bit_depth: u8,
}

/// One hardware media engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HwDeviceInfo {
    /// Which subsystem exposes the engine.
    pub backend: crate::engine::backend::BackendKind,
    /// Human readable engine name, e.g. `"Intel Iris Xe Media Engine"`.
    pub name: String,
    /// Version of the API used to talk to the engine.
    pub api_version: String,
    /// Driver version, when the platform exposes one.
    pub driver_version: String,
    /// What the engine can do.
    pub codecs: Vec<HwCodecSupport>,
}

impl HwDeviceInfo {
    /// Builds an empty engine record.
    pub fn new(
        backend: crate::engine::backend::BackendKind,
        name: impl Into<String>,
        api_version: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            name: name.into(),
            api_version: api_version.into(),
            driver_version: String::new(),
            codecs: Vec::new(),
        }
    }

    /// Looks up one codec's support record.
    pub fn support(&self, codec: Codec) -> Option<&HwCodecSupport> {
        self.codecs.iter().find(|c| c.codec == codec)
    }
}

/// Probes every platform engine available on this machine.
///
/// Blocking and potentially slow; call it on a worker thread.
pub fn devices() -> Vec<HwDeviceInfo> {
    crate::native::probe_devices()
}
