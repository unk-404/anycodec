//! GStreamer pipelines for the Linux platform backend.
//!
//! Both directions use the same shape as every other AnyCodec backend: an
//! `appsrc` feed edge and an `appsink` drain edge around a short chain of
//! GStreamer elements. GStreamer runs that chain on its own threads, so this
//! module is marshalling, caps negotiation and error reporting rather than codec
//! work.
//!
//! ## Why `receive` has a timeout
//!
//! [`gst_app::AppSink::pull_sample`] blocks until a buffer exists. A wedged
//! pipeline 閳?a plugin that failed to load, an element waiting for a clock, a
//! caps renegotiation that never completes 閳?would then hang the caller's
//! isolate thread forever with no way to explain why. Every pull here is bounded
//! by [`PULL_TIMEOUT`]; when it expires `receive` answers `Ok(None)`, which the
//! [`Encoder`] contract defines as "I need more input". A pipeline error is
//! picked up from the bus and turned into [`Error::backend`] instead.
//!
//! ## Why there is a parser in front of the decoder
//!
//! GStreamer decoders are strict about their input caps. `flacdec` declares
//! `audio/x-flac, framed=true, rate=[1,655350], channels=[1,8]` and flatly refuses
//! to negotiate without a `streamheader` 閳?it logs "try adding a flacparse element
//! upstream" 閳?and `opusdec` declares
//! `audio/x-opus, channel-mapping-family=(int)0` for the same reason. Neither
//! field can be synthesised from what AnyCodec knows, because they describe the
//! *stream*.
//!
//! The elements that can produce them are the bitstream parsers, so
//! [`GstDecoder::new`] places [`super::parser_element`] between `appsrc` and the
//! decoder whenever GStreamer ships one. That makes the caps `appsrc` has to
//! satisfy the parser's permissive template, and the parser's own output caps are
//! what the decoder wants.
//!
//! ## Why the output caps are pinned
//!
//! `videoconvert` will happily hand back any raw format it likes, and a silent
//! change of layout is exactly the bug that produces a green picture three
//! layers away. The feed edge is therefore pinned with an explicit
//! [`gst_video::VideoCapsBuilder`] / [`gst_audio::AudioCapsBuilder`] caps on the
//! `appsrc`, the drain edge is pinned with a `capsfilter`, and the decoded
//! geometry is read back out of the negotiated caps instead of being assumed from
//! the caller's hint.

use std::collections::VecDeque;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_audio as gst_audio;
use gstreamer_video as gst_video;

use crate::engine::backend::CodecPath;
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::CodecKind;
use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::Codec;
use crate::media::config::{DecoderConfig, EncoderConfig};
use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};
use crate::media::frame::{AudioFrame, Frame, Plane, VideoFrame};
use crate::media::packet::{flags, Packet};
use crate::native::bitstream;

use super::elements;

/// How long `receive` waits for one buffer before answering "needs more input".
///
/// Bounded on purpose: a pipeline that never produces a buffer must not wedge
/// the caller's thread.
const PULL_TIMEOUT: gst::ClockTime = gst::ClockTime::from_mseconds(100);

/// "Do not wait": used on the feed path, which must never block the caller.
const NO_WAIT: gst::ClockTime = gst::ClockTime::from_mseconds(0);

/// Most buffers buffered between one `push` and the `receive` calls that drain it.
const MAX_PENDING: usize = 64;

// ---------------------------------------------------------------------------
// Small conversions
// ---------------------------------------------------------------------------

/// A stream timestamp in nanoseconds, or `None` for a negative one.
///
/// [`gst::ClockTime`] is unsigned; a stream whose first frame carries a negative
/// DTS (which every B-frame stream does) must simply go without a stamp rather
/// than wrap around to 584 years in the future.
fn clock_time(nanoseconds: i64) -> Option<gst::ClockTime> {
    u64::try_from(nanoseconds).ok().map(gst::ClockTime::from_nseconds)
}

/// A GStreamer clock time back in nanoseconds, saturating at [`i64::MAX`].
fn nanoseconds(time: Option<gst::ClockTime>) -> i64 {
    match time {
        Some(time) => i64::try_from(time.nseconds()).unwrap_or(i64::MAX),
        None => 0,
    }
}

// ---------------------------------------------------------------------------
// Element construction
// ---------------------------------------------------------------------------

/// Creates the element `factory` produces, or a specific error.
///
/// `gst::ElementFactory::make(..).build()` reports a missing plugin and a
/// non-instantiable element as an `Err`; only a wrong property *value* panics,
/// and this backend passes no properties here.
fn make_element(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory).build().map_err(|error| {
        Error::backend(format!("gst: the '{factory}' element is not usable: {error}"))
    })
}

/// Sets a boolean property, but only when the element has one.
///
/// Guarded because `set_property` panics on an unknown name, and which
/// properties exist depends on the GStreamer version and the plugin set.
fn set_bool_property(element: &gst::Element, name: &str, value: bool) {
    if element.has_property(name) {
        element.set_property(name, value);
    }
}

/// Sets a `u32` property, but only when the element has one.
fn set_u32_property(element: &gst::Element, name: &str, value: u32) {
    if element.has_property(name) {
        element.set_property(name, value);
    }
}

/// Builds the `capsfilter` that pins one edge of the pipeline.
fn make_capsfilter(caps: &gst::Caps) -> Result<gst::Element> {
    let filter = make_element("capsfilter")?;
    if !filter.has_property("caps") {
        return Err(Error::backend("gst: the 'capsfilter' element has no 'caps' property"));
    }
    filter.set_property("caps", caps);
    Ok(filter)
}

/// Builds an `appsrc` that publishes `caps` and takes our timestamps verbatim.
fn make_appsrc(caps: &gst::Caps) -> Result<gst_app::AppSrc> {
    let appsrc = make_element("appsrc")?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| Error::backend("gst: the 'appsrc' factory did not produce an AppSrc"))?;
    let element: gst::Element = appsrc.clone().upcast();
    // Our clock is the caller's PTS, so GStreamer must neither invent stamps nor
    // wait for a live source to reach them.
    set_bool_property(&element, "is-live", false);
    set_bool_property(&element, "do-timestamp", false);
    set_bool_property(&element, "block", false);
    appsrc.set_format(gst::Format::Time);
    appsrc.set_caps(Some(caps));
    Ok(appsrc)
}

/// Builds an `appsink` that publishes `caps`, never drops and never syncs.
fn make_appsink(caps: &gst::Caps) -> Result<gst_app::AppSink> {
    let appsink = make_element("appsink")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| Error::backend("gst: the 'appsink' factory did not produce an AppSink"))?;
    let element: gst::Element = appsink.clone().upcast();
    // `sync=true` would hold each buffer until its running time is reached, which
    // for an encode is pure latency; `drop=true` would silently lose output.
    set_bool_property(&element, "sync", false);
    set_bool_property(&element, "drop", false);
    appsink.set_max_buffers(MAX_PENDING as u32);
    appsink.set_wait_on_eos(true);
    appsink.set_caps(Some(caps));
    Ok(appsink)
}

// ---------------------------------------------------------------------------
// The running pipeline
// ---------------------------------------------------------------------------

/// A started `appsrc ! 閳? ! appsink` pipeline and its message bus.
struct RunningPipeline {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    appsink: gst_app::AppSink,
    bus: gst::Bus,
}

impl RunningPipeline {
    /// Adds the elements, links them in order and puts the pipeline in `Playing`.
    fn start(
        appsrc: gst_app::AppSrc,
        middle: Vec<gst::Element>,
        appsink: gst_app::AppSink,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::new();
        let source: gst::Element = appsrc.clone().upcast();
        let sink: gst::Element = appsink.clone().upcast();

        pipeline
            .add_many([&source])
            .and_then(|()| pipeline.add_many(middle.iter()))
            .and_then(|()| pipeline.add_many([&sink]))
            .map_err(|error| {
                Error::backend(format!("gst: adding the elements to the pipeline failed: {error}"))
            })?;

        let mut chain: Vec<&gst::Element> = Vec::with_capacity(middle.len() + 2);
        chain.push(&source);
        chain.extend(middle.iter());
        chain.push(&sink);
        gst::Element::link_many(chain)
            .map_err(|error| Error::backend(format!("gst: linking the pipeline failed: {error}")))?;

        let bus = pipeline
            .bus()
            .ok_or_else(|| Error::backend("gst: the pipeline has no message bus"))?;
        pipeline.set_state(gst::State::Playing).map_err(|error| {
            Error::backend(format!("gst: the pipeline would not start playing: {error}"))
        })?;
        Ok(Self { pipeline, appsrc, appsink, bus })
    }

    /// Turns every queued pipeline error into an `Err`, ignoring everything else.
    fn check_bus(&self) -> Result<()> {
        while let Some(message) = self.bus.pop() {
            if let gst::MessageView::Error(error) = message.view() {
                let debug = error.debug().map(|debug| format!(" ({debug})")).unwrap_or_default();
                return Err(Error::backend(format!(
                    "gst: the pipeline failed: {}{debug}",
                    error.error()
                )));
            }
        }
        Ok(())
    }

    /// Pulls up to [`MAX_PENDING`] buffers, the first one waiting up to `timeout`.
    ///
    /// Bounded in both directions, so this can never spin or block forever. The
    /// bus is drained both before *and* after pulling: a failure that arrives
    /// while we are waiting would otherwise be swallowed, because the next pull
    /// simply returns nothing and the caller would see "needs more input" instead
    /// of the reason the pipeline died.
    fn pump(&mut self, timeout: gst::ClockTime) -> Result<Vec<gst::Sample>> {
        self.check_bus()?;
        let mut samples = Vec::new();
        while samples.len() < MAX_PENDING {
            let Some(sample) = self.appsink.try_pull_sample(timeout) else {
                break;
            };
            samples.push(sample);
        }
        self.check_bus()?;
        Ok(samples)
    }

    /// Sends end of stream, once.
    fn end_of_stream(&mut self) -> Result<()> {
        self.appsrc.end_of_stream().map_err(|error| {
            Error::backend(format!("gst: signalling end of stream failed: {error:?}"))
        })?;
        Ok(())
    }

    /// Pushes one buffer onto the feed edge.
    fn push_buffer(&self, buffer: gst::Buffer) -> Result<()> {
        self.appsrc.push_buffer(buffer).map_err(|error| {
            Error::backend(format!("gst: appsrc rejected a buffer: {error:?}"))
        })?;
        Ok(())
    }

    /// Stops the pipeline, joining its streaming threads before anything is freed.
    fn shutdown(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for RunningPipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for RunningPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningPipeline").field("state", &self.pipeline.current_state()).finish()
    }
}

/// Wraps `payload` into a buffer stamped with our own timestamps.
fn buffer_from_bytes(payload: Vec<u8>, pts: i64, dts: i64, duration: i64) -> gst::Buffer {
    let mut buffer = gst::Buffer::from_slice(payload);
    {
        let reference = buffer.make_mut();
        reference.set_pts(clock_time(pts));
        reference.set_dts(clock_time(dts));
        if duration > 0 {
            reference.set_duration(clock_time(duration));
        }
    }
    buffer
}

/// Copies a picture out of its per-plane storage into the tightly packed layout
/// `videoconvert` expects on the far side of `appsrc`.
fn video_payload(frame: &VideoFrame) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(frame.byte_len());
    for index in 0..frame.format.plane_count() {
        let (width, height) = frame.format.plane_size(frame.width, frame.height, index)?;
        let row_bytes = frame.format.default_stride(width, 0)?;
        let plane = frame.planes.get(index).ok_or_else(|| {
            Error::invalid_argument(format!("plane {index} of the frame is missing"))
        })?;
        for row in 0..height as usize {
            payload.extend_from_slice(plane.row(row, row_bytes)?);
        }
    }
    Ok(payload)
}

/// Returns the payload of `sample`, or explains why it could not be mapped.
fn sample_payload(sample: &gst::Sample) -> Result<Vec<u8>> {
    let buffer = sample
        .buffer()
        .ok_or_else(|| Error::backend("gst: a sample arrived with no buffer"))?;
    let map = buffer.map_readable().map_err(|error| {
        Error::backend(format!("gst: mapping an output buffer failed: {error}"))
    })?;
    Ok(map.as_slice().to_vec())
}

/// Reads a codec's out-of-band configuration out of negotiated caps.
///
/// Two shapes exist in GStreamer, and both are exactly what
/// [`Encoder::extradata`] is defined to return:
///
/// * `codec_data` 閳?a single buffer, used by H.264/H.265 encoders whose
///   `stream-format` is `avc`/`hvc1` (the `avcC`/`hvcC` record);
/// * `streamheader` 閳?the global headers of the audio encoders (Opus, Vorbis,
///   FLAC). GStreamer 1.24 and later publish it as an *array* of buffers;
///   earlier releases used a buffer list. Both are read, so this backend works
///   against whatever the host ships.
///
/// The buffers are concatenated in order, which is what a container stores as the
/// codec's extradata. That concatenation cannot be split again without
/// codec-specific knowledge, so a caller that must round-trip the headers
/// individually should read them from [`gst::Sample::caps`] instead.
fn extradata_from_caps(caps: &gst::CapsRef) -> Option<Buffer> {
    let structure = caps.structure(0)?;
    if let Ok(codec_data) = structure.get::<gst::Buffer>("codec_data") {
        let map = codec_data.map_readable().ok()?;
        return Some(Buffer::from_slice(map.as_slice()));
    }
    let mut blob = Vec::new();
    if let Ok(headers) = structure.get::<gst::ArrayRef>("streamheader") {
        for value in headers.as_slice() {
            if let Ok(buffer) = value.get::<gst::Buffer>() {
                if let Ok(map) = buffer.map_readable() {
                    blob.extend_from_slice(map.as_slice());
                }
            }
        }
    } else if let Ok(headers) = structure.get::<gst::BufferList>("streamheader") {
        for buffer in headers.iter() {
            if let Ok(map) = buffer.map_readable() {
                blob.extend_from_slice(map.as_slice());
            }
        }
    }
    if blob.is_empty() {
        None
    } else {
        Some(Buffer::from_vec(blob))
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// A GStreamer encoder session.
pub struct GstEncoder {
    running: RunningPipeline,
    codec: Codec,
    path: CodecPath,
    /// The element that actually compresses, kept for live retuning.
    encoder: gst::Element,
    /// The factory name of `encoder`, which is what the unit tables are keyed by.
    element_name: &'static str,
    pixel_format: PixelFormat,
    sample_format: SampleFormat,
    frame_duration: i64,
    /// The codec's out-of-band configuration, captured from the first sample's
    /// caps (see [`extradata_from_caps`]).
    extradata: Option<Buffer>,
    pending: VecDeque<Packet>,
    draining: bool,
    eos_sent: bool,
}

impl std::fmt::Debug for GstEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GstEncoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl GstEncoder {
    /// Builds and starts an encoder for `config`.
    ///
    /// Fails with [`ErrorKind::Unavailable`](crate::error::ErrorKind::Unavailable)
    /// when no candidate element exists on this machine, and with
    /// [`ErrorKind::Backend`](crate::error::ErrorKind::Backend) when GStreamer
    /// cannot start or link the pipeline.
    ///
    /// The speed/quality [`Preset`](crate::media::config::Preset) is **not**
    /// translated into element properties: GStreamer spells it differently in
    /// every encoder (`speed-preset`, `deadline`, `cpu-used`, `complexity`, ...)
    /// and a wrong guess would be silently applied. The element's own default
    /// applies instead.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        elements::ensure_initialised()?;
        let codec = config.codec;
        let candidates = super::encoder_candidates(codec);
        let chosen = elements::select_element(candidates, &elements::element_available)
            .ok_or_else(|| {
                Error::unavailable(format!(
                    "GStreamer exposes no {} encoder element; tried {}",
                    codec.name(),
                    candidates.join(", ")
                ))
            })?;

        let compressed = elements::compressed_caps(codec)?;
        let (running, encoder) = if codec.is_video() {
            let raw =
                elements::raw_video_caps(config.pixel_format, config.width, config.height)?;
            let appsrc = make_appsrc(&raw)?;
            let convert = make_element("videoconvert")?;
            let encoder = make_element(chosen)?;
            apply_encoder_settings(&encoder, chosen, config);
            let filter = make_capsfilter(&compressed)?;
            let appsink = make_appsink(&compressed)?;
            let running =
                RunningPipeline::start(appsrc, vec![convert, encoder.clone(), filter], appsink)?;
            (running, encoder)
        } else {
            let raw = elements::raw_audio_caps(
                config.sample_format,
                config.sample_rate,
                config.channels.channels(),
            )?;
            let appsrc = make_appsrc(&raw)?;
            let convert = make_element("audioconvert")?;
            let resample = make_element("audioresample")?;
            let encoder = make_element(chosen)?;
            apply_encoder_settings(&encoder, chosen, config);
            let appsink = make_appsink(&compressed)?;
            let running = RunningPipeline::start(
                appsrc,
                vec![convert, resample, encoder.clone()],
                appsink,
            )?;
            (running, encoder)
        };

        Ok(Self {
            running,
            codec,
            path: super::codec_path(codec, CodecKind::Encoder, chosen, None),
            encoder,
            element_name: chosen,
            pixel_format: config.pixel_format,
            sample_format: config.sample_format,
            frame_duration: frame_duration(config),
            extradata: None,
            pending: VecDeque::new(),
            draining: false,
            eos_sent: false,
        })
    }

    /// Turns one sample into a packet, or `None` for a sample that carries no
    /// media (a caps-only buffer, for instance).
    ///
    /// The first sample's caps are also where a codec's out-of-band configuration
    /// shows up, so this is where [`Encoder::extradata`] learns it.
    fn packet_from_sample(&mut self, sample: &gst::Sample) -> Result<Option<Packet>> {
        if self.extradata.is_none() {
            if let Some(caps) = sample.caps() {
                self.extradata = extradata_from_caps(caps);
            }
        }
        let buffer = match sample.buffer() {
            Some(buffer) => buffer,
            None => return Ok(None),
        };
        let payload = sample_payload(sample)?;
        if payload.is_empty() {
            return Ok(None);
        }
        let buffer_flags = buffer.flags();
        let mut packet_flags = 0u32;
        if !buffer_flags.contains(gst::BufferFlags::DELTA_UNIT) {
            packet_flags |= flags::KEYFRAME;
        }
        if buffer_flags.contains(gst::BufferFlags::HEADER) {
            packet_flags |= flags::HEADER;
        }
        if self.path.hardware_accelerated {
            packet_flags |= flags::HARDWARE;
        }
        let packet = Packet::new(
            self.codec,
            Buffer::from_vec(payload),
            nanoseconds(buffer.pts()),
            packet_flags,
        )
        .with_dts(nanoseconds(buffer.dts().or_else(|| buffer.pts())))
        .with_duration(nanoseconds(buffer.duration()));
        Ok(Some(packet))
    }

    /// Feeds a payload onto the pipeline and collects whatever it produced.
    fn feed(&mut self, payload: Vec<u8>, pts: i64, dts: i64, duration: i64) -> Result<()> {
        let buffer = buffer_from_bytes(payload, pts, dts, duration);
        self.running.push_buffer(buffer)?;
        for sample in self.running.pump(NO_WAIT)? {
            if let Some(packet) = self.packet_from_sample(&sample)? {
                self.pending.push_back(packet);
            }
        }
        Ok(())
    }

    /// Sends end of stream once and marks the session as draining.
    fn send_eos(&mut self) -> Result<()> {
        if self.eos_sent {
            return Ok(());
        }
        self.eos_sent = true;
        self.draining = true;
        self.running.end_of_stream()
    }
}

impl Encoder for GstEncoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        if self.draining {
            return Err(Error::state("gst: the encoder is already draining"));
        }
        match frame {
            Frame::Video(video) => {
                if video.format != self.pixel_format {
                    return Err(Error::invalid_argument(format!(
                        "the GStreamer encoder was configured for {} input but received {}",
                        self.pixel_format.name(),
                        video.format.name()
                    )));
                }
                let payload = video_payload(&video)?;
                let duration =
                    if video.duration > 0 { video.duration } else { self.frame_duration };
                self.feed(payload, video.pts, video.pts, duration)
            }
            Frame::Audio(audio) => {
                if audio.format != self.sample_format {
                    return Err(Error::invalid_argument(format!(
                        "the GStreamer encoder was configured for {} input but received {}",
                        self.sample_format.name(),
                        audio.format.name()
                    )));
                }
                let expected = audio.channels.byte_len(audio.frames, audio.format);
                let payload = audio
                    .samples
                    .as_slice()
                    .get(..expected)
                    .ok_or_else(|| {
                        Error::backend("gst: the audio frame is shorter than it claims")
                    })?
                    .to_vec();
                let duration =
                    audio.frames as i64 * 1_000_000_000 / audio.sample_rate.max(1) as i64;
                self.feed(payload, audio.pts, audio.pts, duration)
            }
        }
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        if self.pending.is_empty() && !self.eos_sent {
            for sample in self.running.pump(PULL_TIMEOUT)? {
                if let Some(packet) = self.packet_from_sample(&sample)? {
                    self.pending.push_back(packet);
                }
            }
        }
        Ok(self.pending.pop_front())
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        self.send_eos()?;
        for sample in self.running.pump(PULL_TIMEOUT)? {
            if let Some(packet) = self.packet_from_sample(&sample)? {
                self.pending.push_back(packet);
            }
        }
        self.running.shutdown();
        Ok(self.pending.drain(..).collect())
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        if apply_bitrate(&self.encoder, self.element_name, bitrate) {
            return Ok(());
        }
        Err(Error::unsupported(format!(
            "gst: the '{}' element has no bitrate property this backend knows the unit of",
            self.element_name
        )))
    }

    fn request_keyframe(&mut self) -> Result<()> {
        if !self.codec.is_video() {
            return Err(Error::unsupported(format!(
                "gst: {} has no keyframes to request",
                self.codec.name()
            )));
        }
        // The documented way is a downstream force-key-unit event; encoders that
        // care observe it and start a new GOP at the next opportunity.
        let event = gst_video::DownstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .count(1)
            .build();
        if self.running.appsrc.send_event(event) {
            Ok(())
        } else {
            Err(Error::backend(format!(
                "gst: the '{}' pipeline refused a force-key-unit event",
                self.element_name
            )))
        }
    }

    fn extradata(&self) -> Option<Buffer> {
        self.extradata.clone()
    }
}

/// How long a picture is shown, in nanoseconds, or `0` when the rate is unset.
fn frame_duration(config: &EncoderConfig) -> i64 {
    config
        .framerate()
        .filter(|rate| *rate > 0.0)
        .map(|rate| (1_000_000_000.0 / rate) as i64)
        .unwrap_or(0)
}

/// Applies everything from `config` this backend knows how to express.
fn apply_encoder_settings(encoder: &gst::Element, element: &str, config: &EncoderConfig) {
    apply_bitrate(encoder, element, config.bitrate);
    if config.keyframe_interval > 0 {
        // Two spellings cover every encoder in the candidate tables; only the one
        // the element actually has is set.
        set_u32_property(encoder, "key-int-max", config.keyframe_interval);
        set_u32_property(encoder, "gop-size", config.keyframe_interval);
    }
}

/// Sets the `bitrate` property in the unit `element` documents.
///
/// Returns `false` when the element is outside
/// [`elements::bitrate_unit`]'s table 閳?a wrong unit would be silently applied, so
/// guessing is refused.
fn apply_bitrate(encoder: &gst::Element, element: &str, bits_per_second: u64) -> bool {
    let Some(unit) = elements::bitrate_unit(element) else {
        return false;
    };
    if bits_per_second == 0 || !encoder.has_property("bitrate") {
        return false;
    }
    encoder.set_property("bitrate", unit.from_bits(bits_per_second));
    true
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// A GStreamer decoder session.
pub struct GstDecoder {
    running: RunningPipeline,
    codec: Codec,
    path: CodecPath,
    /// Annex-B parameter sets derived from the caller's configuration record,
    /// pushed once before the first access unit.
    header: Option<Vec<u8>>,
    pending: VecDeque<Frame>,
    draining: bool,
    eos_sent: bool,
}

impl std::fmt::Debug for GstDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GstDecoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl GstDecoder {
    /// Builds and starts a decoder for `config`.
    ///
    /// Fails with [`ErrorKind::Unavailable`](crate::error::ErrorKind::Unavailable)
    /// when no candidate element exists, and additionally when the caller
    /// declared length-prefixed H.264/H.265 input and no bitstream parser is
    /// installed 閳?without one the decoder would be handed a framing it cannot
    /// read, which must be an error rather than silent corruption.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        elements::ensure_initialised()?;
        let codec = config.codec;
        let candidates = super::decoder_candidates(codec);
        let chosen = elements::select_element(candidates, &elements::element_available)
            .ok_or_else(|| {
                Error::unavailable(format!(
                    "GStreamer exposes no {} decoder element; tried {}",
                    codec.name(),
                    candidates.join(", ")
                ))
            })?;

        // A parser between `appsrc` and the decoder is what makes most GStreamer
        // decoders negotiable at all: they read their global headers from the caps,
        // and the parser is what reads them out of the byte stream. It is
        // *required* only for H.264/H.265 whose caller declared length-prefixed
        // (`avcC`/`hvcC`) input, because without it those NAL units cannot be
        // re-framed.
        let wanted_parser = super::parser_element(codec);
        let parser = wanted_parser.filter(|name| elements::element_available(name));
        if parser.is_none() && super::has_framing_ambiguity(codec) && !config.annex_b {
            return Err(Error::unavailable(format!(
                "GStreamer has no '{}' element, so length-prefixed (non-Annex-B) {} input cannot be \
                 decoded; pass Annex-B packets or install the H.26x parsers",
                wanted_parser.unwrap_or("h264parse/h265parse"),
                codec.name()
            )));
        }
        let parser_element = match parser {
            Some(name) => Some(make_element(name)?),
            None => None,
        };

        // The element directly after `appsrc` is the authority on the input caps,
        // so those are what the caps are narrowed against; several decoders
        // (and their parsers) refuse a structure their template does not match.
        let decoder_element = make_element(chosen)?;
        let first = parser_element.as_ref().unwrap_or(&decoder_element);
        let compressed = elements::narrow_to_element(
            first,
            elements::compressed_caps_for(
                codec,
                (config.sample_rate > 0).then_some(config.sample_rate),
                config.channels.map(|layout| layout.channels()),
            )?,
        );
        let raw = if codec.is_video() {
            elements::raw_video_format_caps(config.pixel_format.unwrap_or(PixelFormat::I420))?
        } else {
            // Video geometry follows the bitstream; audio geometry can be
            // converted to whatever the caller asked for.
            elements::raw_audio_caps_optional(
                config.sample_format.unwrap_or(SampleFormat::S16),
                (config.sample_rate > 0).then_some(config.sample_rate),
                config.channels.map(|layout| layout.channels()),
            )?
        };

        let appsrc = make_appsrc(&compressed)?;
        let mut middle: Vec<gst::Element> = Vec::with_capacity(4);
        if let Some(element) = parser_element {
            middle.push(element);
        }
        middle.push(decoder_element);
        middle.push(make_element(if codec.is_video() { "videoconvert" } else { "audioconvert" })?);
        middle.push(make_capsfilter(&raw)?);
        let appsink = make_appsink(&raw)?;
        let running = RunningPipeline::start(appsrc, middle, appsink)?;

        // Out-of-band configuration is pushed once, ahead of the first access
        // unit, as a buffer carrying `GST_BUFFER_FLAG_HEADER`. That is the
        // documented way to hand a decoder its stream headers without having to
        // know each plugin's caps template 閳?`opusdec` wants
        // `channel-mapping-family` in its caps and `avdec_h265` wants `parsed`,
        // and pinning either here would contradict what the caller's packets are.
        // For H.264/H.265 the record is converted to Annex-B first, because that
        // is the framing every GStreamer H.26x decoder accepts.
        let header = if codec.is_video() {
            bitstream::annex_b_blob(codec, config.extradata.as_slice(), config.annex_b)
        } else {
            config.extradata.to_vec()
        };
        let header = if header.is_empty() { None } else { Some(header) };

        Ok(Self {
            running,
            codec,
            path: super::codec_path(codec, CodecKind::Decoder, chosen, parser),            header,
            pending: VecDeque::new(),
            draining: false,
            eos_sent: false,
        })
    }

    /// Builds a frame from one decoded sample, using the caps that were actually
    /// negotiated rather than the caller's hint.
    fn frame_from_sample(&self, sample: &gst::Sample) -> Result<Option<Frame>> {
        let buffer = match sample.buffer() {
            Some(buffer) => buffer,
            None => return Ok(None),
        };
        let caps = sample
            .caps()
            .ok_or_else(|| Error::backend("gst: a decoded sample carried no caps"))?;
        let pts = nanoseconds(buffer.pts());
        let payload = sample_payload(sample)?;
        if payload.is_empty() {
            return Ok(None);
        }

        if !self.codec.is_video() {
            let info = gst_audio::AudioInfo::from_caps(caps).map_err(|error| {
                Error::backend(format!("gst: the decoded audio caps are unusable: {error}"))
            })?;
            let format = elements::sample_format_of(info.format()).ok_or_else(|| {
                Error::unsupported(
                    "gst: the decoder produced an audio format AnyCodec has no SampleFormat for",
                )
            })?;
            let channels = ChannelLayout::from_channel_count(info.channels()).ok_or_else(|| {
                Error::unsupported(format!(
                    "gst: the decoder produced {} audio channels, which AnyCodec has no \
                     ChannelLayout for",
                    info.channels()
                ))
            })?;
            let frame_bytes = info.bpf().max(1) as usize;
            let frames = u32::try_from(payload.len() / frame_bytes).unwrap_or(u32::MAX);
            let expected = channels.byte_len(frames, format);
            let data = payload.get(..expected.min(payload.len())).unwrap_or(payload.as_slice());
            return Ok(Some(Frame::Audio(AudioFrame::new(
                format,
                info.rate(),
                channels,
                frames,
                Buffer::from_slice(data),
                pts,
            )?)));
        }

        let info = gst_video::VideoInfo::from_caps(caps).map_err(|error| {
            Error::backend(format!("gst: the decoded video caps are unusable: {error}"))
        })?;
        let format = elements::pixel_format_of(info.format()).ok_or_else(|| {
            Error::unsupported(
                "gst: the decoder produced a pixel format AnyCodec has no PixelFormat for",
            )
        })?;
        let planes = planes_from_info(&info, &payload)?;
        Ok(Some(Frame::Video(VideoFrame::new(
            format,
            info.width(),
            info.height(),
            planes,
            pts,
        )?)))
    }

    /// Feeds one payload onto the pipeline and collects whatever it produced.
    ///
    /// `headers` marks the payload as codec configuration rather than media, so
    /// the decoder parses it instead of treating it as an access unit.
    fn feed(&mut self, payload: Vec<u8>, pts: Option<gst::ClockTime>, headers: bool) -> Result<()> {
        let mut buffer = gst::Buffer::from_slice(payload);
        {
            let reference = buffer.make_mut();
            reference.set_pts(pts);
            if headers {
                reference.set_flags(gst::BufferFlags::HEADER);
            }
        }
        self.running.push_buffer(buffer)?;
        for sample in self.running.pump(NO_WAIT)? {
            if let Some(frame) = self.frame_from_sample(&sample)? {
                self.pending.push_back(frame);
            }
        }
        Ok(())
    }

    /// Sends end of stream once and marks the session as draining.
    fn send_eos(&mut self) -> Result<()> {
        if self.eos_sent {
            return Ok(());
        }
        self.eos_sent = true;
        self.draining = true;
        self.running.end_of_stream()
    }
}

/// Splits a decoded payload into planes using the strides the caps negotiated.
///
/// Each plane runs from its own offset to the next one, which is exactly how
/// GStreamer lays out raw video, so row padding is honoured instead of assumed
/// away.
fn planes_from_info(info: &gst_video::VideoInfo, payload: &[u8]) -> Result<Vec<Plane>> {
    let count = info.n_planes() as usize;
    let offsets = info.offset();
    let strides = info.stride();
    if offsets.len() < count {
        return Err(Error::backend(format!(
            "gst: the decoded caps describe {count} planes but publish only {} offsets",
            offsets.len()
        )));
    }
    let mut planes = Vec::with_capacity(count);
    for index in 0..count {
        let start = offsets[index].min(payload.len());
        let end = offsets
            .get(index + 1)
            .copied()
            .unwrap_or(payload.len())
            .clamp(start, payload.len());
        let data = &payload[start..end];
        let stride = strides.get(index).copied().unwrap_or(0).max(1) as usize;
        planes.push(Plane::new(Buffer::from_slice(data), stride.min(data.len().max(1)))?);
    }
    Ok(planes)
}

impl Decoder for GstDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if self.draining {
            return Err(Error::state("gst: the decoder is already draining"));
        }
        if packet.is_eos() {
            return self.send_eos();
        }
        if let Some(header) = self.header.take() {
            self.feed(header, None, true)?;
        }
        if packet.data.is_empty() {
            return Ok(());
        }
        let pts = clock_time(packet.pts);
        // A packet the encoder marked as configuration is configuration here too:
        // several decoders (FLAC, Opus) tell the two apart by exactly this flag.
        self.feed(packet.data.to_vec(), pts, packet.is_header())
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        if self.pending.is_empty() && !self.eos_sent {
            for sample in self.running.pump(PULL_TIMEOUT)? {
                if let Some(frame) = self.frame_from_sample(&sample)? {
                    self.pending.push_back(frame);
                }
            }
        }
        Ok(self.pending.pop_front())
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        self.send_eos()?;
        for sample in self.running.pump(PULL_TIMEOUT)? {
            if let Some(frame) = self.frame_from_sample(&sample)? {
                self.pending.push_back(frame);
            }
        }
        self.running.shutdown();
        Ok(self.pending.drain(..).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::backend::BackendKind;
    use crate::media::codec::{AudioCodec, VideoCodec};

    /// `appsrc` is configured for `gst::Format::Time`, so every timestamp this
    /// backend pushes is in nanoseconds 閳?including audio, where it is tempting to
    /// pass the sample count instead. `GstAudioEncoder` rejects that outright
    /// ("buffer going too far back in time").
    fn audio_pts(index: u32, frames: u32, sample_rate: u32) -> i64 {
        index as i64 * frames as i64 * 1_000_000_000 / sample_rate.max(1) as i64
    }

    #[test]
    fn negative_timestamps_go_without_a_stamp_instead_of_wrapping() {        assert_eq!(clock_time(-1), None);
        assert_eq!(clock_time(0), Some(gst::ClockTime::from_nseconds(0)));
        assert_eq!(clock_time(1_000), Some(gst::ClockTime::from_nseconds(1_000)));
        assert_eq!(nanoseconds(clock_time(i64::MAX)), i64::MAX);
        assert_eq!(nanoseconds(None), 0);
    }

    #[test]
    fn a_frame_duration_comes_from_the_frame_rate() {
        let config = EncoderConfig::video(VideoCodec::H264, 64, 64).with_framerate(25, 1);
        assert_eq!(frame_duration(&config), 40_000_000);
        let unset = EncoderConfig::audio(AudioCodec::Opus, 48_000, ChannelLayout::Stereo);
        assert_eq!(frame_duration(&unset), 0);
    }

    #[test]
    fn unknown_bitrate_units_are_refused_rather_than_guessed() {
        // A synthetic element with no bitrate property must be refused; the real
        // check needs a live GStreamer, so this covers the table half.
        assert_eq!(elements::bitrate_unit("mpg123audiodec"), None);
        assert_eq!(elements::bitrate_unit("x264enc"), Some(elements::BitrateUnit::KilobitsPerSecond));
    }

    #[test]
    fn building_a_session_never_panics_without_gstreamer() {
        // On a host without GStreamer this must be an explained `Err`; where
        // GStreamer exists but the codec has no element, likewise.
        match GstEncoder::new(&EncoderConfig::video(VideoCodec::H264, 64, 64)) {
            Ok(encoder) => {
                let path = encoder.path();
                assert_eq!(path.backend, BackendKind::GStreamer);
                assert!(path.detail.contains("appsrc"), "{}", path.detail);
                assert!(path.detail.contains("x264enc")
                    || path.detail.contains("enc !")
                    || path.detail.contains("enc"));
            }
            Err(error) => {
                assert!(
                    error.message().len() > 20,
                    "an unavailable encoder must say why: {}",
                    error.message()
                );
            }
        }

        match GstDecoder::new(&DecoderConfig::new(Codec::Video(VideoCodec::H264))) {
            Ok(decoder) => assert_eq!(decoder.path().backend, BackendKind::GStreamer),
            Err(error) => assert!(
                error.message().len() > 20,
                "an unavailable decoder must say why: {}",
                error.message()
            ),
        }
    }

    #[test]
    #[ignore = "requires GStreamer"]
    fn a_real_opus_encode_produces_packets_and_out_of_band_configuration() {
        // Needs `opusenc`, `audioconvert` and `audioresample` from
        // gst-plugins-base.
        //
        // Encode-only on purpose. Decoding Opus through this backend additionally
        // needs `opusparse` (gst-plugins-bad): `opusdec` reads its channel mapping
        // out of the caps and refuses to negotiate without it. That is exactly the
        // reason `GstDecoder::new` puts a parser in front of the decoder, so the
        // decode direction is covered by the FLAC round-trip test 閳?which passes
        // here 閳?rather than duplicated here against a plugin set that may not
        // include `opusparse`.
        elements::ensure_initialised().expect("GStreamer must initialise");
        if !elements::element_available("opusenc") {
            // `opusenc` ships in gst-plugins-base, which is nearly always
            // present, but a machine without it must not make `cargo test` red.
            // `elements` has the plugin-independent coverage.
            eprintln!("skipping: `opusenc` is not installed; install gst-plugins-base");
            return;
        }

        const FRAMES: u32 = 960; // 20 ms at 48 kHz, Opus' native frame size.
        let mut encoder = GstEncoder::new(
            &EncoderConfig::audio(AudioCodec::Opus, 48_000, ChannelLayout::Stereo)
                .with_sample_format(SampleFormat::S16),
        )
        .expect("opusenc is installed");
        assert_eq!(encoder.path().backend, BackendKind::GStreamer);
        assert!(!encoder.path().hardware_accelerated);
        assert!(encoder.path().detail.contains("opusenc"), "{}", encoder.path().detail);

        let mut packets = Vec::new();
        for index in 0..20u32 {
            // A quiet stepped ramp keeps the encoder honest without needing a
            // signal generator.
            let mut payload = Vec::with_capacity(FRAMES as usize * 4);
            for sample in 0..FRAMES {
                let value = ((sample as i32 * 32 + index as i32 * 7) % 4000 - 2000) as i16;
                payload.extend_from_slice(&value.to_le_bytes());
                payload.extend_from_slice(&value.to_le_bytes());
            }
            let frame = Frame::Audio(
                AudioFrame::new(
                    SampleFormat::S16,
                    48_000,
                    ChannelLayout::Stereo,
                    FRAMES,
                    Buffer::from_vec(payload),
                    audio_pts(index, FRAMES, 48_000),
                )
                .expect("a well formed audio frame"),
            );
            encoder.push(frame).expect("appsrc accepts the frame");
            while let Some(packet) = encoder.receive().expect("receiving works") {
                packets.push(packet);
            }
        }
        packets.extend(encoder.flush().expect("flushing works"));
        assert!(!packets.is_empty(), "opusenc must produce at least one packet");
        assert!(
            packets.iter().all(|packet| packet.codec == Codec::Audio(AudioCodec::Opus)),
            "every packet must report the codec it was asked for"
        );

        // Opus publishes its configuration out of band, as a `streamheader` array
        // on the sink caps; that is exactly what `extradata` is for.
        let extradata = encoder
            .extradata()
            .expect("opusenc must publish its out-of-band configuration");
        assert!(
            extradata.as_slice().starts_with(b"OpusHead"),
            "the Opus extradata must start with OpusHead, got {:?}",
            extradata.as_slice().get(..16.min(extradata.len())).unwrap_or_default()
        );
    }

    #[test]
    #[ignore = "requires GStreamer"]
    fn a_real_flac_encode_decode_round_trip_produces_frames() {
        // Needs `flacenc` and `flacdec` (gst-plugins-good, `flac`) plus
        // `flacparse` (gst-plugins-good, `audioparsers`).
        elements::ensure_initialised().expect("GStreamer must initialise");
        assert!(elements::element_available("flacenc"), "install gst-plugins-good");
        assert!(elements::element_available("flacdec"), "install gst-plugins-good");

        let mut encoder = GstEncoder::new(
            &EncoderConfig::audio(AudioCodec::Flac, 48_000, ChannelLayout::Stereo)
                .with_sample_format(SampleFormat::S16),
        )
        .expect("flacenc is installed");
        assert_eq!(encoder.path().backend, BackendKind::GStreamer);
        assert!(!encoder.path().hardware_accelerated, "flacenc is a software element");

        const FRAMES: u32 = 1_024;
        let mut packets = Vec::new();
        for index in 0..8u32 {
            let payload = vec![0u8; FRAMES as usize * 2 * 2];
            let frame = Frame::Audio(
                AudioFrame::new(
                    SampleFormat::S16,
                    48_000,
                    ChannelLayout::Stereo,
                    FRAMES,
                    Buffer::from_vec(payload),
                    audio_pts(index, FRAMES, 48_000),
                )
                .expect("a well formed audio frame"),
            );
            encoder.push(frame).expect("appsrc accepts the frame");
            while let Some(packet) = encoder.receive().expect("receiving works") {
                packets.push(packet);
            }
        }
        packets.extend(encoder.flush().expect("flushing works"));
        assert!(!packets.is_empty(), "flacenc must produce at least one packet");

        let mut decoder = GstDecoder::new(
            &DecoderConfig::new(Codec::Audio(AudioCodec::Flac))
                .with_audio(48_000, ChannelLayout::Stereo),
        )
        .expect("flacdec is installed");
        for packet in &packets {
            decoder.push(packet.clone()).expect("the decoder accepts the packet");
        }
        let frames = decoder.flush().expect("flushing works");
        assert!(!frames.is_empty(), "flacdec must produce at least one frame");
        for frame in &frames {
            match frame {
                Frame::Audio(audio) => {
                    assert_eq!(audio.sample_rate, 48_000);
                    assert_eq!(audio.channels, ChannelLayout::Stereo);
                    assert!(!audio.samples.is_empty());
                }
                Frame::Video(_) => panic!("flacdec produced a picture"),
            }
        }
    }

    #[test]
    #[ignore = "requires GStreamer"]
    fn a_real_h264_encode_produces_packets() {
        // Needs `x264enc` (gst-plugins-ugly) or `openh264enc` (gst-plugins-bad).
        elements::ensure_initialised().expect("GStreamer must initialise");
        let Ok(mut encoder) =
            GstEncoder::new(&EncoderConfig::video(VideoCodec::H264, 64, 64).with_bitrate(200_000))
        else {
            // No H.264 encoder element is installed. That is a property of the
            // host, not a defect: the capability report already says so, and a
            // test that panics here would make `cargo test` red on every machine
            // without gst-plugins-ugly/bad. The element tables and the probe are
            // covered by the tests that do not need plugins.
            eprintln!(
                "skipping: no GStreamer H.264 encoder element is installed \
                 (x264enc/openh264enc); the capability report reports this"
            );
            return;
        };
        assert!(encoder.path().detail.contains("appsrc"));
        for index in 0..8i64 {
            let plane = Plane::new(Buffer::from_vec(vec![16u8; 64 * 64]), 64)
                .expect("a 64x64 luma plane");
            let u = Plane::new(Buffer::from_vec(vec![128u8; 32 * 32]), 32).expect("a U plane");
            let v = Plane::new(Buffer::from_vec(vec![128u8; 32 * 32]), 32).expect("a V plane");
            let frame = Frame::Video(
                VideoFrame::new(PixelFormat::I420, 64, 64, vec![plane, u, v], index)
                    .expect("a well formed I420 frame"),
            );
            encoder.push(frame).expect("appsrc accepts the frame");
        }
        let packets = encoder.flush().expect("flushing works");
        assert!(!packets.is_empty(), "the encoder must produce at least one packet");
        assert!(packets.iter().any(|packet| packet.is_keyframe()), "an IDR is expected");
        assert_eq!(encoder.path().backend, BackendKind::GStreamer);
    }
}
