//! The MediaCodec half of the Android backend.
//!
//! Everything here goes through `ndk::media::MediaCodec`, the NDK crate's safe
//! wrapper, so there is no hand-written NDK FFI. The logic that *is* ours is:
//!
//! * which codecs to route (and refusing to claim what the device lacks);
//! * converting decoder configuration records (`avcC` / `hvcC`) into the
//!   Annex-B parameter sets MediaCodec wants;
//! * laying out a raw [`VideoFrame`] for a flexible-YUV encoder;
//! * turning MediaCodec's output buffers back into [`Frame`]s; and
//! * deciding, and reporting, hardware versus software.

use std::collections::VecDeque;
use std::time::Duration;

use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
};
use ndk::media::media_format::MediaFormat;

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, CodecKind, HwDeviceInfo, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::{Buffer, BufferBuilder};
use crate::media::codec::{AudioCodec, Codec, VideoCodec};
use crate::media::config::{DecoderConfig, EncoderConfig};
use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};
use crate::media::frame::{AudioFrame, Frame, Plane, VideoFrame};
use crate::media::packet::{flags, Packet};

// ---------------------------------------------------------------------------
// AMediaFormat keys (the NDK exposes them as C string constants)
// ---------------------------------------------------------------------------

const KEY_MIME: &str = "mime";
const KEY_WIDTH: &str = "width";
const KEY_HEIGHT: &str = "height";
const KEY_BIT_RATE: &str = "bitrate";
const KEY_FRAME_RATE: &str = "frame-rate";
const KEY_I_FRAME_INTERVAL: &str = "i-frame-interval";
const KEY_COLOR_FORMAT: &str = "color-format";
const KEY_STRIDE: &str = "stride";
const KEY_SLICE_HEIGHT: &str = "slice-height";
const KEY_SAMPLE_RATE: &str = "sample-rate";
const KEY_CHANNEL_COUNT: &str = "channel-count";
const KEY_CSD_0: &str = "csd-0";
const KEY_CSD_1: &str = "csd-1";
const KEY_MAX_INPUT_SIZE: &str = "max-input-size";
const KEY_CROP_LEFT: &str = "crop-left";
const KEY_CROP_TOP: &str = "crop-top";
const KEY_CROP_RIGHT: &str = "crop-right";
const KEY_CROP_BOTTOM: &str = "crop-bottom";
const KEY_PCM_ENCODING: &str = "pcm-encoding";

/// `COLOR_FormatYUV420Planar`.
// Deliberately `pub(crate)`: cbindgen parses a platform module twice (once for
// its own target and once for the `#[path]` stub), so anything `pub` here would
// land in the C header twice. See the note in `native/mod.rs`.
pub(crate) const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
/// `COLOR_FormatYUV420SemiPlanar`.
pub(crate) const COLOR_FORMAT_YUV420_SEMIPLANAR: i32 = 21;
/// `COLOR_FormatYUV420Flexible`.
pub(crate) const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7F42_0888;

/// `AMEDIACODEC_BUFFER_FLAG_KEY_FRAME`.
const BUFFER_FLAG_KEY_FRAME: u32 = 1;
/// `AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG`.
const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
/// `AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM`.
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;

/// How long a single dequeue call blocks before reporting "try again".
const DEQUEUE_TIMEOUT: Duration = Duration::from_millis(8);

// ---------------------------------------------------------------------------
// Send wrapper
// ---------------------------------------------------------------------------

/// `ndk::media::MediaCodec` holds a `NonNull<AMediaCodec>`, so it is `!Send`.
///
/// An `AMediaCodec*` is a plain heap handle with no thread affinity; the NDK's
/// own documentation only forbids *concurrent* use, not cross-thread movement.
/// AnyCodec's session model is "created on one worker isolate, used only from
/// that isolate, freed there", which is exactly the contract the NDK requires, so
/// the assertion below is sound.
struct SendCodec(MediaCodec);

// SAFETY: see the doc comment above. The handle is moved to a worker thread
// before first use and never touched from two threads at once.
unsafe impl Send for SendCodec {}

impl std::fmt::Debug for SendCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SendCodec").field(&self.0.name().unwrap_or_default()).finish()
    }
}

impl SendCodec {
    fn codec(&self) -> &MediaCodec {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Framework probing
// ---------------------------------------------------------------------------

/// `true` when MediaCodec answered a trivial query, i.e. `libmediandk` is live.
#[must_use]
pub fn framework_available() -> bool {
    MediaCodec::from_decoder_type(Codec::Video(VideoCodec::H264).mime()).is_some()
}

/// `true` when a component name looks like a dedicated hardware block.
///
/// Android's naming convention is stable enough for this:
/// `OMX.google.*`, `c2.android.*` and `c2.google.*` are software; everything else
/// (`c2.qti.*`, `c2.exynos.*`, `OMX.qcom.*`, ...) is a vendor hardware codec.
#[must_use]
pub fn is_hardware_component(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    !(lowered.starts_with("omx.google.")
        || lowered.starts_with("c2.android.")
        || lowered.starts_with("c2.google.")
        || lowered.contains("software"))
}

/// Creates a throwaway component for `codec`/`kind` and returns its name.
#[must_use]
pub fn component_name(codec: Codec, kind: CodecKind) -> Option<String> {
    let created = match kind {
        CodecKind::Encoder => MediaCodec::from_encoder_type(codec.mime()),
        CodecKind::Decoder => MediaCodec::from_decoder_type(codec.mime()),
    }?;
    let name = created.name().ok();
    // Dropping `created` releases the component.
    name
}

/// The probe used for every registration.
pub fn probe_component(codec: Codec, kind: CodecKind) -> BackendAvailability {
    match component_name(codec, kind) {
        Some(_) => BackendAvailability::Available,
        None => BackendAvailability::Unavailable(format!(
            "Android has no {} component for {} ({}) on this device",
            kind.name(),
            codec.name(),
            codec.mime()
        )),
    }
}

/// Reports the device's hardware and software codec groups.
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    let mut hardware = HwDeviceInfo::new(
        BackendKind::MediaCodec,
        "Android hardware codecs",
        "AMediaCodec (NDK)",
    );
    let mut software = HwDeviceInfo::new(
        BackendKind::MediaCodec,
        "Android software codecs",
        "AMediaCodec (NDK), c2.android.*/OMX.google.*",
    );
    for codec in Codec::ALL {
        let decoder = component_name(codec, CodecKind::Decoder);
        let encoder = component_name(codec, CodecKind::Encoder);
        if decoder.is_none() && encoder.is_none() {
            continue;
        }
        let representative = decoder.clone().or(encoder.clone()).unwrap_or_default();
        let support = super::support_for(codec);
        if is_hardware_component(&representative) {
            hardware.codecs.push(support);
        } else {
            software.codecs.push(support);
        }
    }
    let mut devices = Vec::new();
    if !hardware.codecs.is_empty() {
        devices.push(hardware);
    }
    if !software.codecs.is_empty() {
        devices.push(software);
    }
    devices
}

// ---------------------------------------------------------------------------
// Codec configuration records
// ---------------------------------------------------------------------------

/// Converts a decoder configuration record into Annex-B parameter sets.
///
/// MediaCodec wants `csd-0` / `csd-1` to hold complete start-code-prefixed NAL
/// units, while MP4-style containers hand out `avcC` / `hvcC` records with
/// 2-byte big-endian lengths. This function accepts either shape and always
/// returns start-code-prefixed NAL units, so a caller can feed whatever the
/// container gave them.
///
/// Returns an empty vector when `extradata` is empty or when it is a record this
/// build does not understand; the caller then configures MediaCodec without CSD,
/// which works for codecs whose parameter sets are in-band.
#[must_use]
pub fn annex_b_parameter_sets(codec: Codec, extradata: &[u8], already_annex_b: bool) -> Vec<Vec<u8>> {
    if extradata.is_empty() {
        return Vec::new();
    }
    if already_annex_b || has_start_code(extradata) {
        return split_annex_b(extradata);
    }
    match codec {
        Codec::Video(VideoCodec::H264) => parse_avcc(extradata),
        Codec::Video(VideoCodec::H265) => parse_hvcc(extradata),
        _ => Vec::new(),
    }
}

fn has_start_code(bytes: &[u8]) -> bool {
    bytes.windows(3).take(8).any(|w| w == [0, 0, 1])
}

/// Splits an Annex-B buffer into start-code-free NAL units, re-prefixing each.
fn split_annex_b(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut units = Vec::new();
    let mut index = 0usize;
    let mut start = None;
    while index + 2 < bytes.len() {
        let is_three = bytes[index] == 0 && bytes[index + 1] == 0 && bytes[index + 2] == 1;
        let is_four = index + 3 < bytes.len()
            && bytes[index] == 0
            && bytes[index + 1] == 0
            && bytes[index + 2] == 0
            && bytes[index + 3] == 1;
        if is_three || is_four {
            if let Some(previous) = start {
                let end = index;
                if end > previous {
                    units.push(bytes[previous..end].to_vec());
                }
            }
            index += if is_four { 4 } else { 3 };
            start = Some(index);
        } else {
            index += 1;
        }
    }
    if let Some(previous) = start {
        if previous < bytes.len() {
            units.push(bytes[previous..].to_vec());
        }
    }
    units
}

/// Parses an ISO/IEC 14496-15 `AVCDecoderConfigurationRecord`.
fn parse_avcc(record: &[u8]) -> Vec<Vec<u8>> {
    if record.len() < 7 || record[0] != 1 {
        return Vec::new();
    }
    let mut units = Vec::new();
    let mut cursor = 5usize;
    let sps_count = (record[cursor] & 0x1f) as usize;
    cursor += 1;
    for _ in 0..sps_count {
        if cursor + 2 > record.len() {
            return units;
        }
        let length = ((record[cursor] as usize) << 8) | record[cursor + 1] as usize;
        cursor += 2;
        if cursor + length > record.len() {
            return units;
        }
        units.push(prefix_with_start_code(&record[cursor..cursor + length]));
        cursor += length;
    }
    if cursor >= record.len() {
        return units;
    }
    let pps_count = record[cursor] as usize;
    cursor += 1;
    for _ in 0..pps_count {
        if cursor + 2 > record.len() {
            return units;
        }
        let length = ((record[cursor] as usize) << 8) | record[cursor + 1] as usize;
        cursor += 2;
        if cursor + length > record.len() {
            return units;
        }
        units.push(prefix_with_start_code(&record[cursor..cursor + length]));
        cursor += length;
    }
    units
}

/// Parses an ISO/IEC 14496-15 `HEVCDecoderConfigurationRecord`.
fn parse_hvcc(record: &[u8]) -> Vec<Vec<u8>> {
    if record.len() < 23 || record[0] != 1 {
        return Vec::new();
    }
    let mut units = Vec::new();
    let array_count = record[22] as usize;
    let mut cursor = 23usize;
    for _ in 0..array_count {
        if cursor + 3 > record.len() {
            return units;
        }
        // record[cursor] holds completeness/reserved/NAL_unit_type.
        let nal_count = ((record[cursor + 1] as usize) << 8) | record[cursor + 2] as usize;
        cursor += 3;
        for _ in 0..nal_count {
            if cursor + 2 > record.len() {
                return units;
            }
            let length = ((record[cursor] as usize) << 8) | record[cursor + 1] as usize;
            cursor += 2;
            if cursor + length > record.len() {
                return units;
            }
            units.push(prefix_with_start_code(&record[cursor..cursor + length]));
            cursor += length;
        }
    }
    units
}

fn prefix_with_start_code(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len() + 4);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
    out
}

/// Maps a `COLOR_Format*` constant onto a [`PixelFormat`].
#[must_use]
pub fn pixel_format_from_color_format(color_format: i32) -> Option<PixelFormat> {
    match color_format {
        COLOR_FORMAT_YUV420_PLANAR => Some(PixelFormat::I420),
        COLOR_FORMAT_YUV420_SEMIPLANAR => Some(PixelFormat::Nv12),
        // A flexible buffer's real layout is only knowable through
        // `AMediaCodec_getOutputImage`; I420 is the layout every flexible-YUV
        // decoder in practice emits for the byte-buffer API.
        COLOR_FORMAT_YUV420_FLEXIBLE => Some(PixelFormat::I420),
        _ => None,
    }
}

/// The `color-format` an encoder should be configured with for `format`.
#[must_use]
pub fn color_format_for_input(format: PixelFormat) -> i32 {
    match format {
        PixelFormat::I420 | PixelFormat::P010 => COLOR_FORMAT_YUV420_FLEXIBLE,
        PixelFormat::Nv12 | PixelFormat::Nv21 => COLOR_FORMAT_YUV420_SEMIPLANAR,
        _ => COLOR_FORMAT_YUV420_FLEXIBLE,
    }
}

/// The `color-format` for `PixelFormat::Nv21`, which Android spells separately.
const COLOR_FORMAT_YUV420_SEMIPLANAR_NV21: i32 = 0x7F00_0020;

/// The `pcm-encoding` value for 16-bit signed little-endian samples (API 31+).
const PCM_ENCODING_16BIT: i32 = 2;

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// What a configured MediaCodec session can tell us without another NDK call.
#[derive(Debug, Clone)]
struct SessionInfo {
    component: String,
    hardware: bool,
}

impl SessionInfo {
    fn path(&self, detail: &str) -> CodecPath {
        CodecPath::new(
            BackendKind::MediaCodec,
            self.hardware,
            format!("mediacodec: {} ({detail})", self.component),
        )
    }
}

fn session_info(handle: &MediaCodec) -> SessionInfo {
    let component = handle.name().unwrap_or_else(|_| "<unknown component>".to_string());
    let hardware = is_hardware_component(&component);
    SessionInfo { component, hardware }
}

fn to_backend_error(context: &str, error: impl std::fmt::Display) -> Error {
    Error::backend(format!("mediacodec: {context}: {error}"))
}

fn configure_and_start(handle: &MediaCodec, format: &MediaFormat, direction: MediaCodecDirection) -> Result<()> {
    handle
        .configure(format, None, direction)
        .map_err(|error| to_backend_error("configure", error))?;
    handle.start().map_err(|error| to_backend_error("start", error))
}

/// Copies `bytes` into a MediaCodec input buffer, returning the byte count.
fn write_input(handle: &MediaCodec, bytes: &[u8]) -> Result<usize> {
    match handle
        .dequeue_input_buffer(DEQUEUE_TIMEOUT)
        .map_err(|error| to_backend_error("dequeueInputBuffer", error))?
    {
        DequeuedInputBufferResult::TryAgainLater => Ok(0),
        DequeuedInputBufferResult::Buffer(mut buffer) => {
            let destination = buffer.buffer_mut();
            if destination.len() < bytes.len() {
                return Err(Error::backend(format!(
                    "mediacodec: input buffer is {} bytes but {} are needed",
                    destination.len(),
                    bytes.len()
                )));
            }
            for (slot, byte) in destination.iter_mut().zip(bytes.iter()) {
                slot.write(*byte);
            }
            Ok(bytes.len())
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// A MediaCodec encoder session.
pub struct MediaCodecEncoder {
    codec: Codec,
    handle: SendCodec,
    path: CodecPath,
    input_format: PixelFormat,
    width: u32,
    height: u32,
    pending: VecDeque<Packet>,
    end_of_stream: bool,
}

impl std::fmt::Debug for MediaCodecEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaCodecEncoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl MediaCodecEncoder {
    /// Creates and starts an encoder for `config`.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        if config.codec.is_audio() {
            return Self::new_audio(config);
        }
        Self::new_video(config)
    }

    fn new_video(config: &EncoderConfig) -> Result<Self> {
        let handle = MediaCodec::from_encoder_type(config.codec.mime()).ok_or_else(|| {
            Error::unavailable(format!(
                "Android has no {} encoder component on this device",
                config.codec.name()
            ))
        })?;
        let color_format = match config.option("color_format") {
            Some("nv12") => COLOR_FORMAT_YUV420_SEMIPLANAR,
            Some("nv21") => COLOR_FORMAT_YUV420_SEMIPLANAR_NV21,
            Some("i420") | None => color_format_for_input(config.pixel_format),
            Some(other) => {
                return Err(Error::invalid_argument(format!(
                    "unknown color_format {other:?}; expected i420, nv12 or nv21"
                )));
            }
        };
        let mut format = MediaFormat::new();
        format.set_str(KEY_MIME, config.codec.mime());
        format.set_i32(KEY_WIDTH, config.width as i32);
        format.set_i32(KEY_HEIGHT, config.height as i32);
        format.set_i32(KEY_COLOR_FORMAT, color_format);
        format.set_i64(KEY_BIT_RATE, config.bitrate.max(1) as i64);
        if let Some(framerate) = config.framerate() {
            format.set_i32(KEY_FRAME_RATE, framerate.round() as i32);
        }
        if config.keyframe_interval > 0 {
            format.set_i32(KEY_I_FRAME_INTERVAL, config.keyframe_interval as i32);
        }
        // Some devices refuse a flexible-YUV buffer whose stride is not the
        // coded width, so advertise the tight layout we actually send.
        format.set_i32(KEY_STRIDE, config.width as i32);
        format.set_i32(KEY_SLICE_HEIGHT, config.height as i32);

        configure_and_start(&handle, &format, MediaCodecDirection::Encoder)?;
        let info = session_info(&handle);
        let path = info.path(&format!("encoder {}x{}", config.width, config.height));
        Ok(Self {
            codec: config.codec,
            handle: SendCodec(handle),
            path,
            input_format: config.pixel_format,
            width: config.width,
            height: config.height,
            pending: VecDeque::new(),
            end_of_stream: false,
        })
    }

    fn new_audio(config: &EncoderConfig) -> Result<Self> {
        let handle = MediaCodec::from_encoder_type(config.codec.mime()).ok_or_else(|| {
            Error::unavailable(format!(
                "Android has no {} encoder component on this device",
                config.codec.name()
            ))
        })?;
        let mut format = MediaFormat::new();
        format.set_str(KEY_MIME, config.codec.mime());
        format.set_i32(KEY_SAMPLE_RATE, config.sample_rate as i32);
        format.set_i32(KEY_CHANNEL_COUNT, config.channels.channels() as i32);
        format.set_i64(KEY_BIT_RATE, config.bitrate.max(1) as i64);
        format.set_i32(KEY_MAX_INPUT_SIZE, 64 * 1024);
        configure_and_start(&handle, &format, MediaCodecDirection::Encoder)?;
        let info = session_info(&handle);
        let path = info.path(&format!(
            "encoder {} Hz {}",
            config.sample_rate,
            config.channels.name()
        ));
        Ok(Self {
            codec: config.codec,
            handle: SendCodec(handle),
            path,
            input_format: PixelFormat::I420,
            width: 0,
            height: 0,
            pending: VecDeque::new(),
            end_of_stream: false,
        })
    }

    /// Serialises one raw picture the way a flexible-YUV encoder expects it.
    fn video_payload(&self, frame: &VideoFrame) -> Result<Vec<u8>> {
        if frame.format != self.input_format {
            return Err(Error::invalid_argument(format!(
                "the MediaCodec encoder was configured for {} but received {}",
                self.input_format.name(),
                frame.format.name()
            )));
        }
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::invalid_argument(format!(
                "the MediaCodec encoder was configured for {}x{} but received {}x{}; \
                 AMediaCodec needs a fixed geometry",
                self.width, self.height, frame.width, frame.height
            )));
        }
        let mut builder = BufferBuilder::with_capacity(frame.byte_len());
        match frame.format {
            PixelFormat::I420 | PixelFormat::P010 => {
                // Luma first, then U, then V, each tightened to the coded width.
                for (index, plane) in frame.planes.iter().enumerate() {
                    let (width, height) = frame.format.plane_size(frame.width, frame.height, index)?;
                    let row_bytes = frame.format.default_stride(width, 0)?;
                    for row in 0..height as usize {
                        builder.push(plane.row(row, row_bytes)?);
                    }
                }
            }
            PixelFormat::Nv12 | PixelFormat::Nv21 => {
                for (index, plane) in frame.planes.iter().enumerate() {
                    let (width, height) = frame.format.plane_size(frame.width, frame.height, index)?;
                    let row_bytes = frame.format.default_stride(width, 0)?;
                    for row in 0..height as usize {
                        builder.push(plane.row(row, row_bytes)?);
                    }
                }
            }
            other => {
                return Err(Error::unsupported(format!(
                    "the MediaCodec encoder cannot take {} input; use i420, nv12 or nv21",
                    other.name()
                )));
            }
        }
        Ok(builder.finish().to_vec())
    }

    /// Drains every finished output packet.
    fn drain(&mut self) -> Result<()> {
        loop {
            match self
                .handle
                .codec()
                .dequeue_output_buffer(DEQUEUE_TIMEOUT)
                .map_err(|error| to_backend_error("dequeueOutputBuffer", error))?
            {
                DequeuedOutputBufferInfoResult::TryAgainLater => return Ok(()),
                DequeuedOutputBufferInfoResult::OutputFormatChanged
                | DequeuedOutputBufferInfoResult::OutputBuffersChanged => continue,
                DequeuedOutputBufferInfoResult::Buffer(buffer) => {
                    let info = buffer.info().clone();
                    let payload = buffer.buffer().to_vec();
                    let pts = info.presentation_time_us();
                    let codec_flags = info.flags();
                    self.handle
                        .codec()
                        .release_output_buffer(buffer, false)
                        .map_err(|error| to_backend_error("releaseOutputBuffer", error))?;

                    if codec_flags & BUFFER_FLAG_CODEC_CONFIG != 0 {
                        self.pending.push_back(Packet::new(
                            self.codec,
                            Buffer::from_vec(payload),
                            pts,
                            flags::HEADER,
                        ));
                        continue;
                    }
                    let mut packet_flags = 0u32;
                    if codec_flags & BUFFER_FLAG_KEY_FRAME != 0 {
                        packet_flags |= flags::KEYFRAME;
                    }
                    if codec_flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                        packet_flags |= flags::EOS;
                    }
                    self.pending.push_back(Packet::new(
                        self.codec,
                        Buffer::from_vec(payload),
                        pts,
                        packet_flags,
                    ));
                }
            }
        }
    }
}

impl Encoder for MediaCodecEncoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        if self.end_of_stream {
            return Err(Error::state("mediacodec: the encoder is already draining"));
        }
        let (payload, pts) = match frame {
            Frame::Video(video) => (self.video_payload(&video)?, video.pts),
            Frame::Audio(audio) => {
                // MediaCodec audio encoders take raw PCM in the configured
                // sample format; 16-bit little-endian is what it accepts.
                let samples = match audio.format {
                    SampleFormat::S16 => audio.samples.to_vec(),
                    SampleFormat::F32 => {
                        let mut builder = BufferBuilder::with_capacity(audio.byte_len() / 2);
                        for chunk in audio.samples.as_slice().chunks_exact(4) {
                            let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                            let scaled = (value.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                            builder.push(&scaled.to_le_bytes());
                        }
                        builder.finish().to_vec()
                    }
                    other => {
                        return Err(Error::unsupported(format!(
                            "the MediaCodec audio encoder cannot take {} samples; use s16 or f32",
                            other.name()
                        )));
                    }
                };
                (samples, audio.pts)
            }
        };
        let written = write_input(self.handle.codec(), &payload)?;
        if written == 0 {
            return Err(Error::state(
                "mediacodec: no input buffer became available; drain the encoder first",
            ));
        }
        self.handle
            .codec()
            .queue_input_buffer_by_index(0, 0, written, pts.max(0) as u64, 0)
            .map_err(|error| to_backend_error("queueInputBuffer", error))
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        if let Some(packet) = self.pending.pop_front() {
            return Ok(Some(packet));
        }
        self.drain()?;
        Ok(self.pending.pop_front())
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        self.end_of_stream = true;
        self.drain()?;
        Ok(self.pending.drain(..).collect())
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        let mut parameters = MediaFormat::new();
        parameters.set_i64(KEY_BIT_RATE, bitrate as i64);
        self.handle
            .codec()
            .set_parameters(parameters)
            .map_err(|error| to_backend_error("setParameters(bitrate)", error))
    }

    fn request_keyframe(&mut self) -> Result<()> {
        let mut parameters = MediaFormat::new();
        // The NDK documents this key as the request-sync-frame marker on encoders.
        parameters.set_i32("request-sync", 1);
        self.handle
            .codec()
            .set_parameters(parameters)
            .map_err(|error| to_backend_error("setParameters(request-sync)", error))
    }
}

impl Drop for MediaCodecEncoder {
    fn drop(&mut self) {
        // Best effort: a codec that is already stopped reports an error here,
        // which is not worth surfacing from a destructor.
        let _ = self.handle.codec().stop();
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// A MediaCodec decoder session.
pub struct MediaCodecDecoder {
    codec: Codec,
    handle: SendCodec,
    path: CodecPath,
    output_format: PixelFormat,
    pending: VecDeque<Frame>,
    end_of_stream: bool,
}

impl std::fmt::Debug for MediaCodecDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaCodecDecoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl MediaCodecDecoder {
    /// Creates and starts a decoder for `config`.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        let handle = MediaCodec::from_decoder_type(config.codec.mime()).ok_or_else(|| {
            Error::unavailable(format!(
                "Android has no {} decoder component on this device",
                config.codec.name()
            ))
        })?;

        let mut format = MediaFormat::new();
        format.set_str(KEY_MIME, config.codec.mime());
        if config.width > 0 {
            format.set_i32(KEY_WIDTH, config.width as i32);
            format.set_i32(KEY_STRIDE, config.width as i32);
        }
        if config.height > 0 {
            format.set_i32(KEY_HEIGHT, config.height as i32);
            format.set_i32(KEY_SLICE_HEIGHT, config.height as i32);
        }
        if let Some(pixel_format) = config.pixel_format {
            format.set_i32(KEY_COLOR_FORMAT, color_format_for_input(pixel_format));
        }
        let sets = annex_b_parameter_sets(config.codec, config.extradata.as_slice(), config.annex_b);
        if let Some(first) = sets.first() {
            format.set_buffer(KEY_CSD_0, first);
        }
        if let Some(second) = sets.get(1) {
            format.set_buffer(KEY_CSD_1, second);
        }
        if config.codec.is_audio() {
            if config.sample_rate > 0 {
                format.set_i32(KEY_SAMPLE_RATE, config.sample_rate as i32);
            }
            if let Some(channels) = config.channels {
                format.set_i32(KEY_CHANNEL_COUNT, channels.channels() as i32);
            }
            format.set_i32(KEY_PCM_ENCODING, PCM_ENCODING_16BIT);
        }

        configure_and_start(&handle, &format, MediaCodecDirection::Decoder)?;
        let info = session_info(&handle);
        let output_format = config.pixel_format.unwrap_or(PixelFormat::I420);
        let path = info.path("decoder");
        Ok(Self {
            codec: config.codec,
            handle: SendCodec(handle),
            path,
            output_format,
            pending: VecDeque::new(),
            end_of_stream: false,
        })
    }

    /// The pixel format MediaCodec actually reports for its output.
    fn refresh_output_format(&mut self) {
        let format = self.handle.codec().output_format();
        if let Some(color_format) = format.i32(KEY_COLOR_FORMAT) {
            if let Some(pixel_format) = pixel_format_from_color_format(color_format) {
                self.output_format = pixel_format;
            }
        }
    }

    /// Drains every finished output frame.
    fn drain(&mut self) -> Result<()> {
        loop {
            match self
                .handle
                .codec()
                .dequeue_output_buffer(DEQUEUE_TIMEOUT)
                .map_err(|error| to_backend_error("dequeueOutputBuffer", error))?
            {
                DequeuedOutputBufferInfoResult::TryAgainLater => return Ok(()),
                DequeuedOutputBufferInfoResult::OutputBuffersChanged => continue,
                DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                    self.refresh_output_format();
                    continue;
                }
                DequeuedOutputBufferInfoResult::Buffer(buffer) => {
                    let info = buffer.info().clone();
                    let payload = buffer.buffer().to_vec();
                    let pts = info.presentation_time_us();
                    let codec_flags = info.flags();
                    self.handle
                        .codec()
                        .release_output_buffer(buffer, false)
                        .map_err(|error| to_backend_error("releaseOutputBuffer", error))?;

                    if codec_flags & BUFFER_FLAG_CODEC_CONFIG != 0 {
                        continue;
                    }
                    if payload.is_empty() {
                        continue;
                    }
                    let frame = self.frame_from_payload(&payload, pts)?;
                    self.pending.push_back(frame);
                }
            }
        }
    }

    fn frame_from_payload(&self, payload: &[u8], pts: i64) -> Result<Frame> {
        if self.codec.is_audio() {
            let layout = ChannelLayout::from_channel_count(
                self.handle
                    .codec()
                    .output_format()
                    .i32(KEY_CHANNEL_COUNT)
                    .unwrap_or(2)
                    .max(1) as u32,
            )
            .unwrap_or(ChannelLayout::Stereo);
            let sample_rate = self
                .handle
                .codec()
                .output_format()
                .i32(KEY_SAMPLE_RATE)
                .unwrap_or(48_000)
                .max(1) as u32;
            let frames = (payload.len() / (2 * layout.channels() as usize)) as u32;
            let frame = AudioFrame::new(
                SampleFormat::S16,
                sample_rate,
                layout,
                frames,
                Buffer::from_slice(&payload[..layout.byte_len(frames, SampleFormat::S16)]),
                pts,
            )?;
            return Ok(Frame::Audio(frame));
        }

        let (width, height) = self.coded_size();
        let planes = split_planes(self.output_format, width, height, payload)?;
        Ok(Frame::Video(VideoFrame::new(self.output_format, width, height, planes, pts)?))
    }

    /// The coded size MediaCodec reports, honouring the crop rectangle.
    fn coded_size(&self) -> (u32, u32) {
        let format = self.handle.codec().output_format();
        let width = format.i32(KEY_WIDTH).unwrap_or(0).max(0) as u32;
        let height = format.i32(KEY_HEIGHT).unwrap_or(0).max(0) as u32;
        let left = format.i32(KEY_CROP_LEFT).unwrap_or(0).max(0) as u32;
        let top = format.i32(KEY_CROP_TOP).unwrap_or(0).max(0) as u32;
        let right = format.i32(KEY_CROP_RIGHT).unwrap_or(0).max(0) as u32;
        let bottom = format.i32(KEY_CROP_BOTTOM).unwrap_or(0).max(0) as u32;
        if right > left && bottom > top {
            (right - left + 1, bottom - top + 1)
        } else {
            (width.max(2), height.max(2))
        }
    }
}

/// Splits a decoded payload into planes for `format`.
///
/// The buffer MediaCodec hands back is tightly packed at the *coded* size, so the
/// plane sizes follow from the geometry alone. `payload` must be at least as long
/// as the layout requires.
fn split_planes(
    format: PixelFormat,
    width: u32,
    height: u32,
    payload: &[u8],
) -> Result<Vec<Plane>> {
    let mut planes = Vec::with_capacity(format.plane_count());
    let mut cursor = 0usize;
    for index in 0..format.plane_count() {
        let (plane_width, plane_height) = format.plane_size(width, height, index)?;
        let stride = format.default_stride(plane_width, 0)?;
        let length = stride * plane_height as usize;
        let end = cursor + length;
        let data = if end <= payload.len() {
            &payload[cursor..end]
        } else {
            // A short buffer is a device quirk rather than a caller error: take
            // what is there and let the frame report its real extent.
            &payload[cursor.min(payload.len())..]
        };
        cursor = end;
        planes.push(Plane::new(Buffer::from_slice(data), stride)?);
    }
    Ok(planes)
}

impl Decoder for MediaCodecDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if self.end_of_stream {
            return Err(Error::state("mediacodec: the decoder is already draining"));
        }
        if packet.is_header() {
            // Configuration arrives through `DecoderConfig::extradata`.
            return Ok(());
        }
        let eos = packet.is_eos();
        let payload = packet.data.to_vec();
        let written = write_input(self.handle.codec(), &payload)?;
        if written == 0 {
            return Err(Error::state(
                "mediacodec: no input buffer became available; drain the decoder first",
            ));
        }
        self.handle
            .codec()
            .queue_input_buffer_by_index(
                0,
                0,
                written,
                packet.pts.max(0) as u64,
                if eos { BUFFER_FLAG_END_OF_STREAM } else { 0 },
            )
            .map_err(|error| to_backend_error("queueInputBuffer", error))?;
        self.end_of_stream = eos;
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(Some(frame));
        }
        self.drain()?;
        Ok(self.pending.pop_front())
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        self.end_of_stream = true;
        self.drain()?;
        Ok(self.pending.drain(..).collect())
    }
}

impl Drop for MediaCodecDecoder {
    fn drop(&mut self) {
        let _ = self.handle.codec().stop();
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Registers every codec the device actually exposes.
pub fn register(registry: &mut Registry) {
    for codec in Codec::ALL {
        for kind in CodecKind::ALL {
            if component_name(codec, kind).is_none() {
                continue;
            }
            let probe = move |codec: Codec| super::probe(codec, kind);
            match kind {
                CodecKind::Encoder => {
                    let factory = move |config: &EncoderConfig| {
                        Ok(Some(Box::new(MediaCodecEncoder::new(config)?) as Box<dyn Encoder>))
                    };
                    registry.register_encoder(
                        codec,
                        BackendKind::MediaCodec,
                        super::super::priority::HARDWARE_ENGINE,
                        factory,
                        probe,
                    );
                }
                CodecKind::Decoder => {
                    let factory = move |config: &DecoderConfig| {
                        Ok(Some(Box::new(MediaCodecDecoder::new(config)?) as Box<dyn Decoder>))
                    };
                    registry.register_decoder(
                        codec,
                        BackendKind::MediaCodec,
                        super::super::priority::HARDWARE_ENGINE,
                        factory,
                        probe,
                    );
                }
            }
        }
    }
}

/// Audio codecs MediaCodec is documented to expose on Android.
pub const TYPICAL_AUDIO_DECODERS: [AudioCodec; 6] = [
    AudioCodec::Aac,
    AudioCodec::Mp3,
    AudioCodec::Vorbis,
    AudioCodec::Opus,
    AudioCodec::Flac,
    AudioCodec::Alac,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_heuristic_matches_android_conventions() {
        assert!(!is_hardware_component("OMX.google.h264.decoder"));
        assert!(!is_hardware_component("c2.android.avc.decoder"));
        assert!(!is_hardware_component("c2.google.aac.encoder"));
        assert!(!is_hardware_component("vendor.software.avc.decoder"));
        assert!(is_hardware_component("c2.qti.avc.decoder"));
        assert!(is_hardware_component("OMX.qcom.video.encoder.hevc"));
        assert!(is_hardware_component("c2.exynos.h264.decoder"));
    }

    #[test]
    fn color_formats_round_trip() {
        assert_eq!(
            pixel_format_from_color_format(COLOR_FORMAT_YUV420_PLANAR),
            Some(PixelFormat::I420)
        );
        assert_eq!(
            pixel_format_from_color_format(COLOR_FORMAT_YUV420_SEMIPLANAR),
            Some(PixelFormat::Nv12)
        );
        assert_eq!(
            pixel_format_from_color_format(COLOR_FORMAT_YUV420_FLEXIBLE),
            Some(PixelFormat::I420)
        );
        assert_eq!(pixel_format_from_color_format(0x7F000789), None);
    }

    #[test]
    fn avcc_is_converted_to_annex_b() {
        // avcC: version, profile, compat, level, lengthSizeMinusOne,
        // numSPS=1 (0xE1), SPS length 3, SPS bytes, numPPS=1, PPS length 2, PPS bytes.
        let avcc = [
            0x01, 0x64, 0x00, 0x1f, 0xff, 0xe1, 0x00, 0x03, 0x67, 0x64, 0x00, 0x01, 0x00, 0x02,
            0x68, 0xee,
        ];
        let sets = annex_b_parameter_sets(Codec::Video(VideoCodec::H264), &avcc, false);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0], vec![0, 0, 0, 1, 0x67, 0x64, 0x00]);
        assert_eq!(sets[1], vec![0, 0, 0, 1, 0x68, 0xee]);
    }

    #[test]
    fn annex_b_input_is_passed_through_unchanged() {
        let stream = [0u8, 0, 0, 1, 0x67, 0x11, 0, 0, 1, 0x68, 0x22];
        let sets = annex_b_parameter_sets(Codec::Video(VideoCodec::H264), &stream, true);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0], vec![0x67, 0x11]);
        assert_eq!(sets[1], vec![0x68, 0x22]);
    }

    #[test]
    fn start_codes_are_detected_without_the_hint() {
        let stream = [0u8, 0, 1, 0x67, 0x11];
        let sets = annex_b_parameter_sets(Codec::Video(VideoCodec::H265), &stream, false);
        assert_eq!(sets, vec![vec![0x67, 0x11]]);
    }

    #[test]
    fn empty_or_unknown_extradata_yields_nothing() {
        assert!(annex_b_parameter_sets(Codec::Video(VideoCodec::H264), &[], false).is_empty());
        // An avcC that claims no parameter sets.
        let avcc = [0x01, 0x64, 0x00, 0x1f, 0xff, 0xe0, 0x00];
        assert!(annex_b_parameter_sets(Codec::Video(VideoCodec::H264), &avcc, false).is_empty());
        // AAC extradata is not a NAL record.
        assert!(
            annex_b_parameter_sets(Codec::Audio(AudioCodec::Aac), &[0x12, 0x10], false).is_empty()
        );
    }

    #[test]
    fn hvcc_is_converted_to_annex_b() {
        let mut hvcc = vec![0u8; 23];
        hvcc[0] = 1;
        hvcc[22] = 1; // one array
        hvcc.push(0x80 | 32); // completeness + VPS NAL type
        hvcc.extend_from_slice(&[0, 1]); // one NAL
        hvcc.extend_from_slice(&[0, 2, 0x40, 0x01]);
        let sets = annex_b_parameter_sets(Codec::Video(VideoCodec::H265), &hvcc, false);
        assert_eq!(sets, vec![vec![0, 0, 0, 1, 0x40, 0x01]]);
    }

    #[test]
    fn plane_splitting_follows_the_geometry() {
        // 4x4 I420 is 16 + 4 + 4 bytes.
        let payload = vec![7u8; 24];
        let planes = split_planes(PixelFormat::I420, 4, 4, &payload).unwrap();
        assert_eq!(planes.len(), 3);
        assert_eq!(planes[0].len(), 16);
        assert_eq!(planes[1].len(), 4);
        assert_eq!(planes[2].len(), 4);
        assert_eq!(planes[0].stride(), 4);
        // NV12 is 16 + 8.
        let planes = split_planes(PixelFormat::Nv12, 4, 4, &vec![0u8; 24]).unwrap();
        assert_eq!(planes.len(), 2);
        assert_eq!(planes[1].len(), 8);
    }

    #[test]
    fn short_payloads_do_not_panic() {
        let planes = split_planes(PixelFormat::I420, 4, 4, &[1, 2, 3]).unwrap();
        assert_eq!(planes.len(), 3);
    }

    #[test]
    fn typical_audio_decoders_are_the_documented_set() {
        assert_eq!(TYPICAL_AUDIO_DECODERS.len(), 6);
        for codec in TYPICAL_AUDIO_DECODERS {
            assert!(Codec::Audio(codec).is_audio());
        }
    }

    #[test]
    fn framework_availability_is_a_bool_not_a_panic() {
        let _ = framework_available();
    }
}
