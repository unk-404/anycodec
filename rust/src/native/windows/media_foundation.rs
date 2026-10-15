//! The Media Foundation half of the Windows backend.
//!
//! Media Foundation exposes codecs as *Media Foundation Transforms* (MFTs): COM
//! objects that negotiate a pair of media types and then run a synchronous
//! push/pull loop. That maps onto [`Encoder`] / [`Decoder`] almost exactly, which
//! is why this file is mostly negotiation and marshalling rather than codec work.
//!
//! ## Why transforms are enumerated, not instantiated by CLSID
//!
//! The classic route is `CoCreateInstance(CLSID_CMSH264DecoderMFT, ...)`. It has
//! two problems: the `windows` crate declares none of those `CLSID_CMS*` GUIDs,
//! so they would have to be hard-coded (a maintenance trap, and `CLSID_CMSH265*`
//! and friends are not even documented); and it silently pins the backend to the
//! Microsoft implementation, ignoring vendor MFTs that may be present.
//!
//! `MFTEnumEx` is the documented alternative. This module enumerates the right
//! category, activates each candidate, and keeps the first transform whose input
//! types advertise the codec's subtype. That is more code, but it is honest: the
//! capability report says "Media Foundation has no AV1 decoder on this machine"
//! because enumeration really came up empty, not because a GUID was wrong.
//!
//! ## Hardware acceleration
//!
//! Hardware MFTs are *asynchronous* transforms: they must be unlocked with
//! `MF_TRANSFORM_ASYNC_UNLOCK` and driven through `IMFMediaEventGenerator`. This
//! backend enumerates synchronous transforms only, so
//! [`CodecPath::hardware_accelerated`] is reported as `false` everywhere. That is
//! the truth 鈥?the encode runs on the CPU 鈥?and claiming otherwise would be the
//! exact kind of silent lie [`CodecPath`] exists to prevent.

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaType, IMFSample, IMFTransform, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFMediaType_Audio, MFMediaType_Video, MFSTARTUP_FULL, MFSampleExtension_CleanPoint,
    MFT_CATEGORY_AUDIO_DECODER, MFT_CATEGORY_AUDIO_ENCODER, MFT_CATEGORY_VIDEO_DECODER,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_ENUM_FLAG_SYNCMFT, MFT_ENUM_FLAG_TRANSCODE_ONLY, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_OUTPUT_DATA_BUFFER, MFTEnumEx, MFStartup, MFVideoFormat_AV1, MFVideoFormat_H264,
    MFVideoFormat_HEVC, MFVideoFormat_VP80, MFVideoFormat_VP90, MF_MT_ALL_SAMPLES_INDEPENDENT,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_VERSION, MFVideoInterlace_Progressive,
    MFAudioFormat_AAC, MFAudioFormat_ALAC, MFAudioFormat_FLAC, MFAudioFormat_MP3,
    MFAudioFormat_Opus, MFAudioFormat_PCM, MFAudioFormat_Vorbis,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::core::GUID;

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, CodecKind, HwCodecSupport, HwDeviceInfo, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::{Buffer, BufferBuilder};
use crate::media::codec::{AudioCodec, Codec, VideoCodec};
use crate::media::config::{DecoderConfig, EncoderConfig};
use crate::media::format::{ChannelLayout, SampleFormat};
use crate::media::frame::{AudioFrame, Frame, Plane, VideoFrame};
use crate::media::packet::{flags, Packet};
use crate::native::bitstream;

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// Starts Media Foundation once per process.
///
/// `CoInitializeEx` may legitimately fail with `RPC_E_CHANGED_MODE` when the
/// host already put this thread in a single-threaded apartment; that is not an
/// error for us, so the result is ignored and only `MFStartup` decides.
fn startup() -> std::result::Result<(), String> {
    static STATE: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            // SAFETY: MFStartup/CoInitializeEx are process-wide initialisers with
            // no arguments that can outlive anything here.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_FULL)
                    .map_err(|error| format!("Media Foundation could not start: {error}"))
            }
        })
        .clone()
}

/// `true` when Media Foundation started successfully.
#[must_use]
pub fn framework_available() -> bool {
    startup().is_ok()
}

// ---------------------------------------------------------------------------
// Codec tables
// ---------------------------------------------------------------------------

/// The `MF_MT_SUBTYPE` Media Foundation uses for `codec`.
#[must_use]
pub fn codec_subtype(codec: Codec) -> Option<GUID> {
    Some(match codec {
        Codec::Video(VideoCodec::H264) => MFVideoFormat_H264,
        Codec::Video(VideoCodec::H265) => MFVideoFormat_HEVC,
        // Windows 10/11 build HEVC in through an optional "HEVC Video Extensions"
        // package; VVC has no inbox MFT at all on any shipping Windows.
        Codec::Video(VideoCodec::H266) => return None,
        Codec::Video(VideoCodec::Vp8) => MFVideoFormat_VP80,
        Codec::Video(VideoCodec::Vp9) => MFVideoFormat_VP90,
        Codec::Video(VideoCodec::Av1) => MFVideoFormat_AV1,
        Codec::Audio(AudioCodec::Mp3) => MFAudioFormat_MP3,
        Codec::Audio(AudioCodec::Aac) => MFAudioFormat_AAC,
        Codec::Audio(AudioCodec::Vorbis) => MFAudioFormat_Vorbis,
        Codec::Audio(AudioCodec::Opus) => MFAudioFormat_Opus,
        Codec::Audio(AudioCodec::Flac) => MFAudioFormat_FLAC,
        Codec::Audio(AudioCodec::Alac) => MFAudioFormat_ALAC,
    })
}

/// The MFT category for a codec and direction.
#[must_use]
pub fn category_for(codec: Codec, kind: CodecKind) -> GUID {
    match (codec.is_video(), kind) {
        (true, CodecKind::Encoder) => MFT_CATEGORY_VIDEO_ENCODER,
        (true, CodecKind::Decoder) => MFT_CATEGORY_VIDEO_DECODER,
        (false, CodecKind::Encoder) => MFT_CATEGORY_AUDIO_ENCODER,
        (false, CodecKind::Decoder) => MFT_CATEGORY_AUDIO_DECODER,
    }
}

/// The raw pixel subtypes this backend can both feed and read, best first.
///
/// `NV12` is the layout every Media Foundation video path is happiest with;
/// `IYUV` (planar 4:2:0, U before V) is the fallback. `YV12` is deliberately
/// absent: it is the same geometry with the chroma planes swapped, and silently
/// swapping them is exactly the kind of bug that produces a green picture.
#[must_use]
pub fn video_subtype_candidates() -> [(GUID, crate::media::format::PixelFormat); 2] {
    [
        (
            windows::Win32::Media::MediaFoundation::MFVideoFormat_NV12,
            crate::media::format::PixelFormat::Nv12,
        ),
        (
            windows::Win32::Media::MediaFoundation::MFVideoFormat_IYUV,
            crate::media::format::PixelFormat::I420,
        ),
    ]
}

fn pack_ratio(numerator: u32, denominator: u32) -> u64 {
    ((numerator as u64) << 32) | denominator.max(1) as u64
}

// ---------------------------------------------------------------------------
// Transform discovery and negotiation
// ---------------------------------------------------------------------------

/// Activates the first synchronous MFT in `category` that handles `codec`.
///
/// The direction matters and is easy to get backwards: a **decoder** takes the
/// compressed subtype as its *input* and produces raw frames, while an
/// **encoder** takes raw frames as its input and produces the compressed subtype
/// as its *output*. So the codec subtype is matched against the input stream for
/// a decoder and against the output stream for an encoder.
fn find_transform(codec: Codec, kind: CodecKind) -> Result<IMFTransform> {
    startup().map_err(Error::unavailable)?;
    let Some(subtype) = codec_subtype(codec) else {
        return Err(Error::unavailable(format!(
            "Media Foundation has no transform for {} on any shipping Windows",
            codec.name()
        )));
    };
    let category = category_for(codec, kind);
        // `MFT_ENUM_FLAG_TRANSCODE_ONLY` matters: Windows registers the inbox H.264
    // and AAC *encoders* under that flag, so omitting it makes a stock machine
    // look like it cannot encode at all.
    let flags = MFT_ENUM_FLAG_SYNCMFT
        | MFT_ENUM_FLAG_TRANSCODE_ONLY
        | MFT_ENUM_FLAG_SORTANDFILTER
        | MFT_ENUM_FLAG_LOCALMFT;

    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: `MFTEnumEx` writes a CoTaskMem-allocated array of `count` interface
    // pointers into `activates`; both out-parameters are valid locals.
    unsafe { MFTEnumEx(category, flags, None, None, &mut activates, &mut count) }
        .map_err(|error| Error::backend(format!("MFTEnumEx failed: {error}")))?;

    if activates.is_null() || count == 0 {
        return Err(Error::unavailable(format!(
            "Media Foundation has no {} {} component installed",
            codec.name(),
            kind.name()
        )));
    }

    // SAFETY: `MFTEnumEx` guarantees `count` initialised entries at `activates`.
    let candidates = unsafe { std::slice::from_raw_parts(activates, count as usize) };
    let mut found = None;
    for candidate in candidates.iter().flatten() {
        // SAFETY: `ActivateObject` is safe to call on a live IMFActivate; a
        // failure is reported through the Result.
        let Ok(transform) = (unsafe { candidate.ActivateObject::<IMFTransform>() }) else {
            continue;
        };
        if transform_handles(&transform, kind, subtype) {
            found = Some(transform);
            break;
        }
    }
    // SAFETY: the array came from `MFTEnumEx`, which documents CoTaskMemFree as
    // the matching deallocator.
    unsafe { CoTaskMemFree(Some(activates as *const core::ffi::c_void)) };

    found.ok_or_else(|| {
        Error::unavailable(format!(
            "Media Foundation has {} transforms but none accepts {} ({kind})",
            count,
            codec.name()
        ))
    })
}

/// `true` when `transform` handles `subtype` in the direction `kind` implies.
fn transform_handles(transform: &IMFTransform, kind: CodecKind, subtype: GUID) -> bool {
    for index in 0..64u32 {
        // SAFETY: index-bounded enumeration; the first error means "no more
        // types", which is the documented way to stop.
        let media_type = match kind {
            // An encoder advertises the compressed subtype on its output side.
            CodecKind::Encoder => unsafe { transform.GetOutputAvailableType(0, index) },
            // A decoder advertises it on its input side.
            CodecKind::Decoder => unsafe { transform.GetInputAvailableType(0, index) },
        };
        let Ok(media_type) = media_type else {
            break;
        };
        // SAFETY: `media_type` is a live IMFMediaType.
        if unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(subtype) {
            return true;
        }
    }
    false
}

/// Picks and installs an output type whose subtype is one of `candidates`.
///
/// Returns the index of the winning candidate. Media Foundation order is
/// preserved, so passing the candidates in preference order is enough.
fn negotiate_output_type(
    transform: &IMFTransform,
    candidates: &[GUID],
) -> std::result::Result<usize, windows::core::Error> {
    for index in 0..64u32 {
        let media_type = unsafe { transform.GetOutputAvailableType(0, index) }?;
        let Ok(subtype) = (unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }) else {
            continue;
        };
        if let Some(position) = candidates.iter().position(|candidate| *candidate == subtype) {
            unsafe { transform.SetOutputType(0, &media_type, 0) }?;
            return Ok(position);
        }
    }
    Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))
}

fn video_media_type(
    subtype: GUID,
    width: u32,
    height: u32,
    framerate_num: u32,
    framerate_den: u32,
    bitrate: u64,
) -> windows::core::Result<IMFMediaType> {
    // SAFETY: every call below is a documented COM setter on a live media type.
    unsafe {
        let media_type = MFCreateMediaType()?;
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(width, height))?;
        media_type.SetUINT64(
            &MF_MT_FRAME_RATE,
            pack_ratio(framerate_num.max(1), framerate_den.max(1)),
        )?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
        if bitrate > 0 {
            media_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate.min(u32::MAX as u64) as u32)?;
        }
        Ok(media_type)
    }
}

fn audio_media_type(
    subtype: GUID,
    sample_rate: u32,
    channels: u32,
    bits_per_sample: u32,
    bytes_per_second: u32,
) -> windows::core::Result<IMFMediaType> {
    // SAFETY: as above.
    unsafe {
        let media_type = MFCreateMediaType()?;
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        media_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
        media_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
        media_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, bits_per_sample)?;
        media_type.SetUINT32(
            &MF_MT_AUDIO_BLOCK_ALIGNMENT,
            channels * (bits_per_sample / 8),
        )?;
        media_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_second)?;
        media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        Ok(media_type)
    }
}

// ---------------------------------------------------------------------------
// Sample marshalling
// ---------------------------------------------------------------------------

/// Wraps `bytes` in an `IMFSample` stamped with `pts_us` microseconds.
fn sample_from_bytes(bytes: &[u8], pts_us: i64, duration_us: i64) -> Result<IMFSample> {
    // SAFETY: all three are documented Media Foundation 1:1 constructors.
    unsafe {
        let buffer = MFCreateMemoryBuffer(bytes.len().max(1) as u32)
            .map_err(|error| Error::backend(format!("MFCreateMemoryBuffer failed: {error}")))?;
        let mut pointer = std::ptr::null_mut();
        let mut capacity = 0u32;
        buffer
            .Lock(&mut pointer, Some(&mut capacity), None)
            .map_err(|error| Error::backend(format!("IMFMediaBuffer::Lock failed: {error}")))?;
        if pointer.is_null() || (capacity as usize) < bytes.len() {
            let _ = buffer.Unlock();
            return Err(Error::backend("Media Foundation handed back a short buffer"));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        buffer
            .Unlock()
            .map_err(|error| Error::backend(format!("IMFMediaBuffer::Unlock failed: {error}")))?;
        buffer
            .SetCurrentLength(bytes.len() as u32)
            .map_err(|error| Error::backend(format!("SetCurrentLength failed: {error}")))?;

        let sample = MFCreateSample()
            .map_err(|error| Error::backend(format!("MFCreateSample failed: {error}")))?;
        sample
            .AddBuffer(&buffer)
            .map_err(|error| Error::backend(format!("IMFSample::AddBuffer failed: {error}")))?;
        // Media Foundation times are 100-nanosecond units.
        sample
            .SetSampleTime(pts_us.saturating_mul(10))
            .map_err(|error| Error::backend(format!("SetSampleTime failed: {error}")))?;
        if duration_us > 0 {
            let _ = sample.SetSampleDuration(duration_us.saturating_mul(10));
        }
        Ok(sample)
    }
}

/// Copies the payload of `sample` out, and reports whether it is a keyframe.
fn bytes_from_sample(sample: &IMFSample) -> Result<(Vec<u8>, i64, bool)> {
    // SAFETY: `ConvertToContiguousBuffer` / `Lock` are the documented readers.
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|error| Error::backend(format!("ConvertToContiguousBuffer failed: {error}")))?;
        let mut pointer = std::ptr::null_mut();
        let mut length = 0u32;
        buffer
            .Lock(&mut pointer, None, Some(&mut length))
            .map_err(|error| Error::backend(format!("IMFMediaBuffer::Lock failed: {error}")))?;
        let mut payload = Vec::with_capacity(length as usize);
        if !pointer.is_null() {
            payload.extend_from_slice(std::slice::from_raw_parts(pointer, length as usize));
        }
        buffer
            .Unlock()
            .map_err(|error| Error::backend(format!("IMFMediaBuffer::Unlock failed: {error}")))?;

        let pts = sample
            .GetSampleTime()
            .map(|value| value / 10)
            .unwrap_or_default();
        // `MFSampleExtension_CleanPoint` is how Media Foundation marks a random
        // access point; absence means "not a keyframe", never "unknown".
        let keyframe = sample
            .GetUINT32(&MFSampleExtension_CleanPoint)
            .map(|value| value != 0)
            .unwrap_or(false);
        Ok((payload, pts, keyframe))
    }
}

/// Writes `sample`'s payload into `sink`, returning `false` when there is nothing.
fn take_output(transform: &IMFTransform, sink: &mut VecDeque<(Vec<u8>, i64, bool)>) -> Result<bool> {
    // SAFETY: `GetOutputStreamInfo` describes the buffer Media Foundation wants.
    let stream_info = unsafe { transform.GetOutputStreamInfo(0) }
        .map_err(|error| Error::backend(format!("GetOutputStreamInfo failed: {error}")))?;
    // SAFETY: sample/buffer construction, documented 1:1.
    let sample = unsafe {
        let sample = MFCreateSample()
            .map_err(|error| Error::backend(format!("MFCreateSample failed: {error}")))?;
        let buffer = MFCreateMemoryBuffer(stream_info.cbSize.max(1)).map_err(|error| {
            Error::backend(format!("MFCreateMemoryBuffer failed: {error}"))
        })?;
        sample
            .AddBuffer(&buffer)
            .map_err(|error| Error::backend(format!("IMFSample::AddBuffer failed: {error}")))?;
        sample
    };

    let mut output = [MFT_OUTPUT_DATA_BUFFER {
        dwStreamID: 0,
        pSample: ManuallyDrop::new(Some(sample)),
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
    }];
    let mut status = 0u32;
    // SAFETY: `output` is a single, fully initialised buffer entry.
    match unsafe { transform.ProcessOutput(0, &mut output, &mut status) } {
        Ok(()) => {}
        Err(error)
            if error.code()
                == windows::Win32::Media::MediaFoundation::MF_E_TRANSFORM_NEED_MORE_INPUT =>
        {
            return Ok(false);
        }
        Err(error) if error.code() == windows::Win32::Media::MediaFoundation::MF_E_TRANSFORM_STREAM_CHANGE => {
            // The transform wants to renegotiate; the caller handles it.
            return Err(Error::state("mf: the transform asked to renegotiate its output type"));
        }
        Err(error) => {
            return Err(Error::backend(format!("ProcessOutput failed: {error}")));
        }
    }

    let produced = {
        let produced = &output[0];
        // SAFETY: `pSample` is ManuallyDrop; dereferencing without taking
        // ownership is exactly what ManuallyDrop is for here.
        let inner = unsafe { ManuallyDrop::into_inner(std::ptr::read(&produced.pSample)) };
        match inner {
            Some(sample) => Some(bytes_from_sample(&sample)?),
            None => None,
        }
    };
    // SAFETY: the same ManuallyDrop has already been consumed above, so only the
    // empty slot is dropped here.
    unsafe {
        std::ptr::drop_in_place(&mut output[0].pEvents);
        std::ptr::write(&mut output[0].pEvents, ManuallyDrop::new(None));
    }
    if let Some(entry) = produced {
        sink.push_back(entry);
        Ok(true)
    } else {
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// Frame conversion
// ---------------------------------------------------------------------------

/// Serialises a picture for an NV12 or I420 input.
fn video_payload(pixel_format: crate::media::format::PixelFormat, frame: &VideoFrame) -> Result<Vec<u8>> {
    if frame.format != pixel_format {
        return Err(Error::invalid_argument(format!(
            "the Media Foundation transform wants {} but received {}",
            pixel_format.name(),
            frame.format.name()
        )));
    }
    let mut builder = BufferBuilder::with_capacity(frame.byte_len());
    for index in 0..frame.format.plane_count() {
        let (width, height) = frame.format.plane_size(frame.width, frame.height, index)?;
        let row_bytes = frame.format.default_stride(width, 0)?;
        let Some(plane) = frame.planes.get(index) else {
            return Err(Error::invalid_argument(format!("plane {index} is missing")));
        };
        for row in 0..height as usize {
            builder.push(plane.row(row, row_bytes)?);
        }
    }
    Ok(builder.finish().to_vec())
}

/// Splits a decoded payload into planes of `pixel_format`.
fn planes_from_payload(
    pixel_format: crate::media::format::PixelFormat,
    width: u32,
    height: u32,
    payload: &[u8],
) -> Result<Vec<Plane>> {
    let mut planes = Vec::with_capacity(pixel_format.plane_count());
    let mut cursor = 0usize;
    for index in 0..pixel_format.plane_count() {
        let (plane_width, plane_height) = pixel_format.plane_size(width, height, index)?;
        let stride = pixel_format.default_stride(plane_width, 0)?;
        let end = cursor + stride * plane_height as usize;
        let data = if end <= payload.len() {
            &payload[cursor..end]
        } else {
            &payload[cursor.min(payload.len())..]
        };
        cursor = end;
        planes.push(Plane::new(Buffer::from_slice(data), stride)?);
    }
    Ok(planes)
}

/// The `(sample_rate, channels)` Media Foundation reports on a type.
fn audio_geometry(media_type: &IMFMediaType) -> (u32, u32) {
    // SAFETY: read-only attribute getters on a live media type.
    let sample_rate = unsafe { media_type.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }
        .unwrap_or(48_000)
        .max(1);
    let channels = unsafe { media_type.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }
        .unwrap_or(2)
        .max(1);
    (sample_rate, channels)
}

fn f32_to_s16(payload: &[u8]) -> Vec<u8> {
    let mut builder = BufferBuilder::with_capacity(payload.len() / 2);
    for chunk in payload.chunks_exact(4) {
        let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let scaled = (value.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        builder.push(&scaled.to_le_bytes());
    }
    builder.finish().to_vec()
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// A Media Foundation encoder session.
pub struct MfEncoder {
    transform: IMFTransform,
    codec: Codec,
    path: CodecPath,
    video_input: crate::media::format::PixelFormat,
    /// Duration to stamp on a picture that carries none of its own. Media
    /// Foundation's video encoders reject a sample without one
    /// (`MF_E_NO_SAMPLE_DURATION`), so the configured frame rate has to supply it.
    frame_duration_us: i64,
    pending: VecDeque<Packet>,
    draining: bool,
}

// SAFETY: an `IMFTransform` is a COM object with no thread affinity that AnyCodec
// only ever touches from the one isolate thread that owns the session. The
// `windows` crate types it as `!Send` because a raw pointer could in principle be
// shared; this session model never shares it, moves it exactly once (to a Dart
// worker isolate) before first use, and frees it on that same thread.
unsafe impl Send for MfEncoder {}

impl std::fmt::Debug for MfEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfEncoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .finish()
    }
}

impl MfEncoder {
    /// Creates and starts an encoder for `config`.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        let codec = config.codec;
        let transform = find_transform(codec, CodecKind::Encoder)?;
        let path = CodecPath::new(
            BackendKind::MediaFoundation,
            false,
            format!("mediafoundation: {codec} encoder (synchronous MFT)"),
        );

        if codec.is_video() {
            let subtype = codec_subtype(codec).expect("checked by find_transform");
            let output = video_media_type(
                subtype,
                config.width,
                config.height,
                config.framerate_num,
                config.framerate_den,
                config.bitrate,
            )
            .map_err(|error| Error::backend(format!("building the output type failed: {error}")))?;
            // SAFETY: documented COM negotiation calls.
            unsafe { transform.SetOutputType(0, &output, 0) }.map_err(|error| {
                Error::backend(format!("the encoder rejected its output type: {error}"))
            })?;

            let candidates = video_subtype_candidates();
            let mut chosen = None;
            for (subtype, pixel_format) in candidates {
                let Ok(input) = video_media_type(
                    subtype,
                    config.width,
                    config.height,
                    config.framerate_num,
                    config.framerate_den,
                    0,
                ) else {
                    continue;
                };
                // SAFETY: as above.
                if unsafe { transform.SetInputType(0, &input, 0) }.is_ok() {
                    chosen = Some(pixel_format);
                    break;
                }
            }
            let video_input = chosen.ok_or_else(|| {
                Error::unavailable(format!(
                    "the Media Foundation {codec} encoder accepts neither NV12 nor I420 input"
                ))
            })?;
            start_streaming(&transform)?;
            let frame_duration_us = config
                .framerate()
                .filter(|rate| *rate > 0.0)
                .map(|rate| (1_000_000.0 / rate) as i64)
                .unwrap_or(33_333);
            return Ok(Self {
                transform,
                codec,
                path,
                video_input,
                frame_duration_us,
                pending: VecDeque::new(),
                draining: false,
            });
        }

        let subtype = codec_subtype(codec).expect("checked by find_transform");
        let channels = config.channels.channels();
        let output = audio_media_type(
            subtype,
            config.sample_rate,
            channels,
            16,
            ((config.bitrate / 8).min(u32::MAX as u64)) as u32,
        )
        .map_err(|error| Error::backend(format!("building the output type failed: {error}")))?;
        // SAFETY: documented COM negotiation call.
        unsafe { transform.SetOutputType(0, &output, 0) }
            .map_err(|error| Error::backend(format!("the encoder rejected its output type: {error}")))?;

        let input = audio_media_type(
            MFAudioFormat_PCM,
            config.sample_rate,
            channels,
            16,
            config.sample_rate * channels * 2,
        )
        .map_err(|error| Error::backend(format!("building the input type failed: {error}")))?;
        // SAFETY: as above.
        unsafe { transform.SetInputType(0, &input, 0) }.map_err(|error| {
            Error::backend(format!("the encoder rejected 16-bit PCM input: {error}"))
        })?;
        start_streaming(&transform)?;
        Ok(Self {
            transform,
            codec,
            path,
            video_input: crate::media::format::PixelFormat::Nv12,
            frame_duration_us: 0,
            pending: VecDeque::new(),
            draining: false,
        })
    }

    fn drain(&mut self) -> Result<()> {
        let mut produced = VecDeque::new();
        loop {
            match take_output(&self.transform, &mut produced) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(error) if error.kind() == crate::error::ErrorKind::State => {
                    // The transform wants a new output type; accept its first
                    // advertised one and carry on.
                    let candidates: Vec<GUID> = video_subtype_candidates()
                        .iter()
                        .map(|(subtype, _)| *subtype)
                        .chain([MFAudioFormat_PCM])
                        .collect();
                    match negotiate_output_type(&self.transform, &candidates) {
                        Ok(_) => continue,
                        Err(inner) => {
                            return Err(Error::backend(format!(
                                "the transform asked to renegotiate but offered nothing usable: {inner}"
                            )));
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        }
        for (payload, pts, keyframe) in produced {
            let mut packet_flags = 0u32;
            if keyframe {
                packet_flags |= flags::KEYFRAME;
            }
            self.pending.push_back(Packet::new(
                self.codec,
                Buffer::from_vec(payload),
                pts,
                packet_flags,
            ));
        }
        Ok(())
    }
}

fn start_streaming(transform: &IMFTransform) -> Result<()> {
    // SAFETY: documented stream-control messages; `ulParam` is unused for these.
    unsafe {
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(|error| Error::backend(format!("BEGIN_STREAMING failed: {error}")))?;
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(|error| Error::backend(format!("START_OF_STREAM failed: {error}")))?;
    }
    Ok(())
}

impl Encoder for MfEncoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        if self.draining {
            return Err(Error::state("mf: the encoder is already draining"));
        }
        let (payload, pts_us, duration_us) = match frame {
            Frame::Video(video) => {
                let duration =
                    if video.duration > 0 { video.duration } else { self.frame_duration_us };
                (video_payload(self.video_input, &video)?, video.pts, duration)
            }
            Frame::Audio(audio) => {
                let samples = match audio.format {
                    SampleFormat::S16 => audio
                        .samples
                        .as_slice()
                        .get(..audio.channels.byte_len(audio.frames, SampleFormat::S16))
                        .ok_or_else(|| Error::backend("the audio frame is shorter than it claims"))?
                        .to_vec(),
                    SampleFormat::F32 => {
                        let bytes = audio
                            .samples
                            .as_slice()
                            .get(..audio.channels.byte_len(audio.frames, SampleFormat::F32))
                            .ok_or_else(|| {
                                Error::backend("the audio frame is shorter than it claims")
                            })?;
                        f32_to_s16(bytes)
                    }
                    other => {
                        return Err(Error::unsupported(format!(
                            "the Media Foundation encoder cannot take {} samples; use s16 or f32",
                            other.name()
                        )));
                    }
                };
                let duration =
                    audio.frames as i64 * 1_000_000 / audio.sample_rate.max(1) as i64;
                (samples, audio.pts, duration)
            }
        };
        let sample = sample_from_bytes(&payload, pts_us, duration_us)?;
        // SAFETY: `transform` is live and `sample` is a fully formed IMFSample.
        unsafe { self.transform.ProcessInput(0, &sample, 0) }
            .map_err(|error| Error::backend(format!("ProcessInput failed: {error}")))?;
        self.drain()
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        if self.pending.is_empty() {
            self.drain()?;
        }
        Ok(self.pending.pop_front())
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        self.draining = true;
        // SAFETY: documented drain sequence.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        }
        let _ = self.drain();
        Ok(self.pending.drain(..).collect())
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        if !self.codec.is_video() {
            return Err(Error::unsupported("mf: audio bitrate cannot be retuned live"));
        }
        // SAFETY: getting the current output type and re-setting it with a new
        // average bitrate is the documented way to retune a video encoder MFT.
        let media_type = unsafe { self.transform.GetOutputCurrentType(0) }
            .map_err(|error| Error::backend(format!("GetOutputCurrentType failed: {error}")))?;
        unsafe {
            media_type
                .SetUINT32(&MF_MT_AVG_BITRATE, bitrate.min(u32::MAX as u64) as u32)
                .map_err(|error| Error::backend(format!("SetUINT32 failed: {error}")))?;
            self.transform.SetOutputType(0, &media_type, 0).map_err(|error| {
                Error::backend(format!("the encoder rejected the new bitrate: {error}"))
            })?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// A Media Foundation decoder session.
pub struct MfDecoder {
    transform: IMFTransform,
    codec: Codec,
    path: CodecPath,
    output_pixel_format: crate::media::format::PixelFormat,
    output_sample_format: SampleFormat,
    sample_rate: u32,
    channels: ChannelLayout,
    pending: VecDeque<Frame>,
    draining: bool,
}

// SAFETY: see the identical note on `MfEncoder`.
unsafe impl Send for MfDecoder {}

impl std::fmt::Debug for MfDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfDecoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .finish()
    }
}

impl MfDecoder {
    /// Creates and starts a decoder for `config`.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        let codec = config.codec;
        let transform = find_transform(codec, CodecKind::Decoder)?;
        let subtype = codec_subtype(codec).expect("checked by find_transform");

        let input = if codec.is_video() {
            video_media_type(subtype, config.width, config.height, 0, 1, 0)
        } else {
            audio_media_type(subtype, config.sample_rate, config.channels.map_or(2, |c| c.channels()), 0, 0)
        }
        .map_err(|error| Error::backend(format!("building the input type failed: {error}")))?;

        // Media Foundation wants H.264/HEVC parameter sets out of band when the
        // stream does not carry them in band.
        let parameters = bitstream::annex_b_blob(codec, config.extradata.as_slice(), config.annex_b);
        if !parameters.is_empty() {
            // SAFETY: documented attribute write on a live media type.
            unsafe { input.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &parameters) }.map_err(|error| {
                Error::backend(format!("the decoder rejected the sequence header: {error}"))
            })?;
        }
        // SAFETY: documented COM negotiation call.
        unsafe { transform.SetInputType(0, &input, 0) }
            .map_err(|error| Error::backend(format!("the decoder rejected its input type: {error}")))?;

        let (output_pixel_format, output_sample_format, sample_rate, channels) = if codec.is_video() {
            let candidates: Vec<GUID> =
                video_subtype_candidates().iter().map(|(subtype, _)| *subtype).collect();
            let choice = negotiate_output_type(&transform, &candidates).map_err(|error| {
                Error::backend(format!("the decoder offered no usable output type: {error}"))
            })?;
            (video_subtype_candidates()[choice].1, SampleFormat::S16, 0, ChannelLayout::Stereo)
        } else {
            let choice = negotiate_output_type(&transform, &[MFAudioFormat_PCM]).map_err(|error| {
                Error::backend(format!("the decoder offered no PCM output: {error}"))
            })?;
            let _ = choice;
            // SAFETY: read-only attribute reads on the negotiated output type.
            let current = unsafe { transform.GetOutputCurrentType(0) }
                .map_err(|error| Error::backend(format!("GetOutputCurrentType failed: {error}")))?;
            let (sample_rate, channel_count) = audio_geometry(&current);
            let channels = ChannelLayout::from_channel_count(channel_count)
                .ok_or_else(|| Error::unsupported(format!("{channel_count}-channel audio")))?;
            (
                crate::media::format::PixelFormat::I420,
                SampleFormat::S16,
                sample_rate,
                channels,
            )
        };

        start_streaming(&transform)?;
        Ok(Self {
            transform,
            codec,
            path: CodecPath::new(
                BackendKind::MediaFoundation,
                false,
                format!("mediafoundation: {codec} decoder (synchronous MFT)"),
            ),
            output_pixel_format,
            output_sample_format,
            sample_rate,
            channels,
            pending: VecDeque::new(),
            draining: false,
        })
    }

    fn drain(&mut self) -> Result<()> {
        let mut produced = VecDeque::new();
        loop {
            match take_output(&self.transform, &mut produced) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(error) if error.kind() == crate::error::ErrorKind::State => {
                    let candidates: Vec<GUID> = video_subtype_candidates()
                        .iter()
                        .map(|(subtype, _)| *subtype)
                        .chain([MFAudioFormat_PCM])
                        .collect();
                    match negotiate_output_type(&self.transform, &candidates) {
                        Ok(_) => continue,
                        Err(inner) => {
                            return Err(Error::backend(format!(
                                "the decoder asked to renegotiate but offered nothing usable: {inner}"
                            )));
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        }
        for (payload, pts, _) in produced {
            if payload.is_empty() {
                continue;
            }
            let frame = self.frame_from_payload(&payload, pts)?;
            self.pending.push_back(frame);
        }
        Ok(())
    }

    fn frame_from_payload(&self, payload: &[u8], pts: i64) -> Result<Frame> {
        if self.codec.is_video() {
            let (width, height) = self.output_size();
            let planes = planes_from_payload(self.output_pixel_format, width, height, payload)?;
            return Ok(Frame::Video(VideoFrame::new(
                self.output_pixel_format,
                width,
                height,
                planes,
                pts,
            )?));
        }
        let bytes_per_frame = 2 * self.channels.channels() as usize;
        let frames = (payload.len() / bytes_per_frame.max(1)) as u32;
        let expected = self.channels.byte_len(frames, self.output_sample_format);
        Ok(Frame::Audio(AudioFrame::new(
            self.output_sample_format,
            self.sample_rate,
            self.channels,
            frames,
            Buffer::from_slice(&payload[..expected.min(payload.len())]),
            pts,
        )?))
    }

    fn output_size(&self) -> (u32, u32) {
        // SAFETY: read-only attribute reads on the negotiated output type.
        let Ok(current) = (unsafe { self.transform.GetOutputCurrentType(0) }) else {
            return (2, 2);
        };
        let packed = unsafe { current.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
        let width = ((packed >> 32) as u32).max(2);
        let height = (packed as u32).max(2);
        (width & !1, height & !1)
    }
}

impl Decoder for MfDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if self.draining {
            return Err(Error::state("mf: the decoder is already draining"));
        }
        if packet.is_header() {
            // Configuration reached the transform through `DecoderConfig::extradata`.
            return Ok(());
        }
        let sample = sample_from_bytes(packet.data.as_slice(), packet.pts, packet.duration)?;
        // SAFETY: `transform` is live and `sample` is fully formed.
        unsafe { self.transform.ProcessInput(0, &sample, 0) }
            .map_err(|error| Error::backend(format!("ProcessInput failed: {error}")))?;
        self.drain()
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        if self.pending.is_empty() {
            self.drain()?;
        }
        Ok(self.pending.pop_front())
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        self.draining = true;
        // SAFETY: documented drain sequence.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        }
        let _ = self.drain();
        Ok(self.pending.drain(..).collect())
    }
}

// ---------------------------------------------------------------------------
// Probing and registration
// ---------------------------------------------------------------------------

/// Probes one codec/direction and explains the outcome.
pub fn probe_component(codec: Codec, kind: CodecKind) -> BackendAvailability {
    if let Err(reason) = startup() {
        return BackendAvailability::Unavailable(reason);
    }
    if codec == Codec::Video(VideoCodec::H266) {
        return BackendAvailability::Unavailable(
            "Windows ships no VVC transform; Media Foundation has no h266 decoder or encoder"
                .to_string(),
        );
    }
    match find_transform(codec, kind) {
        Ok(_) => BackendAvailability::Available,
        Err(error) => BackendAvailability::Unavailable(error.message().to_string()),
    }
}

/// Reports one device per Media Foundation codec category.
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    if startup().is_err() {
        return Vec::new();
    }
    let mut devices = Vec::new();
    for (name, kind, video) in [
        ("Media Foundation video decoders", CodecKind::Decoder, true),
        ("Media Foundation video encoders", CodecKind::Encoder, true),
        ("Media Foundation audio decoders", CodecKind::Decoder, false),
        ("Media Foundation audio encoders", CodecKind::Encoder, false),
    ] {
        let mut device = HwDeviceInfo::new(BackendKind::MediaFoundation, name, "Media Foundation 1.x");
        for codec in Codec::ALL {
            if codec.is_video() != video {
                continue;
            }
            match find_transform(codec, kind) {
                Ok(_) => device.codecs.push(HwCodecSupport {
                    codec,
                    decode: kind == CodecKind::Decoder,
                    encode: kind == CodecKind::Encoder,
                    max_width: 0,
                    max_height: 0,
                    max_bit_depth: 8,
                }),
                Err(_) => continue,
            }
        }
        if !device.codecs.is_empty() {
            devices.push(device);
        }
    }
    devices
}

/// Registers every codec Media Foundation can actually serve on this machine.
pub fn register(registry: &mut Registry) {
    if startup().is_err() {
        return;
    }
    for codec in Codec::ALL {
        for kind in CodecKind::ALL {
            if !probe_component(codec, kind).is_available() {
                continue;
            }
            let probe = move |codec: Codec| probe_component(codec, kind);
            match kind {
                CodecKind::Encoder => {
                    let factory = move |config: &EncoderConfig| {
                        Ok(Some(Box::new(MfEncoder::new(config)?) as Box<dyn Encoder>))
                    };
                    registry.register_encoder(
                        codec,
                        BackendKind::MediaFoundation,
                        super::super::priority::PLATFORM_FRAMEWORK,
                        factory,
                        probe,
                    );
                }
                CodecKind::Decoder => {
                    let factory = move |config: &DecoderConfig| {
                        Ok(Some(Box::new(MfDecoder::new(config)?) as Box<dyn Decoder>))
                    };
                    registry.register_decoder(
                        codec,
                        BackendKind::MediaFoundation,
                        super::super::priority::PLATFORM_FRAMEWORK,
                        factory,
                        probe,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_foundation_actually_starts() {
        // This is the *native capability* test for this backend: if MFStartup
        // fails, nothing else here can work, and the reason must be readable.
        match startup() {
            Ok(()) => assert!(framework_available()),
            Err(reason) => panic!("Media Foundation did not start: {reason}"),
        }
    }

    #[test]
    fn the_subtype_table_is_total_for_every_codec_media_foundation_knows() {
        for codec in Codec::ALL {
            let known = codec_subtype(codec);
            assert_eq!(
                known.is_some(),
                codec != Codec::Video(VideoCodec::H266),
                "{codec} must have a subtype exactly when Windows has one"
            );
        }
        assert_eq!(codec_subtype(Codec::Video(VideoCodec::H264)), Some(MFVideoFormat_H264));
        assert_eq!(codec_subtype(Codec::Audio(AudioCodec::Aac)), Some(MFAudioFormat_AAC));
    }

    #[test]
    fn categories_match_the_direction_and_media_kind() {
        assert_eq!(
            category_for(Codec::Video(VideoCodec::H264), CodecKind::Encoder),
            MFT_CATEGORY_VIDEO_ENCODER
        );
        assert_eq!(
            category_for(Codec::Video(VideoCodec::H264), CodecKind::Decoder),
            MFT_CATEGORY_VIDEO_DECODER
        );
        assert_eq!(
            category_for(Codec::Audio(AudioCodec::Aac), CodecKind::Encoder),
            MFT_CATEGORY_AUDIO_ENCODER
        );
        assert_eq!(
            category_for(Codec::Audio(AudioCodec::Aac), CodecKind::Decoder),
            MFT_CATEGORY_AUDIO_DECODER
        );
    }

    #[test]
    fn the_video_candidates_are_nv12_then_i420() {
        let candidates = video_subtype_candidates();
        assert_eq!(candidates[0].1, crate::media::format::PixelFormat::Nv12);
        assert_eq!(candidates[1].1, crate::media::format::PixelFormat::I420);
    }

    #[test]
    fn the_h264_decoder_transform_exists_on_a_stock_windows() {
        // H.264 decoding is inbox on every Windows since 8, so a stock machine
        // must be able to produce this transform.
        let transform = find_transform(Codec::Video(VideoCodec::H264), CodecKind::Decoder);
        assert!(transform.is_ok(), "{:?}", transform.err().map(|e| e.message().to_string()));
    }

    #[test]
    fn the_h264_encoder_transform_exists_on_a_stock_windows() {
        let transform = find_transform(Codec::Video(VideoCodec::H264), CodecKind::Encoder);
        assert!(transform.is_ok(), "{:?}", transform.err().map(|e| e.message().to_string()));
    }

    #[test]
    fn vvc_is_reported_as_absent_with_a_specific_reason() {
        let availability = probe_component(Codec::Video(VideoCodec::H266), CodecKind::Decoder);
        let reason = availability.reason().expect("h266 must be unavailable");
        assert!(reason.contains("h266"), "{reason}");
        assert!(reason.contains("VVC") || reason.contains("vvc"), "{reason}");
    }

    #[test]
    fn an_unavailable_codec_never_returns_an_empty_reason() {
        for codec in Codec::ALL {
            for kind in CodecKind::ALL {
                if let BackendAvailability::Unavailable(reason) = probe_component(codec, kind) {
                    assert!(!reason.is_empty(), "{codec} {kind:?}");
                    assert!(reason.len() > 15, "{codec} {kind:?}: {reason}");
                }
            }
        }
    }

    #[test]
    fn registering_never_panics() {
        let mut registry = Registry::new();
        register(&mut registry);
        assert_eq!(registry.capabilities().len(), Codec::ALL.len());
    }

    /// The end-to-end native-capability test: build a real H.264 encoder, feed it
    /// synthetic NV12 pictures, and require at least one packet out. If the
    /// transform is genuinely absent the failure must be a specific reason, not a
    /// panic 鈥?but on Windows it is always present, so this asserts production.
    #[test]
    fn a_real_h264_encode_produces_a_packet() {
        if !framework_available() {
            panic!("Media Foundation must be available on Windows");
        }
        let config = EncoderConfig::video(VideoCodec::H264, 64, 64)
            .with_framerate(30, 1)
            .with_bitrate(400_000);
        let mut encoder = MfEncoder::new(&config).expect("the H.264 encoder MFT must open");

        // A flat NV12 picture: 64 bytes of luma per row for 64 rows, then 64 bytes
        // of interleaved chroma per row for 32 rows. `Plane::rows` is what keeps
        // the stride honest; `Plane::packed` would claim a single 4096-byte row.
        let luma = Plane::rows(Buffer::from_vec(vec![16u8; 64 * 64]), 64).unwrap();
        let chroma = Plane::rows(Buffer::from_vec(vec![128u8; 32 * 64]), 64).unwrap();
        let frame = VideoFrame::new(
            crate::media::format::PixelFormat::Nv12,
            64,
            64,
            vec![luma, chroma],
            0,
        )
        .unwrap();

        let mut packets = Vec::new();
        for index in 0..8 {
            let mut shifted = frame.clone();
            shifted.pts = index;
            encoder.push(Frame::Video(shifted)).expect("ProcessInput must accept the picture");
            while let Some(packet) = encoder.receive().expect("ProcessOutput must not fail") {
                packets.push(packet);
            }
            if !packets.is_empty() {
                break;
            }
        }
        packets.extend(encoder.flush().expect("flush must not fail"));

        assert!(!packets.is_empty(), "a real H.264 encode produced no output at all");
        assert!(
            packets.iter().any(|packet| !packet.is_empty()),
            "every packet was empty"
        );
        let path = encoder.path();
        assert_eq!(path.backend, BackendKind::MediaFoundation);
        assert!(
            !path.hardware_accelerated,
            "this backend uses synchronous MFTs, so it must not claim hardware"
        );
    }
}
