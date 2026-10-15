//! Layer 2 — the backend abstraction and the selection policy.
//!
//! * [`backend`] — backend identity and [`CodecPath`], the "which
//!   implementation actually did the work?" report.
//! * [`traits`] — the push/pull [`Encoder`] and [`Decoder`] contracts that every
//!   platform framework and every software codec implements.
//! * [`registry`] — registration, capability reporting and policy-driven
//!   backend selection.
//! * [`device`] — on-demand hardware engine probing.

pub mod backend;
pub mod device;
pub mod registry;
pub mod traits;

pub use backend::{BackendKind, BackendPreference, CodecPath, Implementation};
pub use device::{HwCodecSupport, HwDeviceInfo};
pub use registry::{
    registry, BackendAvailability, BackendInfo, CapabilityReport, CodecKind, Registry,
};
pub use traits::{Decoder, Encoder};
