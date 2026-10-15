//! Resolution of `libavcodec` at runtime.
//!
//! Nothing in this module is a build-time dependency. The shared libraries are
//! searched for in the platform's usual places and loaded on first use, and the
//! `libavcodec` entry points AnyCodec needs are then resolved by name.
//!
//! Every failure turns into a *specific* message — `"libavcodec.so.61 not found
//! (searched: ...)"`, `"avcodec-61.dll is present but has no avcodec_version"` — //! because that string is what ends up in `CodecPath::fallback_reason` and
//! therefore in front of a developer trying to work out why they are on software.

/// What was found, once, for the process lifetime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegProbe {
    /// `"libavcodec.so.61"`, or `av_version_info()` when the library exposes it.
    pub library: String,
    /// `avcodec_version()` unpacked as `(major, minor, micro)`.
    pub version: (u32, u32, u32),
}

impl FfmpegProbe {
    /// `"libavcodec.so.61 (61.3.100)"`.
    pub fn describe(&self) -> String {
        format!("{} ({}.{}.{})", self.library, self.version.0, self.version.1, self.version.2)
    }
}

mod imp {
    use std::sync::OnceLock;

    use super::FfmpegProbe;

    /// The `libavcodec` sonames tried, in order, per platform.
    #[cfg(target_os = "windows")]
    pub const CANDIDATES: &[&str] =
        &["avcodec-62.dll", "avcodec-61.dll", "avcodec-60.dll", "avcodec.dll"];
    #[cfg(target_os = "macos")]
    pub const CANDIDATES: &[&str] = &[
        "libavcodec.62.dylib",
        "libavcodec.61.dylib",
        "libavcodec.60.dylib",
        "libavcodec.dylib",
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    pub const CANDIDATES: &[&str] =
        &["libavcodec.so.62", "libavcodec.so.61", "libavcodec.so.60", "libavcodec.so"];

    static PROBE: OnceLock<Result<FfmpegProbe, String>> = OnceLock::new();

    /// Probes for `libavcodec`, caching the outcome for the process lifetime.
    ///
    /// The library is deliberately leaked: once loaded it must stay mapped for as
    /// long as any resolved symbol pointer might be called.
    pub fn probe() -> Result<FfmpegProbe, String> {
        PROBE.get_or_init(load).clone()
    }

    fn load() -> Result<FfmpegProbe, String> {
        let mut attempted = Vec::new();
        for name in CANDIDATES {
            attempted.push(*name);
            // SAFETY: `Library::new` is unsafe only because arbitrary code can run
            // in the library's initialisers; loading a system codec library is the
            // intended use. The handle is leaked below so it outlives every symbol.
            let library = match unsafe { libloading::Library::new(name) } {
                Ok(library) => library,
                Err(_) => continue,
            };

            // SAFETY: `avcodec_version` is documented as `unsigned avcodec_version(void)`.
            let version: libloading::Symbol<unsafe extern "C" fn() -> u32> =
                match unsafe { library.get(b"avcodec_version\0") } {
                    Ok(symbol) => symbol,
                    Err(_) => {
                        return Err(format!(
                            "{name} is present but has no avcodec_version; it is not libavcodec"
                        ));
                    }
                };
            // SAFETY: the signature above matches libavcodec's actual ABI.
            let packed = unsafe { version() };
            let version = ((packed >> 16) & 0xff, (packed >> 8) & 0xff, packed & 0xff);

            // `const char *av_version_info(void)` is optional.
            let reported = unsafe {
                library
                    // SAFETY: the signature matches libavcodec's actual ABI.
                    .get::<unsafe extern "C" fn() -> *const std::os::raw::c_char>(
                        b"av_version_info\0",
                    )
            }
            .ok()
            .and_then(|symbol| {
                // SAFETY: libavcodec returns a NUL-terminated static string.
                let pointer = unsafe { symbol() };
                if pointer.is_null() {
                    return None;
                }
                // SAFETY: non-null pointer to a NUL-terminated static string.
                Some(unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy().into_owned())
            });
            let _ = &version;
            // Keep the library mapped for the life of the process.
            std::mem::forget(library);

            let probe = FfmpegProbe { library: reported.unwrap_or_else(|| (*name).to_string()), version };
            log::info!("anycodec: loaded {} as {}", name, probe.describe());
            return Ok(probe);
        }
        // Short on purpose: this string is reported for every codec and every
        // direction, so a paragraph here turns the capability report into noise.
        // The remedy lives in the crate docs and in `soft::ffmpeg::summary`.
        Err(format!("libavcodec not found (searched: {})", attempted.join(", ")))
    }
}

pub use imp::{probe, CANDIDATES};

/// `true` when `libavcodec` was found, regardless of whether a session can be
/// built from it. The distinction lives in
/// [`super::unavailable_reason`].
#[must_use]
pub fn is_present() -> bool {
    probe().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_never_panics_and_explains_itself() {
        match probe() {
            Ok(info) => {
                assert!(!info.library.is_empty());
                assert!(info.describe().contains(&info.version.0.to_string()));
            }
            Err(reason) => {
                // The message must be actionable: it is what a developer sees.
                assert!(reason.contains("libavcodec"), "{reason}");
                assert!(reason.len() > 40, "{reason}");
            }
        }
    }

    #[test]
    fn describe_is_stable() {
        let probe = FfmpegProbe { library: "libavcodec.so.61".into(), version: (61, 3, 100) };
        assert_eq!(probe.describe(), "libavcodec.so.61 (61.3.100)");
    }
}
