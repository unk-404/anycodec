//! The VideoToolbox half of the Apple backend.
//!
//! VideoToolbox exposes codecs as *sessions*: `VTCompressionSession` for encoding
//! and `VTDecompressionSession` for decoding. A session is created for one codec
//! and one geometry, configured with `VTSessionSetProperty`, and then driven by
//! handing it `CMSampleBuffer`s. Compressed frames come back through a C callback
//! and decoded frames through another, which is why this module is mostly session
//! lifetime and callback plumbing rather than codec work.
//!
//! ## The output callback and the session lifetime
//!
//! `VTCompressionSessionCreate` takes an `extern "C"` output callback *and* an
//! opaque `refcon`. The refcon here is an `Arc<CompressionSink>`'s raw pointer, and
//! the callback mutates the sink through its mutex. VideoToolbox may invoke the
//! callback from its own thread, so the sink must outlive the session:
//! [`VtEncoder`]'s `Drop` therefore calls `VTCompressionSessionInvalidate` — the
//! documented "deterministic, orderly teardown" that guarantees no further
//! callback — *before* the `Arc` is released. The same invalidate-then-release
//! ordering holds for [`VtDecoder`].
//!
//! ## Hardware acceleration
//!
//! After the session is configured,
//! `kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder` is read back
//! and reported verbatim. If the encoder does not answer at all, the flag is
//! `false` and `CodecPath::detail` says so: "the encoder did not say" is not the
//! same as "the encoder said no", and neither is the same as "there is hardware".
//!
//! Decoding has no per-session equivalent, so the system-wide
//! `VTIsHardwareDecodeSupported` is used instead and is named as the source of the
//! flag in `detail`.
//!
//! ## Profiles, and `VTProfessionalVideoWorkflow`
//!
//! `kVTCompressionPropertyKey_ProfileLevel` is set from the `"profile"` option:
//! H.264 accepts `baseline`, `main` and `high` (the default), and HEVC accepts
//! `main` (the default) and `main10`.
//!
//! `VTRegisterProfessionalVideoWorkflowVideoDecoders` / `...Encoders` exist in the
//! bindings but are deliberately **not** called. They add the professional codecs
//! (ProRes and friends) to the process's codec set; AnyCodec routes none of them,
//! so calling them would mutate global codec state on behalf of the application for
//! no benefit — and [`register`] must stay side-effect free.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{CMVideoCodecType, CMVideoFormatDescription, CMSampleBuffer};
use objc2_core_video::{CVImageBuffer, CVPixelBuffer};
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
    kVTProfileLevel_H264_Baseline_AutoLevel, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_H264_Main_AutoLevel, kVTProfileLevel_HEVC_Main10_AutoLevel,
    kVTProfileLevel_HEVC_Main_AutoLevel, kVTVideoEncoderList_CodecType,
    kVTVideoEncoderList_EncoderName, kVTVideoEncoderList_IsHardwareAccelerated, VTCopyVideoEncoderList,
    VTCompressionSession, VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTEncodeInfoFlags, VTIsHardwareDecodeSupported, VTSession,
    VTSessionCopyProperty, VTSessionSetProperty,
};

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, CodecKind, HwCodecSupport, HwDeviceInfo, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::{Codec, VideoCodec};
use crate::media::config::{DecoderConfig, EncoderConfig, Preset};
use crate::media::format::PixelFormat;
use crate::media::frame::{Frame, VideoFrame};
use crate::media::packet::{flags as packet_flags, Packet};

use super::pixel_buffer;
use super::{four_char_code, ms_from_time, os_status_text, time_from_ms};

/// How many packets the compression callback will hold before it reports that the
/// caller is not draining the encoder.
const MAX_PENDING_PACKETS: usize = 4_096;

/// How many frames the decompression callback will hold before it reports that the
/// caller is not draining the decoder.
const MAX_PENDING_FRAMES: usize = 256;

// ---------------------------------------------------------------------------
// The generic `VTSession` view
// ---------------------------------------------------------------------------

/// Reinterprets a compression session as the generic `VTSession` (a `CFType`) that
/// `VTSessionSetProperty` and `VTSessionCopyProperty` are declared for.
fn as_vt_session(session: &VTCompressionSession) -> &VTSession {
    session
}

// ---------------------------------------------------------------------------
// The encoder list
// ---------------------------------------------------------------------------

/// One encoder row from `VTCopyVideoEncoderList`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoEncoderInfo {
    /// The `CMVideoCodecType` this encoder produces.
    pub codec_type: CMVideoCodecType,
    /// The encoder's own name, from `kVTVideoEncoderList_EncoderName`.
    pub name: String,
    /// `kVTVideoEncoderList_IsHardwareAccelerated`, as reported.
    pub hardware: bool,
}

/// Lists every video encoder this machine offers.
///
/// `VTCopyVideoEncoderList` is the documented enumeration. It costs a framework
/// round trip, so it is only reached from [`available`], the probes and
/// [`probe_devices`] — never from a per-frame path.
pub fn encoder_list() -> Result<Vec<VideoEncoderInfo>> {
    let mut list: *const CFArray = std::ptr::null();
    let Some(out) = NonNull::new(&mut list) else {
        return Err(Error::internal("the encoder list out-parameter cannot be null"));
    };
    // SAFETY: `VTCopyVideoEncoderList` writes a +1 array reference into `out` on
    // success; the options dictionary is optional.
    let status = unsafe { VTCopyVideoEncoderList(None, out) };
    if status != 0 {
        return Err(Error::backend(format!(
            "VTCopyVideoEncoderList failed: {}",
            os_status_text(status)
        )));
    }
    let Some(pointer) = NonNull::new(list.cast_mut()) else {
        return Err(Error::backend("VTCopyVideoEncoderList reported success but returned no list"));
    };
    // SAFETY: follows the Create rule, so the reference carries +1.
    let list: CFRetained<CFArray> = unsafe { CFRetained::from_raw(pointer) };
    // SAFETY: the list holds one dictionary per encoder, keyed by `CFString`.
    let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(list) };

    // SAFETY: reading framework constants that VideoToolbox initialises.
    let (codec_key, name_key, hardware_key) = unsafe {
        (
            kVTVideoEncoderList_CodecType,
            kVTVideoEncoderList_EncoderName,
            kVTVideoEncoderList_IsHardwareAccelerated,
        )
    };

    let mut encoders = Vec::with_capacity(list.len());
    for index in 0..list.len() {
        let Some(entry) = list.get(index) else {
            continue;
        };
        let codec_type = entry
            .get(codec_key)
            .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32));
        // An encoder row without a codec type cannot be attributed to a codec.
        let Some(codec_type) = codec_type else {
            continue;
        };
        let name = entry
            .get(name_key)
            .and_then(|value| value.downcast_ref::<CFString>().map(ToString::to_string))
            .unwrap_or_else(|| format!("'{}'", four_char_code(codec_type)));
        let hardware = entry
            .get(hardware_key)
            .and_then(|value| value.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool))
            .unwrap_or(false);
        encoders.push(VideoEncoderInfo {
            codec_type: codec_type as CMVideoCodecType,
            name,
            hardware,
        });
    }
    Ok(encoders)
}

/// `true` when VideoToolbox answered a real query.
///
/// VideoToolbox has no `MFStartup`-style initialiser, so the framework counts as
/// reachable exactly when enumerating its encoders succeeds. Never panics.
#[must_use]
pub fn available() -> bool {
    encoder_list().is_ok()
}

/// A decompression output callback that does nothing.
///
/// Only [`decoder_available`] uses it: the probe asks whether a session can be
/// created, never decodes, and `VTDecompressionSessionCreate` takes a callback
/// record rather than letting a caller opt out of one.
unsafe extern "C-unwind" fn ignore_decoded_frame(
    _decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    _status: i32,
    _info_flags: VTDecodeInfoFlags,
    _image_buffer: *mut CVImageBuffer,
    _presentation_time_stamp: objc2_core_media::CMTime,
    _presentation_duration: objc2_core_media::CMTime,
) {
}

/// `true` when VideoToolbox can instantiate a decoder for `codec_type`.
///
/// The probe really tries: it builds a bare format description for the codec type
/// and calls `VTDecompressionSessionCreate`. A codec with no decoder fails there
/// with `kVTCouldNotFindVideoDecoderErr`; nothing is assumed from a table.
pub fn decoder_available(codec_type: CMVideoCodecType) -> Result<bool> {
    let description = pixel_buffer::bare_format_description(codec_type, 16, 16)?;
    let record = VTDecompressionOutputCallbackRecord {
        decompressionOutputCallback: Some(ignore_decoded_frame),
        decompressionOutputRefCon: std::ptr::null_mut(),
    };
    let mut session: *mut VTDecompressionSession = std::ptr::null_mut();
    let Some(out) = NonNull::new(&mut session) else {
        return Err(Error::internal("the session out-parameter cannot be null"));
    };
    // SAFETY: `description` is a live format description, `record` a live callback
    // record whose callback never touches its refcon, and `out` a valid local.
    // `VTDecompressionSessionCreate` writes a +1 session reference into `out` on
    // success. The decoder specification and image attributes are optional.
    let status =
        unsafe { VTDecompressionSession::create(None, &description, None, None, &record, out) };
    if status != 0 {
        log::debug!(
            "VideoToolbox has no decoder for '{}': {}",
            four_char_code(codec_type as i32),
            os_status_text(status)
        );
        return Ok(false);
    }
    let Some(pointer) = NonNull::new(session) else {
        return Err(Error::backend(
            "VTDecompressionSessionCreate reported success but returned no session",
        ));
    };
    // SAFETY: follows the Create rule, so the reference carries +1.
    let session: CFRetained<VTDecompressionSession> = unsafe { CFRetained::from_raw(pointer) };
    // SAFETY: invalidating a live session is always allowed, and this one is never
    // used for anything else.
    unsafe { session.invalidate() };
    Ok(true)
}

/// `true` when the system reports hardware decode support for `codec_type`.
///
/// `VTIsHardwareDecodeSupported` is the platform's own answer. It describes the
/// *machine*, not one session, which is why [`VtDecoder::new`] names it as the
/// source of the flag it reports.
#[must_use]
pub fn hardware_decode_available(codec_type: CMVideoCodecType) -> bool {
    // SAFETY: a pure query over the codec type, with no pointer arguments.
    unsafe { VTIsHardwareDecodeSupported(codec_type) }
}

// ---------------------------------------------------------------------------
// Probing and registration
// ---------------------------------------------------------------------------

/// The picture codecs this backend can decode, in report order.
pub(crate) const DECODABLE: [Codec; 5] = [
    Codec::Video(VideoCodec::H264),
    Codec::Video(VideoCodec::H265),
    Codec::Video(VideoCodec::Vp9),
    Codec::Video(VideoCodec::Av1),
    Codec::Video(VideoCodec::H266),
];

/// The picture codecs this backend can encode, in report order.
///
/// VideoToolbox ships no VP9, AV1 or VVC *encoder*, so those are decode-only and
/// are not registered in the encoder direction at all.
pub(crate) const ENCODABLE: [Codec; 2] = [Codec::Video(VideoCodec::H264), Codec::Video(VideoCodec::H265)];

/// Probes one codec and direction, and explains the outcome.
///
/// Where the framework offers a cheap probe, it is used: the encoder direction
/// reads `VTCopyVideoEncoderList`. The decoder direction really attempts a session,
/// so a codec with no decoder is reported as such rather than assumed.
#[must_use]
pub fn probe_component(codec: Codec, kind: CodecKind) -> BackendAvailability {
    if !codec.is_video() {
        return BackendAvailability::Unavailable(format!(
            "VideoToolbox handles video only; {} is an AudioToolbox codec",
            codec.name()
        ));
    }
    let Some(codec_type) = super::video_codec_type(codec) else {
        return BackendAvailability::Unavailable(format!(
            "CoreMedia declares no CMVideoCodecType for {}",
            codec.name()
        ));
    };
    match kind {
        CodecKind::Encoder => {
            if !ENCODABLE.contains(&codec) {
                return BackendAvailability::Unavailable(format!(
                    "{} has no VideoToolbox encoder; this backend encodes H.264 and HEVC only",
                    codec.label()
                ));
            }
            match encoder_list() {
                Ok(list) if list.iter().any(|encoder| encoder.codec_type == codec_type) => {
                    BackendAvailability::Available
                }
                Ok(_) => BackendAvailability::Unavailable(format!(
                    "VTCopyVideoEncoderList reports no {} encoder on this machine",
                    codec.label()
                )),
                Err(error) => BackendAvailability::Unavailable(error.message().to_string()),
            }
        }
        CodecKind::Decoder => match decoder_available(codec_type) {
            Ok(true) => BackendAvailability::Available,
            Ok(false) => BackendAvailability::Unavailable(format!(
                "VTDecompressionSessionCreate refused {} on this machine",
                codec.label()
            )),
            Err(error) => BackendAvailability::Unavailable(error.message().to_string()),
        },
    }
}

/// Reports the VideoToolbox engine and the codecs it can actually serve.
///
/// Called on demand, never during [`register`]. Only codecs whose probe succeeds
/// appear, so the record cannot overstate what this machine can do. Sizes and bit
/// depths stay at `0`, meaning "unknown": VideoToolbox publishes neither through
/// these APIs, and a guessed limit in a capability report is worse than no limit.
#[must_use]
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    let Ok(encoders) = encoder_list() else {
        return Vec::new();
    };
    let mut device =
        HwDeviceInfo::new(BackendKind::VideoToolbox, "Apple VideoToolbox media engine", "VideoToolbox");
    let hardware: Vec<&str> =
        encoders.iter().filter(|encoder| encoder.hardware).map(|encoder| encoder.name.as_str()).collect();
    if !hardware.is_empty() {
        device.driver_version = hardware.join(", ");
    }
    for codec in Codec::ALL {
        let Some(codec_type) = super::video_codec_type(codec) else {
            continue;
        };
        let encode =
            ENCODABLE.contains(&codec) && encoders.iter().any(|encoder| encoder.codec_type == codec_type);
        let decode = decoder_available(codec_type).unwrap_or(false);
        if !encode && !decode {
            continue;
        }
        device.codecs.push(HwCodecSupport {
            codec,
            decode,
            encode,
            max_width: 0,
            max_height: 0,
            max_bit_depth: 0,
        });
    }
    if device.codecs.is_empty() {
        Vec::new()
    } else {
        vec![device]
    }
}

/// Registers the codecs VideoToolbox can actually serve on this machine.
pub fn register(registry: &mut Registry) {
    if !available() {
        log::debug!("VideoToolbox is not reachable; the Apple video backend registers nothing");
        return;
    }
    for codec in ENCODABLE {
        if !probe_component(codec, CodecKind::Encoder).is_available() {
            continue;
        }
        registry.register_encoder(
            codec,
            BackendKind::VideoToolbox,
            super::super::priority::HARDWARE_ENGINE,
            |config: &EncoderConfig| {
                if !VtEncoder::supports_input(config.pixel_format) {
                    return Ok(None);
                }
                Ok(Some(Box::new(VtEncoder::new(config)?) as Box<dyn Encoder>))
            },
            |codec: Codec| probe_component(codec, CodecKind::Encoder),
        );
    }
    for codec in DECODABLE {
        if !probe_component(codec, CodecKind::Decoder).is_available() {
            continue;
        }
        registry.register_decoder(
            codec,
            BackendKind::VideoToolbox,
            super::super::priority::HARDWARE_ENGINE,
            |config: &DecoderConfig| Ok(Some(Box::new(VtDecoder::new(config)?) as Box<dyn Decoder>)),
            |codec: Codec| probe_component(codec, CodecKind::Decoder),
        );
    }
}

// ---------------------------------------------------------------------------
// Compression
// ---------------------------------------------------------------------------

/// The packets the compression callback produced but `receive()` has not handed
/// out yet, plus the first failure it saw.
#[derive(Debug, Default)]
struct CompressionState {
    packets: VecDeque<Packet>,
    failure: Option<String>,
    saturated: bool,
}

/// Shared between a compression session and its output callback.
#[derive(Debug)]
struct CompressionSink {
    codec: Codec,
    state: Mutex<CompressionState>,
}

impl CompressionSink {
    /// Wraps an empty queue for `codec`.
    fn new(codec: Codec) -> Self {
        Self { codec, state: Mutex::new(CompressionState::default()) }
    }
}

/// The `VTCompressionOutputCallback` that funnels compressed frames into the
/// trait's `receive()` queue.
///
/// # Safety
///
/// `output_callback_ref_con` must be the raw pointer of the live
/// `Arc<CompressionSink>` that owns the session, and that `Arc` must outlive the
/// last callback. Both hold because the session is invalidated before the `Arc` is
/// released.
unsafe extern "C-unwind" fn compression_output(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: i32,
    _info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut CMSampleBuffer,
) {
    if output_callback_ref_con.is_null() {
        return;
    }
    // SAFETY: the contract above. `CompressionSink` is `Sync` (a `Mutex` over a
    // `Send` payload), and the mutex is what makes an access from VideoToolbox's
    // own encoder thread sound.
    let sink = unsafe { &*output_callback_ref_con.cast::<CompressionSink>() };
    let Ok(mut state) = sink.state.lock() else {
        // A poisoned mutex means another thread panicked while holding it. There is
        // nothing sensible left to do here, and panicking across the C boundary is
        // not an option.
        return;
    };
    if status != 0 {
        if state.failure.is_none() {
            state.failure = Some(format!(
                "the VideoToolbox output callback reported {}",
                os_status_text(status)
            ));
        }
        return;
    }
    if sample_buffer.is_null() {
        return;
    }
    // SAFETY: a non-null sample buffer handed to this callback is live for the
    // duration of the call.
    let sample_buffer = unsafe { &*sample_buffer };
    match pixel_buffer::bytes_from_sample(sample_buffer) {
        Ok(Some((data, pts, keyframe))) => {
            if state.packets.len() >= MAX_PENDING_PACKETS {
                state.saturated = true;
                return;
            }
            let flags = if keyframe { packet_flags::KEYFRAME } else { 0 };
            state.packets.push_back(Packet::new(sink.codec, Buffer::from_vec(data), pts, flags));
        }
        Ok(None) => {}
        Err(error) => {
            if state.failure.is_none() {
                state.failure = Some(error.message().to_string());
            }
        }
    }
}

/// A live VideoToolbox compression session.
pub struct VtEncoder {
    session: CFRetained<VTCompressionSession>,
    codec: Codec,
    path: CodecPath,
    input_format: PixelFormat,
    format_description: CFRetained<CMVideoFormatDescription>,
    /// Duration stamped on a picture that carries none of its own.
    frame_duration_ms: i64,
    /// One strong reference kept for the session's `refcon`; see `Drop`.
    sink: Arc<CompressionSink>,
}

// SAFETY: `VTCompressionSession` is a reference-counted CoreFoundation object with
// no thread affinity. A session is owned by exactly one `VtEncoder`, which the
// registry moves once onto the worker thread that will use it and frees on that
// same thread. The `objc2` binding types the session as `!Send` because a raw
// pointer could in principle be shared; this type never shares it.
unsafe impl Send for VtEncoder {}

impl std::fmt::Debug for VtEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtEncoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .finish()
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        // Order matters, and it is the whole reason this `Drop` exists: the session
        // must be torn down *before* the sink its `refcon` points at is released.
        // `VTCompressionSessionInvalidate` is the documented deterministic teardown
        // that guarantees no further output callback, so after it returns nothing
        // can reach the sink; the `Arc` is released afterwards, when the fields drop.
        // SAFETY: invalidating a live session is always allowed, and no other thread
        // can be using this encoder.
        unsafe { self.session.invalidate() };
    }
}

impl VtEncoder {
    /// `true` when VideoToolbox will accept `format` as encoder input.
    ///
    /// Its encoders take 4:2:0 source buffers, 8-bit or 10-bit. Packed RGB and
    /// packed 4:2:2 input would need an internal conversion VideoToolbox does not
    /// promise, so the factory returns `Ok(None)` for those configurations and the
    /// registry falls through to a backend that does handle them.
    #[must_use]
    pub fn supports_input(format: PixelFormat) -> bool {
        matches!(format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::P010)
    }

    /// Creates and configures a compression session for `config`.
    ///
    /// Fails with a specific message when CoreMedia has no codec type for the codec,
    /// when the input layout is unsupported, or when VideoToolbox refuses the
    /// session or one of the required properties.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        let codec = config.codec;
        let codec_type = super::video_codec_type(codec).ok_or_else(|| {
            Error::unsupported(format!("CoreMedia has no video codec type for {}", codec.name()))
        })?;
        if !Self::supports_input(config.pixel_format) {
            return Err(Error::unsupported(format!(
                "the VideoToolbox encoder cannot take {} input",
                config.pixel_format.name()
            )));
        }
        if !ENCODABLE.contains(&codec) {
            return Err(Error::unsupported(format!("VideoToolbox has no {} encoder", codec.label())));
        }

        let sink = Arc::new(CompressionSink::new(codec));
        let refcon = Arc::as_ptr(&sink).cast_mut().cast::<c_void>();
        let mut session: *mut VTCompressionSession = std::ptr::null_mut();
        let Some(out) = NonNull::new(&mut session) else {
            return Err(Error::internal("the session out-parameter cannot be null"));
        };
        // SAFETY: `VTCompressionSessionCreate` writes a +1 session reference into
        // `out` on success. The callback is a real `extern "C-unwind"` function and
        // `refcon` is the live `Arc` above, which outlives the session by the
        // ordering documented on `Drop`.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                config.width as i32,
                config.height as i32,
                codec_type,
                None,
                None,
                None,
                Some(compression_output),
                refcon,
                out,
            )
        };
        if status != 0 {
            return Err(Error::backend(format!(
                "VTCompressionSessionCreate({} {}x{}) failed: {}",
                codec.name(),
                config.width,
                config.height,
                os_status_text(status)
            )));
        }
        let Some(pointer) = NonNull::new(session) else {
            return Err(Error::backend(
                "VTCompressionSessionCreate reported success but returned no session",
            ));
        };
        // SAFETY: follows the Create rule, so the reference carries +1.
        let session: CFRetained<VTCompressionSession> = unsafe { CFRetained::from_raw(pointer) };

        let frame_rate = config.framerate().unwrap_or(30.0).max(1.0);
        let bitrate = if config.bitrate > 0 { config.bitrate } else { derive_bitrate(config) };
        let keyframe_interval = if config.keyframe_interval > 0 {
            config.keyframe_interval
        } else {
            (frame_rate * 2.0).round().clamp(1.0, 600.0) as u32
        };

        set_required_property(
            &session,
            // SAFETY: reading a framework constant that VideoToolbox initialises.
            unsafe { kVTCompressionPropertyKey_AverageBitRate },
            &CFNumber::new_i64(bitrate as i64),
        )?;
        set_required_property(
            &session,
            // SAFETY: as above.
            unsafe { kVTCompressionPropertyKey_ExpectedFrameRate },
            &CFNumber::new_f64(frame_rate),
        )?;
        set_required_property(
            &session,
            // SAFETY: as above.
            unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval },
            &CFNumber::new_i32(keyframe_interval as i32),
        )?;
        set_required_property(&session, profile_level_key(), profile_level_value(config)?)?;
        // `_RealTime` and `_AllowFrameReordering` are hints that a few encoders
        // legitimately refuse, so a refusal is logged rather than fatal.
        set_optional_property(
            &session,
            // SAFETY: as above.
            unsafe { kVTCompressionPropertyKey_RealTime },
            CFBoolean::new(config.preset == Preset::Speed),
        );
        set_optional_property(
            &session,
            // SAFETY: as above.
            unsafe { kVTCompressionPropertyKey_AllowFrameReordering },
            CFBoolean::new(config.preset != Preset::Speed),
        );

        let path = encoder_path(codec, using_hardware_encoder(&session));
        let format_description =
            pixel_buffer::raw_video_format_description(config.pixel_format, config.width, config.height)?;
        let frame_duration_ms = (1_000.0 / frame_rate).round().max(1.0) as i64;
        Ok(Self {
            session,
            codec,
            path,
            input_format: config.pixel_format,
            format_description,
            frame_duration_ms,
            sink,
        })
    }

    /// Reads the queue the output callback filled, mapping a callback failure and a
    /// full queue to specific errors.
    fn take_pending(&mut self) -> Result<Option<Packet>> {
        let mut state = self
            .sink
            .state
            .lock()
            .map_err(|_| Error::state("the VideoToolbox output queue is poisoned"))?;
        if let Some(failure) = state.failure.take() {
            return Err(Error::backend(failure));
        }
        if state.saturated {
            state.saturated = false;
            return Err(Error::state(format!(
                "VideoToolbox produced more than {MAX_PENDING_PACKETS} packets without receive() being \
                 called; the excess was dropped"
            )));
        }
        Ok(state.packets.pop_front())
    }
}

/// Derives a target bitrate from the quality hint when the caller set no bitrate.
///
/// The model is bits per pixel per frame, which is also how the quality hint itself
/// is derived; `0.05` bits per pixel is the low end of the hint and `0.20` the high
/// end.
fn derive_bitrate(config: &EncoderConfig) -> u64 {
    let quality = f64::from(config.effective_quality()).clamp(0.05, 1.0);
    let pixels = f64::from(config.width) * f64::from(config.height);
    let frame_rate = config.framerate().unwrap_or(30.0).max(1.0);
    let bits = pixels * frame_rate * (0.05 + 0.15 * quality);
    bits.clamp(50_000.0, 200_000_000.0) as u64
}

/// The property key `kVTCompressionPropertyKey_ProfileLevel`.
fn profile_level_key() -> &'static CFString {
    // SAFETY: reading a framework constant that VideoToolbox initialises.
    unsafe { kVTCompressionPropertyKey_ProfileLevel }
}

/// The `kVTProfileLevel_*` constant for the codec and the caller's `"profile"`
/// option.
///
/// H.264 defaults to High and HEVC to Main, the profiles every VideoToolbox encoder
/// supports on every shipping OS.
fn profile_level_value(config: &EncoderConfig) -> Result<&'static CFString> {
    let requested = config.option("profile").unwrap_or("auto").to_ascii_lowercase();
    let level = match config.codec {
        // SAFETY: these are framework constants that VideoToolbox initialises.
        Codec::Video(VideoCodec::H264) => match requested.as_str() {
            "baseline" | "constrainedbaseline" | "constrained_baseline" => unsafe {
                kVTProfileLevel_H264_Baseline_AutoLevel
            },
            "main" => unsafe { kVTProfileLevel_H264_Main_AutoLevel },
            _ => unsafe { kVTProfileLevel_H264_High_AutoLevel },
        },
        Codec::Video(VideoCodec::H265) => match requested.as_str() {
            "main10" | "main_10" => unsafe { kVTProfileLevel_HEVC_Main10_AutoLevel },
            _ => unsafe { kVTProfileLevel_HEVC_Main_AutoLevel },
        },
        other => {
            return Err(Error::unsupported(format!(
                "{} has no VideoToolbox profile level",
                other.name()
            )))
        }
    };
    Ok(level)
}

/// Sets a session property the encoder must honour.
fn set_required_property(session: &VTCompressionSession, key: &CFString, value: &CFType) -> Result<()> {
    // SAFETY: `session` is live and `value` is a CoreFoundation object of the type
    // the key documents (a CFNumber or a CFString).
    let status = unsafe { VTSessionSetProperty(as_vt_session(session), key, Some(value)) };
    if status != 0 {
        return Err(Error::backend(format!(
            "VTSessionSetProperty({key}) failed: {}",
            os_status_text(status)
        )));
    }
    Ok(())
}

/// Sets a session property that is only a hint.
///
/// A refusal is logged at debug level instead of failing the session: `_RealTime`
/// and `_AllowFrameReordering` are both documented as hints and some encoders do not
/// implement them.
fn set_optional_property(session: &VTCompressionSession, key: &CFString, value: &CFType) {
    // SAFETY: as in `set_required_property`.
    let status = unsafe { VTSessionSetProperty(as_vt_session(session), key, Some(value)) };
    if status != 0 {
        log::debug!(
            "the VideoToolbox encoder did not accept the hint {key}: {} (continuing)",
            os_status_text(status)
        );
    }
}

/// Reads `kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder`.
///
/// `None` means the encoder did not answer at all, which is reported as "not
/// hardware" with the reason spelled out in `CodecPath::detail`.
fn using_hardware_encoder(session: &VTCompressionSession) -> Option<bool> {
    // SAFETY: reading a framework constant that VideoToolbox initialises.
    let key = unsafe { kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder };
    let mut value: *const c_void = std::ptr::null();
    // SAFETY: `VTSessionCopyProperty` writes a +1 CoreFoundation object into `value`
    // on success; the allocator argument is optional.
    let status = unsafe {
        VTSessionCopyProperty(
            as_vt_session(session),
            key,
            None,
            (&mut value as *mut *const c_void).cast::<c_void>(),
        )
    };
    if status != 0 || value.is_null() {
        return None;
    }
    let pointer = NonNull::new(value.cast_mut().cast::<CFType>())?;
    // SAFETY: `VTSessionCopyProperty` follows the Create rule, so the value carries
    // +1 and `CFRetained` takes ownership of exactly that.
    let value: CFRetained<CFType> = unsafe { CFRetained::from_raw(pointer) };
    if let Some(boolean) = value.downcast_ref::<CFBoolean>() {
        return Some(boolean.as_bool());
    }
    value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32).map(|number| number != 0)
}

impl Encoder for VtEncoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        let Frame::Video(video) = frame else {
            return Err(Error::invalid_argument(format!(
                "the {} VideoToolbox encoder needs raw video frames",
                self.codec.name()
            )));
        };
        if video.format != self.input_format {
            return Err(Error::invalid_argument(format!(
                "this {} encoder was opened for {} input but was handed {}",
                self.codec.name(),
                self.input_format.name(),
                video.format.name()
            )));
        }
        let buffer = pixel_buffer::create_buffer(self.input_format, video.width, video.height)?;
        pixel_buffer::fill_buffer(&buffer, &video)?;
        let duration = if video.duration > 0 { video.duration } else { self.frame_duration_ms };
        // The sample buffer is what carries the picture's timing; the image buffer
        // and the presentation time are then handed to the session from it, so there
        // is exactly one place where a frame's timestamp is decided.
        let sample =
            pixel_buffer::image_sample_buffer(&buffer, &self.format_description, video.pts, duration)?;
        let image = pixel_buffer::image_of(&sample)?;
        // SAFETY: the session is live, `image` is the pixel buffer the sample buffer
        // wraps, and the presentation times come from the caller. VideoToolbox
        // requires them to increase; a repeat is rejected with a specific status
        // rather than corrupting the stream.
        let status = unsafe {
            self.session.encode_frame(
                &image,
                sample.presentation_time_stamp(),
                time_from_ms(duration),
                None,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(Error::backend(format!(
                "VTCompressionSessionEncodeFrame(pts {}) failed: {}",
                video.pts,
                os_status_text(status)
            )));
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        self.take_pending()
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        // SAFETY: `VTCompressionSessionCompleteFrames` with a non-numeric time
        // flushes every pending frame, and the session is live.
        let invalid = unsafe { objc2_core_media::kCMTimeInvalid };
        let status = unsafe { self.session.complete_frames(invalid) };
        if status != 0 {
            return Err(Error::backend(format!(
                "VTCompressionSessionCompleteFrames failed: {}",
                os_status_text(status)
            )));
        }
        let mut packets = Vec::new();
        while let Some(packet) = self.take_pending()? {
            packets.push(packet);
        }
        Ok(packets)
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        if bitrate == 0 {
            return Err(Error::invalid_argument("a zero bitrate is not a bitrate"));
        }
        set_required_property(
            &self.session,
            // SAFETY: reading a framework constant that VideoToolbox initialises.
            unsafe { kVTCompressionPropertyKey_AverageBitRate },
            &CFNumber::new_i64(bitrate as i64),
        )
    }

    // `request_keyframe` and `extradata` keep their trait defaults:
    //
    // * A forced keyframe would need `kVTCompressionPropertyKey_ForceKeyFrame`
    //   through `VTSessionSetProperty`; it is not implemented here because nothing
    //   in AnyCodec asks for it yet, and claiming support without a way to test it
    //   would be worse than reporting `Unsupported`.
    // * `extradata` returns `None` because this backend emits Annex-B packets.
    //   VideoToolbox carries the parameter sets inside the format description
    //   attached to the compressed sample buffer, and rebuilding an `avcC` / `hvcC`
    //   record from it is a container concern this backend does not take on.
}

// ---------------------------------------------------------------------------
// Decompression
// ---------------------------------------------------------------------------

/// The frames the decompression callback produced but `receive()` has not handed
/// out yet, plus the first failure it saw.
#[derive(Debug, Default)]
struct DecompressionState {
    frames: VecDeque<Frame>,
    failure: Option<String>,
    saturated: bool,
}

/// Shared between a decompression session and its output callback.
#[derive(Debug)]
struct DecompressionSink {
    state: Mutex<DecompressionState>,
}

impl DecompressionSink {
    /// Wraps an empty queue.
    fn new() -> Self {
        Self { state: Mutex::new(DecompressionState::default()) }
    }
}

/// The `VTDecompressionOutputCallback` that funnels decoded pictures into the
/// trait's `receive()` queue.
///
/// # Safety
///
/// `decompression_output_ref_con` must be the raw pointer of the live
/// `Arc<DecompressionSink>` that owns the session, and that `Arc` must outlive the
/// last callback. Both hold because the session is invalidated before the `Arc` is
/// released.
unsafe extern "C-unwind" fn decompression_output(
    decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: i32,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    presentation_time_stamp: objc2_core_media::CMTime,
    _presentation_duration: objc2_core_media::CMTime,
) {
    if decompression_output_ref_con.is_null() {
        return;
    }
    // SAFETY: the contract above; the mutex mediates an access from VideoToolbox's
    // own decoder thread.
    let sink = unsafe { &*decompression_output_ref_con.cast::<DecompressionSink>() };
    let Ok(mut state) = sink.state.lock() else {
        return;
    };
    if status != 0 {
        if state.failure.is_none() {
            state.failure = Some(format!(
                "the VideoToolbox decompression callback reported {}",
                os_status_text(status)
            ));
        }
        return;
    }
    if image_buffer.is_null() {
        // A frame the decoder chose not to emit; not an error.
        return;
    }
    if state.frames.len() >= MAX_PENDING_FRAMES {
        state.saturated = true;
        return;
    }
    // SAFETY: a non-null image buffer handed to this callback is live for the
    // duration of the call, and a video decompression session only ever emits
    // `CVPixelBuffer`s. The C API types the parameter as the `CVImageBuffer`
    // superclass because the same callback shape serves other image buffers.
    let pixel_buffer = unsafe { &*image_buffer.cast::<CVPixelBuffer>() };
    let frame = match pixel_buffer::read_buffer(pixel_buffer) {
        Ok((format, width, height, planes)) => {
            VideoFrame::new(format, width, height, planes, ms_from_time(presentation_time_stamp))
                .map(Frame::Video)
        }
        Err(error) => Err(error),
    };
    match frame {
        Ok(frame) => state.frames.push_back(frame),
        Err(error) => {
            if state.failure.is_none() {
                state.failure = Some(error.message().to_string());
            }
        }
    }
}

/// A live VideoToolbox decompression session.
pub struct VtDecoder {
    session: CFRetained<VTDecompressionSession>,
    codec: Codec,
    path: CodecPath,
    /// The format description the session was created with, kept because
    /// `VTDecompressionSession` has no accessor for it and every compressed sample
    /// buffer has to be created with the same description.
    description: CFRetained<CMVideoFormatDescription>,
    /// One strong reference kept for the session's `refcon`; see `Drop`.
    sink: Arc<DecompressionSink>,
}

// SAFETY: the same argument as `VtEncoder`: a `VTDecompressionSession` is a
// reference-counted CoreFoundation object with no thread affinity, owned by exactly
// one `VtDecoder` and moved once, before use, onto its worker thread.
unsafe impl Send for VtDecoder {}

impl std::fmt::Debug for VtDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtDecoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .finish()
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        // The same ordering rule as `VtEncoder::drop`: the callback must be dead
        // before the sink its `refcon` points at is released.
        // SAFETY: invalidating a live session is always allowed.
        unsafe { self.session.invalidate() };
    }
}

impl VtDecoder {
    /// Creates a decompression session for `config`.
    ///
    /// H.264 and HEVC are configured from the parameter sets in `config.extradata`
    /// when there are any, and from the codec type plus the caller's size hints
    /// otherwise — which is what a stream carrying its parameter sets in band needs.
    /// VP9, AV1 and VVC always take the second route, because their configuration is
    /// in the bitstream by construction.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        let codec = config.codec;
        let codec_type = super::video_codec_type(codec).ok_or_else(|| {
            Error::unsupported(format!("CoreMedia has no video codec type for {}", codec.name()))
        })?;
        let carries_parameter_sets =
            matches!(codec, Codec::Video(VideoCodec::H264) | Codec::Video(VideoCodec::H265));
        let (description, from_extradata) = if carries_parameter_sets {
            match pixel_buffer::compressed_video_format_description(
                codec,
                config.extradata.as_slice(),
                config.annex_b,
            ) {
                Ok(description) => (description, true),
                Err(_) if config.extradata.is_empty() => (
                    pixel_buffer::bare_format_description(codec_type, config.width, config.height)?,
                    false,
                ),
                Err(error) => return Err(error),
            }
        } else {
            (
                pixel_buffer::bare_format_description(codec_type, config.width, config.height)?,
                false,
            )
        };

        let attributes = match config.pixel_format.and_then(pixel_buffer::pixel_format_type) {
            Some(kind) => Some(destination_attributes(kind)?),
            None => None,
        };

        let sink = Arc::new(DecompressionSink::new());
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(decompression_output),
            decompressionOutputRefCon: Arc::as_ptr(&sink).cast_mut().cast::<c_void>(),
        };
        let mut session: *mut VTDecompressionSession = std::ptr::null_mut();
        let Some(out) = NonNull::new(&mut session) else {
            return Err(Error::internal("the session out-parameter cannot be null"));
        };
        // SAFETY: `description` is live, `record` names a real callback and the live
        // `Arc` above, and `out` is a valid local.
        // `VTDecompressionSessionCreate` writes a +1 session reference into `out` on
        // success.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &description,
                None,
                attributes.as_ref().map(|dictionary| dictionary.as_opaque()),
                &record,
                out,
            )
        };
        if status != 0 {
            return Err(Error::backend(format!(
                "VTDecompressionSessionCreate({}{}) failed: {}",
                codec.name(),
                if attributes.is_some() { " with a requested output layout" } else { "" },
                os_status_text(status)
            )));
        }
        let Some(pointer) = NonNull::new(session) else {
            return Err(Error::backend(
                "VTDecompressionSessionCreate reported success but returned no session",
            ));
        };
        // SAFETY: follows the Create rule, so the reference carries +1.
        let session: CFRetained<VTDecompressionSession> = unsafe { CFRetained::from_raw(pointer) };

        let path = decoder_path(codec, hardware_decode_available(codec_type), from_extradata);
        Ok(Self { session, codec, path, description, sink })
    }

    /// Reads the queue the output callback filled.
    fn take_pending(&mut self) -> Result<Option<Frame>> {
        let mut state = self
            .sink
            .state
            .lock()
            .map_err(|_| Error::state("the VideoToolbox frame queue is poisoned"))?;
        if let Some(failure) = state.failure.take() {
            return Err(Error::backend(failure));
        }
        if state.saturated {
            state.saturated = false;
            return Err(Error::state(format!(
                "VideoToolbox decoded more than {MAX_PENDING_FRAMES} frames without receive() being \
                 called; the excess was dropped"
            )));
        }
        Ok(state.frames.pop_front())
    }
}

/// Builds the `destinationImageBufferAttributes` dictionary that asks the decoder
/// for a particular output layout.
///
/// An empty `IOSurfaceProperties` dictionary is included because VideoToolbox's
/// hardware decoders only return `CVPixelBuffer`s backed by an `IOSurface`.
fn destination_attributes(pixel_format_type: u32) -> Result<CFRetained<CFDictionary<CFType, CFType>>> {
    // SAFETY: reading framework constants that CoreVideo initialises.
    let (format_key, surface_key) = unsafe {
        (
            objc2_core_video::kCVPixelBufferPixelFormatTypeKey,
            objc2_core_video::kCVPixelBufferIOSurfacePropertiesKey,
        )
    };
    let number = CFNumber::new_i32(pixel_format_type as i32);
    // An empty `IOSurfaceProperties` dictionary: hardware decoders only hand back
    // `CVPixelBuffer`s backed by an `IOSurface`.
    let surface = CFDictionary::<CFString, CFType>::empty();
    let keys: [&CFType; 2] = [format_key, surface_key];
    let surface_value: &CFType = &surface;
    let values: [&CFType; 2] = [&number, surface_value];
    Ok(CFDictionary::<CFType, CFType>::from_slices(&keys, &values))
}

impl Decoder for VtDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_header() {
            // Decoder configuration reached the session through
            // `DecoderConfig::extradata`; a header packet adds nothing.
            return Ok(());
        }
        if packet.is_eos() {
            return self.flush().map(|_| ());
        }
        if packet.codec != self.codec {
            return Err(Error::invalid_argument(format!(
                "this {} decoder cannot take {} packets",
                self.codec.name(),
                packet.codec.name()
            )));
        }
        let payload = pixel_buffer::packet_payload(&packet)?;
        let sample =
            pixel_buffer::bytes_sample_buffer(&payload, &self.description, packet.pts, packet.duration)?;
        // SAFETY: the session is live and `sample` is a fully formed one-picture
        // sample buffer. Clearing the decode flags makes the decode synchronous, so
        // the callback has already run by the time this returns.
        let status = unsafe {
            self.session.decode_frame(
                &sample,
                VTDecodeFrameFlags(0),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(Error::backend(format!(
                "VTDecompressionSessionDecodeFrame(pts {}) failed: {}",
                packet.pts,
                os_status_text(status)
            )));
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        self.take_pending()
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        // SAFETY: finishing delayed frames and waiting for the asynchronous ones are
        // both no-ops on a session that never enabled temporal processing, and both
        // are safe on a live session.
        let finish = unsafe { self.session.finish_delayed_frames() };
        if finish != 0 {
            return Err(Error::backend(format!(
                "VTDecompressionSessionFinishDelayedFrames failed: {}",
                os_status_text(finish)
            )));
        }
        // SAFETY: as above.
        let wait = unsafe { self.session.wait_for_asynchronous_frames() };
        if wait != 0 {
            return Err(Error::backend(format!(
                "VTDecompressionSessionWaitForAsynchronousFrames failed: {}",
                os_status_text(wait)
            )));
        }
        let mut frames = Vec::new();
        while let Some(frame) = self.take_pending()? {
            frames.push(frame);
        }
        Ok(frames)
    }
}

// ---------------------------------------------------------------------------
// Paths, exposed so the tests can assert them without a media engine
// ---------------------------------------------------------------------------

/// The `CodecPath` a compression session reports for `codec` and an observed
/// hardware flag.
///
/// Also used by [`VtEncoder::new`], so the statement the tests check is the same
/// statement a caller sees.
#[must_use]
pub fn encoder_path(codec: Codec, hardware: Option<bool>) -> CodecPath {
    let detail = match hardware {
        Some(true) => format!(
            "VTCompressionSession {} (UsingHardwareAcceleratedVideoEncoder = true)",
            codec.name()
        ),
        Some(false) => format!(
            "VTCompressionSession {} (UsingHardwareAcceleratedVideoEncoder = false)",
            codec.name()
        ),
        None => format!(
            "VTCompressionSession {} (the encoder did not report \
             UsingHardwareAcceleratedVideoEncoder, so hardware is not claimed)",
            codec.name()
        ),
    };
    CodecPath::new(BackendKind::VideoToolbox, hardware.unwrap_or(false), detail)
}

/// The `CodecPath` a decompression session reports for `codec`, given the system's
/// own hardware answer and where the configuration came from.
#[must_use]
pub fn decoder_path(codec: Codec, hardware: bool, from_extradata: bool) -> CodecPath {
    CodecPath::new(
        BackendKind::VideoToolbox,
        hardware,
        format!(
            "VTDecompressionSession {} ({}, hardware decode: VTIsHardwareDecodeSupported = {})",
            codec.name(),
            if from_extradata { "parameter sets from extradata" } else { "configuration in band" },
            hardware
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::codec::AudioCodec;
    use crate::media::format::{ChannelLayout, SampleFormat};
    use crate::media::frame::AudioFrame;

    #[test]
    fn the_codec_lists_match_what_videotoolbox_can_do() {
        // H.264 and HEVC are the only picture codecs VideoToolbox encodes.
        assert_eq!(ENCODABLE.len(), 2);
        assert!(ENCODABLE.contains(&Codec::Video(VideoCodec::H264)));
        assert!(ENCODABLE.contains(&Codec::Video(VideoCodec::H265)));
        // Decoding covers every picture codec CoreMedia names.
        for codec in [VideoCodec::H264, VideoCodec::H265, VideoCodec::Vp9, VideoCodec::Av1, VideoCodec::H266]
        {
            assert!(DECODABLE.contains(&Codec::Video(codec)), "{codec} must be decodable");
        }
        // VP8 is in neither list: CoreMedia has no codec type for it.
        assert!(!ENCODABLE.contains(&Codec::Video(VideoCodec::Vp8)));
        assert!(!DECODABLE.contains(&Codec::Video(VideoCodec::Vp8)));
        // No audio codec belongs to VideoToolbox.
        for codec in DECODABLE.iter().chain(ENCODABLE.iter()) {
            assert!(codec.is_video(), "{codec} must be a picture codec");
        }
    }

    #[test]
    fn every_listed_codec_has_a_coremedia_codec_type() {
        for codec in ENCODABLE.iter().chain(DECODABLE.iter()) {
            assert!(super::super::video_codec_type(*codec).is_some(), "{codec}");
        }
    }

    #[test]
    fn the_input_layout_policy_is_the_three_four_two_zero_layouts() {
        assert!(VtEncoder::supports_input(PixelFormat::I420));
        assert!(VtEncoder::supports_input(PixelFormat::Nv12));
        assert!(VtEncoder::supports_input(PixelFormat::P010));
        assert!(!VtEncoder::supports_input(PixelFormat::Nv21));
        assert!(!VtEncoder::supports_input(PixelFormat::Rgba8));
        assert!(!VtEncoder::supports_input(PixelFormat::Bgra8));
        assert!(!VtEncoder::supports_input(PixelFormat::Yuy2));
    }

    #[test]
    fn the_encoder_path_reports_hardware_consistently_with_its_detail() {
        let codec = Codec::Video(VideoCodec::H264);
        for observed in [Some(true), Some(false), None] {
            let path = encoder_path(codec, observed);
            assert_eq!(path.backend, BackendKind::VideoToolbox);
            assert!(path.is_native());
            assert!(!path.is_software());
            assert_eq!(path.hardware_accelerated, observed.unwrap_or(false));
            assert!(path.detail.contains("h264"), "{}", path.detail);
            assert_eq!(path.detail.contains("= true"), path.hardware_accelerated, "{}", path.detail);
            assert_eq!(
                path.detail.contains("= false"),
                observed == Some(false),
                "{}",
                path.detail
            );
            if observed.is_none() {
                assert!(path.detail.contains("did not report"), "{}", path.detail);
                assert!(!path.hardware_accelerated, "an unanswered flag is not hardware");
            }
        }
    }

    #[test]
    fn the_decoder_path_names_videotoolbox_and_its_hardware_source() {
        let codec = Codec::Video(VideoCodec::H265);
        for hardware in [true, false] {
            for from_extradata in [true, false] {
                let path = decoder_path(codec, hardware, from_extradata);
                assert_eq!(path.backend, BackendKind::VideoToolbox);
                assert!(path.is_native());
                assert_eq!(path.hardware_accelerated, hardware);
                assert!(path.detail.contains("VTDecompressionSession"), "{}", path.detail);
                assert!(path.detail.contains("h265"), "{}", path.detail);
                assert!(
                    path.detail.contains("VTIsHardwareDecodeSupported"),
                    "the flag's source must be named: {}",
                    path.detail
                );
                let source = if from_extradata {
                    "parameter sets from extradata"
                } else {
                    "configuration in band"
                };
                assert!(path.detail.contains(source), "{}", path.detail);
            }
        }
    }

    #[test]
    fn the_derived_bitrate_follows_the_quality_hint() {
        let low = EncoderConfig::video(VideoCodec::H264, 1280, 720).with_bitrate(0).with_quality(0.05);
        let high = EncoderConfig::video(VideoCodec::H264, 1280, 720).with_bitrate(0).with_quality(1.0);
        let low = derive_bitrate(&low);
        let high = derive_bitrate(&high);
        assert!(low >= 50_000, "{low}");
        assert!(high > low, "{high} must exceed {low}");
        assert!(high <= 200_000_000, "{high}");
    }

    #[test]
    fn the_profile_level_option_selects_a_documented_constant() {
        // SAFETY: reading framework constants that VideoToolbox initialises.
        let (baseline, main, high, main10, hevc_main) = unsafe {
            (
                kVTProfileLevel_H264_Baseline_AutoLevel,
                kVTProfileLevel_H264_Main_AutoLevel,
                kVTProfileLevel_H264_High_AutoLevel,
                kVTProfileLevel_HEVC_Main10_AutoLevel,
                kVTProfileLevel_HEVC_Main_AutoLevel,
            )
        };
        let h264 = |profile: &str| {
            let config = EncoderConfig::video(VideoCodec::H264, 640, 480).with_option("profile", profile);
            profile_level_value(&config)
                .expect("H.264 has profile levels")
                .to_string()
        };
        assert_eq!(h264("baseline"), baseline.to_string());
        assert_eq!(h264("main"), main.to_string());
        assert_eq!(h264("high"), high.to_string());
        assert_eq!(h264("unknown"), high.to_string());
        assert_eq!(h264("BASELINE"), baseline.to_string());
        let h265 = |profile: &str| {
            let config = EncoderConfig::video(VideoCodec::H265, 640, 480).with_option("profile", profile);
            profile_level_value(&config)
                .expect("HEVC has profile levels")
                .to_string()
        };
        assert_eq!(h265("main10"), main10.to_string());
        assert_eq!(h265("main"), hevc_main.to_string());
    }

    #[test]
    fn a_profile_level_exists_for_every_encodable_codec_and_no_other() {
        for codec in ENCODABLE {
            let config = EncoderConfig::video(codec.as_video().expect("a picture codec"), 640, 480);
            assert!(profile_level_value(&config).is_ok(), "{codec}");
        }
        for codec in Codec::ALL {
            if ENCODABLE.contains(&codec) {
                continue;
            }
            let config = match codec {
                Codec::Video(video) => EncoderConfig::video(video, 640, 480),
                Codec::Audio(audio) => EncoderConfig::audio(audio, 48_000, ChannelLayout::Stereo),
            };
            assert!(profile_level_value(&config).is_err(), "{codec} must have no profile level");
        }
    }

    #[test]
    fn probing_a_non_video_codec_answers_with_a_specific_reason() {
        for codec in Codec::ALL {
            if codec.is_video() {
                continue;
            }
            for kind in CodecKind::ALL {
                let availability = probe_component(codec, kind);
                let reason = availability.reason().expect("audio is never a VideoToolbox codec");
                assert!(reason.contains("AudioToolbox"), "{reason}");
            }
        }
    }

    #[test]
    fn probing_vp8_answers_with_a_specific_reason() {
        let availability = probe_component(Codec::Video(VideoCodec::Vp8), CodecKind::Decoder);
        let reason = availability.reason().expect("vp8 has no CoreMedia codec type");
        assert!(reason.contains("CoreMedia"), "{reason}");
        assert!(reason.contains("vp8"), "{reason}");
    }

    #[test]
    fn probing_an_unencodable_picture_codec_names_the_codec() {
        let availability = probe_component(Codec::Video(VideoCodec::Av1), CodecKind::Encoder);
        let reason = availability.reason().expect("av1 has no VideoToolbox encoder");
        assert!(reason.contains("AV1"), "{reason}");
    }

    #[test]
    fn the_preset_selects_realtime_and_frame_reordering_hints() {
        // The two hints are derived from the preset: `speed` means no reordering.
        let speed = EncoderConfig::video(VideoCodec::H264, 640, 480).with_preset(Preset::Speed);
        let quality = EncoderConfig::video(VideoCodec::H264, 640, 480).with_preset(Preset::Quality);
        assert!(CFBoolean::new(speed.preset == Preset::Speed).as_bool());
        assert!(!CFBoolean::new(quality.preset == Preset::Speed).as_bool());
        assert!(CFBoolean::new(quality.preset != Preset::Speed).as_bool());
    }

    #[test]
    fn the_frame_mismatch_diagnostics_exist_for_audio_input() {
        // The video sessions reject audio; the frame model has to be able to say so.
        let frame = Frame::Audio(
            AudioFrame::new(
                SampleFormat::F32,
                48_000,
                ChannelLayout::Stereo,
                1,
                Buffer::from_slice(&[0u8; 8]),
                0,
            )
            .expect("a one-frame stereo block"),
        );
        assert!(frame.is_audio());
        assert!(frame.as_video().is_none());
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn videotoolbox_is_available_and_lists_an_h264_encoder() {
        assert!(available(), "VideoToolbox must be reachable on macOS");
        let encoders = encoder_list().expect("VTCopyVideoEncoderList must succeed");
        assert!(!encoders.is_empty(), "a stock macOS lists at least one encoder");
        assert!(
            encoders
                .iter()
                .any(|encoder| encoder.codec_type == objc2_core_media::kCMVideoCodecType_H264),
            "{encoders:?}"
        );
        // H.264 decoding is inbox on every macOS that ships VideoToolbox.
        let decodes = decoder_available(objc2_core_media::kCMVideoCodecType_H264)
            .expect("the decoder probe must answer, not fail");
        assert!(decodes, "H.264 must be decodable on macOS");
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn registration_adds_exactly_the_codecs_videotoolbox_serves() {
        let mut registry = Registry::new();
        register(&mut registry);
        for codec in ENCODABLE {
            if probe_component(codec, CodecKind::Encoder).is_available() {
                let report = registry.capability(codec).expect("a registered codec is reported");
                assert!(
                    report.encoders.iter().any(|backend| backend.backend == BackendKind::VideoToolbox),
                    "{codec} lost its encoder registration"
                );
            }
        }
        for codec in DECODABLE {
            if probe_component(codec, CodecKind::Decoder).is_available() {
                let report = registry.capability(codec).expect("a registered codec is reported");
                assert!(
                    report.decoders.iter().any(|backend| backend.backend == BackendKind::VideoToolbox),
                    "{codec} lost its decoder registration"
                );
            }
        }
        // And nothing outside those lists is claimed.
        for codec in Codec::ALL {
            let report = registry.capability(codec).expect("every codec is reported");
            if !ENCODABLE.contains(&codec) {
                assert!(
                    !report.encoders.iter().any(|backend| backend.backend == BackendKind::VideoToolbox),
                    "{codec} must not have a VideoToolbox encoder"
                );
            }
            if !DECODABLE.contains(&codec) {
                assert!(
                    !report.decoders.iter().any(|backend| backend.backend == BackendKind::VideoToolbox),
                    "{codec} must not have a VideoToolbox decoder"
                );
            }
        }
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn a_real_h264_encode_produces_a_packet_and_reports_hardware_honestly() {
        let config = EncoderConfig::video(VideoCodec::H264, 64, 64)
            .with_framerate(30, 1)
            .with_bitrate(400_000);
        let mut encoder = VtEncoder::new(&config).expect("the H.264 compression session must open");
        let luma = crate::media::frame::Plane::rows(Buffer::from_vec(vec![16u8; 64 * 64]), 64)
            .expect("64-byte rows");
        let chroma = crate::media::frame::Plane::rows(Buffer::from_vec(vec![128u8; 32 * 64]), 64)
            .expect("64-byte rows");
        let frame = VideoFrame::new(PixelFormat::Nv12, 64, 64, vec![luma, chroma], 0)
            .expect("a valid NV12 frame");
        let mut packets = Vec::new();
        for index in 0..8 {
            let mut shifted = frame.clone();
            shifted.pts = index * 33;
            encoder.push(Frame::Video(shifted)).expect("the session must accept the picture");
            while let Some(packet) = encoder.receive().expect("receive must not fail") {
                packets.push(packet);
            }
            if !packets.is_empty() {
                break;
            }
        }
        packets.extend(encoder.flush().expect("flush must not fail"));
        assert!(!packets.is_empty(), "a real H.264 encode produced no output at all");
        assert!(packets.iter().any(|packet| !packet.is_empty()), "every packet was empty");
        let path = encoder.path();
        assert_eq!(path.backend, BackendKind::VideoToolbox);
        assert!(
            path.detail.contains("UsingHardwareAcceleratedVideoEncoder"),
            "the hardware flag's source must be named: {}",
            path.detail
        );
        // The output must be Annex-B, like every other backend in this crate.
        assert!(packets.iter().all(|packet| packet.data.starts_with(&[0, 0, 0, 1])
            || packet.data.starts_with(&[0, 0, 1])));
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn a_real_h264_decode_session_opens_and_drains_empty() {
        let config = DecoderConfig::new(Codec::Video(VideoCodec::H264)).with_size(64, 64);
        let mut decoder = VtDecoder::new(&config).expect("the H.264 decompression session must open");
        // No packets: the session must simply produce nothing rather than fail.
        assert!(decoder.receive().expect("receive must not fail").is_none());
        assert!(decoder.flush().expect("flush must not fail").is_empty());
        assert_eq!(decoder.path().backend, BackendKind::VideoToolbox);
    }

    #[test]
    fn an_audio_codec_cannot_be_smuggled_into_the_video_lists() {
        for codec in [AudioCodec::Aac, AudioCodec::Alac, AudioCodec::Mp3] {
            assert!(!ENCODABLE.contains(&Codec::Audio(codec)));
            assert!(!DECODABLE.contains(&Codec::Audio(codec)));
        }
    }
}
