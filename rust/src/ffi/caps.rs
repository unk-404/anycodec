//! Capability and hardware-device reporting.

use std::os::raw::{c_char, c_uchar};

use crate::engine::backend::Implementation;
use crate::engine::registry;
use crate::media::codec::Codec;

use super::error::{STATUS_INVALID_ARGUMENT, STATUS_OK, set_error};
use super::guard;
use super::handles::AnyCodecDeviceList;
use super::types::{
    ANYCODEC_DEVICE_NAME_CAPACITY, ANYCODEC_NOTE_CAPACITY, ANYCODEC_VERSION_CAPACITY,
    write_cstring,
};

/// What is available for one codec in one direction.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AnyCodecCapability {
    /// One of the `ANYCODEC_CODEC_*` constants.
    pub codec: u32,
    /// `0` for encoders, `1` for decoders.
    pub kind: u32,
    /// Bitmask of every backend registered for this codec, available or not.
    pub backends: u32,
    /// Bitmask of the backends that can actually run right now.
    pub available_backends: u32,
    /// `1` when at least one available backend is known to use dedicated hardware.
    pub hardware: u32,
    /// `1` when at least one available backend is a software fallback.
    pub software: u32,
}

/// One backend's report for one codec.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AnyCodecBackendInfo {
    /// One of the `ANYCODEC_BACKEND_*` constants.
    pub backend: u32,
    /// `ANYCODEC_IMPLEMENTATION_NATIVE` or `ANYCODEC_IMPLEMENTATION_SOFTWARE`.
    pub implementation: u32,
    /// `1` when this backend is known to use dedicated hardware.
    pub hardware: u32,
    /// `1` when the backend can be constructed right now.
    pub available: u32,
    /// `"ready"`, or the specific reason the backend is unavailable.
    pub note: [c_char; ANYCODEC_NOTE_CAPACITY],
}

impl Default for AnyCodecBackendInfo {
    fn default() -> Self {
        Self {
            backend: 0,
            implementation: 0,
            hardware: 0,
            available: 0,
            note: [0; ANYCODEC_NOTE_CAPACITY],
        }
    }
}

impl std::fmt::Debug for AnyCodecBackendInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyCodecBackendInfo")
            .field("backend", &self.backend)
            .field("implementation", &self.implementation)
            .field("hardware", &self.hardware)
            .field("available", &self.available)
            .field("note", &self.note_str())
            .finish()
    }
}

impl AnyCodecBackendInfo {
    /// The `note` field as a string.
    pub fn note_str(&self) -> std::borrow::Cow<'_, str> {
        // SAFETY: the array is NUL-terminated by construction.
        unsafe { std::ffi::CStr::from_ptr(self.note.as_ptr()) }.to_string_lossy()
    }
}

/// One hardware media engine.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AnyCodecDeviceInfo {
    /// One of the `ANYCODEC_BACKEND_*` constants.
    pub backend: u32,
    /// Number of codecs `name`'s engine supports.
    pub codec_count: u32,
    /// Engine name.
    pub name: [c_char; ANYCODEC_DEVICE_NAME_CAPACITY],
    /// API version string.
    pub api_version: [c_char; ANYCODEC_VERSION_CAPACITY],
    /// Driver version string; empty when the platform does not expose one.
    pub driver_version: [c_char; ANYCODEC_VERSION_CAPACITY],
}

impl Default for AnyCodecDeviceInfo {
    fn default() -> Self {
        Self {
            backend: 0,
            codec_count: 0,
            name: [0; ANYCODEC_DEVICE_NAME_CAPACITY],
            api_version: [0; ANYCODEC_VERSION_CAPACITY],
            driver_version: [0; ANYCODEC_VERSION_CAPACITY],
        }
    }
}

impl std::fmt::Debug for AnyCodecDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyCodecDeviceInfo")
            .field("backend", &self.backend)
            .field("codec_count", &self.codec_count)
            .finish()
    }
}

/// One codec a hardware engine supports.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AnyCodecDeviceCodec {
    /// One of the `ANYCODEC_BACKEND_*` constants.
    pub backend: u32,
    /// One of the `ANYCODEC_CODEC_*` constants.
    pub codec: u32,
    /// `1` when the engine can decode this codec.
    pub decode: u32,
    /// `1` when the engine can encode this codec.
    pub encode: u32,
    /// Largest supported luma width, `0` when unknown.
    pub max_width: u32,
    /// Largest supported luma height, `0` when unknown.
    pub max_height: u32,
    /// Bits per component, `0` when unknown.
    pub max_bit_depth: u32,
}

// ---------------------------------------------------------------------------
// Exported entry points
// ---------------------------------------------------------------------------

/// Returns the number of (codec, direction) pairs in the capability report.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_capability_count() -> usize {
    Codec::ALL.len() * 2
}

/// Fills up to `capacity` entries of `out` and returns the **total** number of
/// entries available.
///
/// Call it once with `out == NULL` to learn the size, then again with a buffer of
/// that size. When the return value exceeds `capacity`, `out` was truncated.
///
/// # Safety
///
/// `out` must be NULL or point to `capacity` writable [`AnyCodecCapability`]
/// entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_capabilities(
    out: *mut AnyCodecCapability,
    capacity: usize,
) -> usize {
    guard::guard(0, || {
        let reports = registry().capabilities();
        let mut written = 0usize;
        let mut total = 0usize;
        for report in &reports {
            for kind in 0..2u32 {
                total += 1;
                if out.is_null() || written >= capacity {
                    continue;
                }
                let entries = if kind == 0 { &report.encoders } else { &report.decoders };
                let mut backends = 0u32;
                let mut available = 0u32;
                let mut hardware = false;
                let mut software = false;
                for entry in entries {
                    backends |= entry.backend.mask();
                    if entry.available {
                        available |= entry.backend.mask();
                        hardware |= entry.hardware;
                        software |= entry.implementation == Implementation::Software;
                    }
                }
                // SAFETY: `written < capacity` and `out` is non-null.
                unsafe {
                    *out.add(written) = AnyCodecCapability {
                        codec: report.codec.raw(),
                        kind,
                        backends,
                        available_backends: available,
                        hardware: u32::from(hardware),
                        software: u32::from(software),
                    };
                }
                written += 1;
            }
        }
        total
    })
}

/// Fills up to `capacity` backend entries for one codec/direction and returns the
/// total number of entries available.
///
/// # Safety
///
/// `out` must be NULL or point to `capacity` writable [`AnyCodecBackendInfo`]
/// entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_capability_backends(
    codec: u32,
    kind: u32,
    out: *mut AnyCodecBackendInfo,
    capacity: usize,
) -> usize {
    guard::guard(0, || {
        let Some(codec) = Codec::from_raw(codec) else {
            return 0;
        };
        // Only directions 0 (encoder) and 1 (decoder) exist. Anything else must
        // answer "nothing" rather than silently meaning "decoders".
        let entries = match kind {
            0 => &registry().capability(codec).map(|report| report.encoders),
            1 => &registry().capability(codec).map(|report| report.decoders),
            _ => return 0,
        };
        let Some(entries) = entries.as_ref() else {
            return 0;
        };
        for (index, entry) in entries.iter().enumerate() {
            if out.is_null() || index >= capacity {
                continue;
            }
            let mut info = AnyCodecBackendInfo {
                backend: entry.backend.raw(),
                implementation: entry.implementation.raw(),
                hardware: u32::from(entry.hardware),
                available: u32::from(entry.available),
                ..Default::default()
            };
            write_cstring(&mut info.note, &entry.note);
            // SAFETY: `index < capacity` and `out` is non-null.
            unsafe { *out.add(index) = info };
        }
        entries.len()
    })
}

/// Returns a human readable capability report on the **thread-local** string slot.
///
/// The pointer stays valid until the next AnyCodec call on the same thread.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_capability_summary() -> *const c_char {
    guard::guard_cstr(|| super::error::set_and_return_string(&crate::capability_summary()))
}

/// Probes the machine's hardware media engines.
///
/// This can block for milliseconds and may touch the GPU, so call it off the UI
/// thread (or through the Dart isolate pool). Returns NULL on failure.
#[unsafe(no_mangle)]
pub extern "C" fn anycodec_probe_devices() -> *mut AnyCodecDeviceList {
    guard::guard_ptr(|| {
        let devices = crate::engine::device::devices();
        super::error::clear();
        Box::into_raw(Box::new(AnyCodecDeviceList { devices }))
    })
}

/// Releases a device list.
///
/// # Safety
///
/// `list` must be NULL or an unfreed handle from [`anycodec_probe_devices`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_device_list_free(list: *mut AnyCodecDeviceList) {
    guard::guard_void(|| {
        if !list.is_null() {
            // SAFETY: the caller guarantees the handle came from `Box::into_raw`.
            drop(unsafe { Box::from_raw(list) });
        }
    });
}

/// Returns the number of engines in `list`, or `0` when `list` is NULL.
///
/// # Safety
///
/// `list` must be NULL or a live device list handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_device_list_len(list: *const AnyCodecDeviceList) -> usize {
    guard::guard(0, || {
        if list.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees `list` is live.
        unsafe { (*list).devices.len() }
    })
}

/// Writes engine `index` of `list` into `out`.
///
/// Returns `0` on success and `ANYCODEC_STATUS_INVALID_ARGUMENT` when a pointer
/// is NULL or the index is out of range.
///
/// # Safety
///
/// `list` must be a live device list handle and `out` a writable
/// [`AnyCodecDeviceInfo`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_device_list_device(
    list: *const AnyCodecDeviceList,
    index: usize,
    out: *mut AnyCodecDeviceInfo,
) -> i32 {
    guard::guard_status(|| {
        if list.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_device_list_device: list or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees `list` is live.
        let Some(device) = (unsafe { &*list }).devices.get(index) else {
            set_error(&crate::error::Error::invalid_argument(format!(
                "anycodec_device_list_device: index {index} is out of range"
            )));
            return STATUS_INVALID_ARGUMENT;
        };
        let mut info = AnyCodecDeviceInfo {
            backend: device.backend.raw(),
            codec_count: device.codecs.len() as u32,
            ..Default::default()
        };
        write_cstring(&mut info.name, &device.name);
        write_cstring(&mut info.api_version, &device.api_version);
        write_cstring(&mut info.driver_version, &device.driver_version);
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = info };
        super::error::clear();
        STATUS_OK
    })
}

/// Writes codec `codec_index` of engine `device_index` into `out`.
///
/// # Safety
///
/// `list` must be a live device list handle and `out` a writable
/// [`AnyCodecDeviceCodec`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn anycodec_device_list_codec(
    list: *const AnyCodecDeviceList,
    device_index: usize,
    codec_index: usize,
    out: *mut AnyCodecDeviceCodec,
) -> i32 {
    guard::guard_status(|| {
        if list.is_null() || out.is_null() {
            set_error(&crate::error::Error::invalid_argument(
                "anycodec_device_list_codec: list or out is NULL",
            ));
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller guarantees `list` is live.
        let devices = &unsafe { &*list }.devices;
        let Some(device) = devices.get(device_index) else {
            set_error(&crate::error::Error::invalid_argument(format!(
                "anycodec_device_list_codec: device index {device_index} is out of range"
            )));
            return STATUS_INVALID_ARGUMENT;
        };
        let Some(codec) = device.codecs.get(codec_index) else {
            set_error(&crate::error::Error::invalid_argument(format!(
                "anycodec_device_list_codec: codec index {codec_index} is out of range"
            )));
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: `out` is non-null and writable.
        unsafe {
            *out = AnyCodecDeviceCodec {
                backend: device.backend.raw(),
                codec: codec.codec.raw(),
                decode: u32::from(codec.decode),
                encode: u32::from(codec.encode),
                max_width: codec.max_width,
                max_height: codec.max_height,
                max_bit_depth: codec.max_bit_depth as u32,
            };
        }
        super::error::clear();
        STATUS_OK
    })
}

/// A pointer type kept so `c_uchar` stays referenced in this module's docs.
#[allow(dead_code)]
type BytePointer = *const c_uchar;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::error;
    use crate::ffi::types::ANYCODEC_CODEC_H264;

    #[test]
    fn capability_count_matches_the_codec_list() {
        assert_eq!(anycodec_capability_count(), Codec::ALL.len() * 2);
    }

    #[test]
    fn sizing_query_works_with_a_null_pointer() {
        // SAFETY: NULL is explicitly allowed and means "just tell me the size".
        let total = unsafe { anycodec_capabilities(std::ptr::null_mut(), 0) };
        assert_eq!(total, anycodec_capability_count());
    }

    #[test]
    fn capabilities_are_filled_in() {
        let mut entries = vec![AnyCodecCapability::default(); anycodec_capability_count()];
        // SAFETY: the buffer holds exactly `capacity` entries.
        let total = unsafe { anycodec_capabilities(entries.as_mut_ptr(), entries.len()) };
        assert_eq!(total, entries.len());
        assert!(entries.iter().any(|e| e.codec == super::super::types::ANYCODEC_CODEC_H264));
        assert!(entries.iter().all(|e| e.kind <= 1));
    }

    #[test]
    fn truncation_is_reported_by_the_return_value() {
        let mut one = [AnyCodecCapability::default(); 1];
        // SAFETY: the buffer holds exactly one entry.
        let total = unsafe { anycodec_capabilities(one.as_mut_ptr(), 1) };
        assert!(total > 1);
    }

    #[test]
    fn backend_report_for_an_unknown_codec_is_empty() {
        // SAFETY: NULL is explicitly allowed.
        let total = unsafe { anycodec_capability_backends(0xdead, 0, std::ptr::null_mut(), 0) };
        assert_eq!(total, 0);
    }

    #[test]
    fn every_production_codec_is_queryable_through_the_abi() {
        for codec in Codec::ALL {
            // The codec round-trips through the ABI constant.
            assert!(Codec::from_raw(codec.raw()).is_some(), "{codec}");
            for kind in 0..2u32 {
                let mut entries = vec![AnyCodecBackendInfo::default(); 16];
                // SAFETY: the buffer holds 16 writable entries.
                let total = unsafe {
                    anycodec_capability_backends(codec.raw(), kind, entries.as_mut_ptr(), entries.len())
                };
                // Whether a backend exists depends on the machine and the enabled
                // features; what must hold is that the query is well behaved.
                assert!(total <= entries.len(), "the report must not exceed the buffer");
            }
        }
    }

    #[test]
    fn an_unknown_codec_direction_is_reported_as_zero() {
        // Direction 9 is not a thing; the query must answer "nothing".
        // SAFETY: NULL is explicitly allowed.
        let total = unsafe {
            anycodec_capability_backends(ANYCODEC_CODEC_H264, 9, std::ptr::null_mut(), 0)
        };
        assert_eq!(total, 0);
    }

    #[test]
    fn capability_summary_is_a_readable_string() {
        error::clear();
        let pointer = anycodec_capability_summary();
        assert!(!pointer.is_null());
        // SAFETY: the pointer is a thread-local NUL-terminated string.
        let text = unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy();
        assert!(text.contains("h264"));
        assert!(text.contains("opus"));
    }

    #[test]
    fn device_list_accessors_are_null_safe() {
        // SAFETY: NULL is explicitly handled everywhere.
        unsafe {
            assert_eq!(anycodec_device_list_len(std::ptr::null()), 0);
            anycodec_device_list_free(std::ptr::null_mut());
            assert_eq!(
                anycodec_device_list_device(std::ptr::null(), 0, std::ptr::null_mut()),
                STATUS_INVALID_ARGUMENT
            );
            assert_eq!(
                anycodec_device_list_codec(std::ptr::null(), 0, 0, std::ptr::null_mut()),
                STATUS_INVALID_ARGUMENT
            );
        }
    }

    #[test]
    fn probing_devices_never_panics() {
        let list = anycodec_probe_devices();
        // The list may legitimately be empty on a machine with no media engine.
        let len = unsafe {
            // SAFETY: `list` is a live handle (or NULL, which is handled).
            let len = anycodec_device_list_len(list);
            anycodec_device_list_free(list);
            len
        };
        assert!(len < 1024, "an implausible number of engines: {len}");
    }
}
