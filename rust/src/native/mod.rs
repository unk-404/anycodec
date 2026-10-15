//! Layer 3a 鈥?platform media frameworks.
//!
//! One submodule per platform API, and one API per submodule. No `cfg` soup
//! inside a backend: each platform module compiles to a real implementation on
//! its target and to a no-op stub everywhere else, so `cargo check` works for
//! every target from any host.
//!
//! | Platform | Framework | Module |
//! |----------|-----------|--------|
//! | Android | NDK `AMediaCodec` | [`android`] |
//! | Windows | Media Foundation `IMFTransform` | [`windows`] |
//! | macOS / iOS | VideoToolbox + AudioToolbox | [`apple`] |
//! | Linux | GStreamer | [`linux`] |
//! | Web | WebCodecs | *implemented in Dart; see `lib/src/platform/web`* |
//!
//! ## Priority
//!
//! Platform frameworks register between `10` and `49`; software fallbacks
//! (`crate::soft`) register at `100` and above. A lower number wins.
//!
//! ## Scope note
//!
//! Every symbol in this module is declared and driven by AnyCodec itself: the
//! `extern` blocks, the COM vtables, the NDK calls and the `dlopen` resolver all
//! live in this crate. In particular this project does **not** depend on
//! `oxideav-videotoolbox` (or any other OxideAV crate) for Apple support 鈥?the
//! Apple backends below are AnyCodec's own.

/// Bitstream helpers shared by the platform backends.
pub mod bitstream;

// NOTE for anyone adding a backend: every platform module below is declared
// twice — once for its own target and once as its `#[path]` stub — and cbindgen
// does not collapse those two declarations. Anything `pub` inside a platform
// module therefore reaches the C header twice. Keep platform-module items
// `pub(crate)`; the C ABI lives in `crate::ffi` and nowhere else.

#[cfg(target_os = "android")]
pub mod android;
#[cfg(not(target_os = "android"))]
#[path = "android/stub.rs"]
pub mod android;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
#[path = "apple/stub.rs"]
pub mod apple;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(not(target_os = "linux"))]
#[path = "linux/stub.rs"]
pub mod linux;

#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(not(target_os = "windows"))]
#[path = "windows/stub.rs"]
pub mod windows;

/// Registration priorities for platform frameworks.
///
/// Kept in one place so the ordering is auditable in a single screen.
// `pub(crate)`: cbindgen emits every `pub const` in the crate, prefixed or not.
//
// Not every constant is used on every target — a Linux-only priority looks dead
// on Windows — so the vocabulary as a whole is allowed to be unused.
#[allow(dead_code)]
pub(crate) mod priority {
    /// A hardware media engine, first choice.
    pub(crate) const HARDWARE_ENGINE: u8 = 10;
    /// A platform framework that may or may not use hardware.
    pub(crate) const PLATFORM_FRAMEWORK: u8 = 20;
    /// A platform framework reached through a runtime-loaded shared library.
    pub(crate) const DYNAMIC_FRAMEWORK: u8 = 30;
}

/// Registers every platform framework that exists on the compiling target.
#[allow(unused_variables)]
pub fn register(registry: &mut crate::engine::Registry) {
    android::register(registry);
    apple::register(registry);
    linux::register(registry);
    windows::register(registry);
}

/// Probes every platform engine available on this machine.
#[allow(unused_variables)]
pub fn probe_devices() -> Vec<crate::engine::HwDeviceInfo> {
    let mut devices = Vec::new();
    devices.extend(android::probe_devices());
    devices.extend(apple::probe_devices());
    devices.extend(linux::probe_devices());
    devices.extend(windows::probe_devices());
    devices
}

/// `true` when at least one platform framework is usable in this process.
pub fn available() -> bool {
    android::available() || apple::available() || linux::available() || windows::available()
}
