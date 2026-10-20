//! Stand-in for the `android` backend on hosts that are not `android`.
//!
//! Compiling this module must never require the platform SDK, so the real
//! implementation lives in `android/` and is only built on its own target.

use crate::engine::{HwDeviceInfo, Registry};

/// Registers nothing: the framework does not exist on this host.
pub fn register(_registry: &mut Registry) {}

/// Always `false` on this host.
pub fn available() -> bool {
    false
}

/// Always empty on this host.
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    Vec::new()
}
