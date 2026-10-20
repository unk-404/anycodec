//! Stand-in for the `apple` backend on hosts that are not `apple`.
//!
//! Compiling this module must never require the platform SDK, so the real
//! implementation lives in `apple/` and is only built on its own target.

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
