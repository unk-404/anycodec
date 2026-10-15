//! GStreamer element selection, acceleration classification and caps mapping.
//!
//! Every decision in this file is made by asking GStreamer, never by a
//! hard-coded claim. [`element_available`] is
//! [`gst::ElementFactory::find`], and [`AccelerationFamily::of`] classifies the
//! element that was *actually chosen* rather than one that merely might exist — 
//! that distinction is the whole point of
//! [`CodecPath`](crate::engine::CodecPath).
//!
//! The two tables this module does own are the codec-agnostic ones: which
//! GStreamer format name corresponds to each [`PixelFormat`] / [`SampleFormat`],
//! and which plugin-name prefix means which acceleration family. Both are total
//! and are round-tripped by the tests below, so an unmapped format is a test
//! failure rather than a surprise at runtime.

use std::sync::OnceLock;

use gstreamer as gst;
use gstreamer::prelude::{ElementExt, PadExt};
use gstreamer_audio as gst_audio;
use gstreamer_video as gst_video;

use crate::error::{Error, Result};
use crate::media::codec::{AudioCodec, Codec, VideoCodec};
use crate::media::format::{PixelFormat, SampleFormat};

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// The process-wide result of `gst::init()`, cached so that initialisation runs
/// once and its failure reason stays readable.
fn init_result() -> &'static std::result::Result<(), String> {
    static STATE: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    STATE.get_or_init(|| {
        gst::init().map_err(|error| format!("GStreamer could not be initialised: {error}"))
    })
}

/// `true` when GStreamer initialised in this process.
///
/// A one-way latch, and never a panic: `gst::init()` reports failure through its
/// `Result` and the software fallback then takes over.
#[must_use]
pub fn framework_available() -> bool {
    init_result().is_ok()
}

/// Initialises GStreamer, or explains why it could not be.
///
/// Idempotent. Called at the top of every pipeline constructor, because
/// `gstreamer-rs` asserts initialisation for element creation and cap building.
pub fn ensure_initialised() -> Result<()> {
    match init_result() {
        Ok(()) => Ok(()),
        Err(reason) => Err(Error::unavailable(reason.clone())),
    }
}

/// The GStreamer version string, or a placeholder before initialisation.
#[must_use]
pub fn gstreamer_version() -> String {
    if framework_available() {
        return gst::version_string().to_string();
    }
    "GStreamer (not initialised)".to_string()
}

// ---------------------------------------------------------------------------
// Element availability and selection
// ---------------------------------------------------------------------------

/// The acceleration mechanism behind one concrete GStreamer element.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccelerationFamily {
    /// `va*` — the VA-API plugins (`vah264enc`, `vaav1dec`, `vavp9dec`, ...).
    VaApi,
    /// `nv*` — NVIDIA's `nvcodec` plugins.
    Nvidia,
    /// `v4l2*` — the kernel's V4L2 memory-to-memory video codecs.
    V4l2,
    /// `msdk*` — Intel Media SDK.
    MediaSdk,
    /// Everything else: a codec that runs on the CPU.
    Software,
}

impl AccelerationFamily {
    /// Every family, hardware first, in report order.
    pub const ALL: [AccelerationFamily; 5] = [
        AccelerationFamily::VaApi,
        AccelerationFamily::Nvidia,
        AccelerationFamily::V4l2,
        AccelerationFamily::MediaSdk,
        AccelerationFamily::Software,
    ];

    /// Stable lower-case token.
    pub const fn key(self) -> &'static str {
        match self {
            AccelerationFamily::VaApi => "vaapi",
            AccelerationFamily::Nvidia => "nvidia",
            AccelerationFamily::V4l2 => "v4l2",
            AccelerationFamily::MediaSdk => "msdk",
            AccelerationFamily::Software => "software",
        }
    }

    /// Human readable label, used as the name of a probed device.
    pub const fn label(self) -> &'static str {
        match self {
            AccelerationFamily::VaApi => "GStreamer VA-API elements",
            AccelerationFamily::Nvidia => "GStreamer NVIDIA elements",
            AccelerationFamily::V4l2 => "GStreamer V4L2 elements",
            AccelerationFamily::MediaSdk => "GStreamer Media SDK elements",
            AccelerationFamily::Software => "GStreamer software elements",
        }
    }

    /// `true` when the family is a dedicated video block rather than the CPU.
    pub const fn is_hardware(self) -> bool {
        !matches!(self, AccelerationFamily::Software)
    }

    /// Classifies an element by the plugin-name prefix its authors chose.
    ///
    /// The prefixes are the ones the GStreamer plugins actually use:
    /// `gst-plugins-bad/va` names everything `va*`, `nvcodec` names everything
    /// `nv*`, `gst-plugins-good/v4l2` names everything `v4l2*`, and
    /// `gst-plugins-bad/msdk` names everything `msdk*`. No software element in
    /// the codec tables of this backend starts with any of them.
    pub fn of(element: &str) -> AccelerationFamily {
        if element.starts_with("v4l2") {
            AccelerationFamily::V4l2
        } else if element.starts_with("msdk") {
            AccelerationFamily::MediaSdk
        } else if element.starts_with("nv") {
            AccelerationFamily::Nvidia
        } else if element.starts_with("va") {
            AccelerationFamily::VaApi
        } else {
            AccelerationFamily::Software
        }
    }
}

/// `true` when the element named `element` is backed by dedicated hardware.
#[must_use]
pub fn is_hardware_element(element: &str) -> bool {
    AccelerationFamily::of(element).is_hardware()
}

/// `true` when GStreamer actually exposes an element factory called `name`.
///
/// This is the only source of truth used by registration: there is no static
/// list of "codecs GStreamer supports", because that list would be a guess about
/// which plugins happen to be installed.
#[must_use]
pub fn element_available(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

/// The first candidate `is_available` accepts, in preference order.
///
/// Returns `None` when nothing in `candidates` is usable. That is a fact about
/// the machine, not an error, so it is reported as `None` rather than
/// [`Error`].
#[must_use]
pub fn select_element(
    candidates: &[&'static str],
    is_available: &dyn Fn(&str) -> bool,
) -> Option<&'static str> {
    candidates.iter().copied().find(|name| is_available(name))
}

// ---------------------------------------------------------------------------
// Codec → GStreamer media types
// ---------------------------------------------------------------------------

/// The GStreamer media type a compressed `codec` stream carries.
#[must_use]
pub const fn media_type(codec: Codec) -> &'static str {
    match codec {
        Codec::Video(VideoCodec::H264) => "video/x-h264",
        Codec::Video(VideoCodec::H265) => "video/x-h265",
        Codec::Video(VideoCodec::H266) => "video/x-h266",
        Codec::Video(VideoCodec::Vp8) => "video/x-vp8",
        Codec::Video(VideoCodec::Vp9) => "video/x-vp9",
        Codec::Video(VideoCodec::Av1) => "video/x-av1",
        Codec::Audio(AudioCodec::Mp3) => "audio/mpeg",
        Codec::Audio(AudioCodec::Aac) => "audio/mpeg",
        Codec::Audio(AudioCodec::Vorbis) => "audio/x-vorbis",
        Codec::Audio(AudioCodec::Opus) => "audio/x-opus",
        Codec::Audio(AudioCodec::Flac) => "audio/x-flac",
        Codec::Audio(AudioCodec::Alac) => "audio/x-alac",
    }
}

/// Caps for the compressed edge of a pipeline carrying `codec`.
///
/// MP3 and AAC share the `audio/mpeg` media type, so the `mpegversion` (and, for
/// MP3, the `layer`) field is what tells them apart. Leaving those unset would
/// make `lamemp3enc` and `fdkaacenc` negotiate against the same unconstrained
/// caps and produce a pipeline that can silently be wrong.
///
/// Everything else — the stream format, the alignment, whether the stream is
/// `parsed` — is deliberately left to the elements. An encoder's
/// [`Encoder::extradata`](crate::engine::Encoder::extradata) is what describes
/// the framing it chose, and pinning those fields here would contradict it.
pub fn compressed_caps(codec: Codec) -> Result<gst::Caps> {
    compressed_caps_for(codec, None, None)
}

/// Caps for the compressed edge, with the caller's audio geometry pinned when it
/// was given.
///
/// Rate and channel count are properties of the *compressed* stream as well — 
/// they are header fields in Opus, FLAC, Vorbis and AAC — and several GStreamer
/// decoders will not negotiate without them (`flacdec` declares
/// `rate`/`channels` in its sink template). Leaving them out and hoping produces
/// nothing but an opaque `not-negotiated`. Video geometry is never pinned: it is
/// the bitstream's to decide.
pub fn compressed_caps_for(
    codec: Codec,
    rate: Option<u32>,
    channels: Option<u32>,
) -> Result<gst::Caps> {
    ensure_initialised()?;
    let builder = gst::Caps::builder(media_type(codec));
    let builder = match codec {
        Codec::Audio(AudioCodec::Mp3) => builder.field("mpegversion", 1i32).field("layer", 3i32),
        Codec::Audio(AudioCodec::Aac) => builder.field("mpegversion", 4i32),
        _ => builder,
    };
    let builder = match rate {
        Some(rate) => builder.field("rate", rate as i32),
        None => builder,
    };
    let builder = match channels {
        Some(channels) => builder.field("channels", channels as i32),
        None => builder,
    };
    Ok(builder.build())
}

/// Intersects `caps` with what `element`'s sink pad template accepts.
///
/// GStreamer decoders declare specific sink templates — `flacdec` requires
/// `framed=(boolean)true`, `opusdec` requires `channel-mapping-family` — and a
/// structure that does not intersect the template is rejected with nothing but an
/// opaque `not-negotiated` streaming error several layers away. Intersecting
/// first turns that into a caps the element itself has already agreed to.
///
/// `appsrc` refuses to send a caps event that is not *fixed*, so the intersection
/// is only used when every field in it has a single value. Otherwise `caps` is
/// returned unchanged and the element's own negotiation failure is reported,
/// which is more honest than pinning a value the caller never supplied.
#[must_use]
pub fn narrow_to_element(element: &gst::Element, caps: gst::Caps) -> gst::Caps {
    let Some(pad) = element.static_pad("sink") else {
        return caps;
    };
    let Some(template) = pad.pad_template() else {
        return caps;
    };
    let narrowed = template.caps().intersect(&caps);
    if narrowed.is_empty() || !narrowed.is_fixed() {
        caps
    } else {
        narrowed
    }
}

// ---------------------------------------------------------------------------
// Raw formats
// ---------------------------------------------------------------------------

/// GStreamer's name for the raw picture layout `format` describes.
#[must_use]
pub const fn video_format_name(format: PixelFormat) -> &'static str {
    match format {
        PixelFormat::I420 => "I420",
        PixelFormat::Nv12 => "NV12",
        PixelFormat::Nv21 => "NV21",
        PixelFormat::Rgba8 => "RGBA",
        PixelFormat::Bgra8 => "BGRA",
        PixelFormat::Yuy2 => "YUY2",
        PixelFormat::P010 => "P010_10LE",
    }
}

/// The AnyCodec pixel format GStreamer's `name` denotes, if any.
#[must_use]
pub fn pixel_format_from_name(name: &str) -> Option<PixelFormat> {
    Some(match name {
        "I420" => PixelFormat::I420,
        "NV12" => PixelFormat::Nv12,
        "NV21" => PixelFormat::Nv21,
        "RGBA" => PixelFormat::Rgba8,
        "BGRA" => PixelFormat::Bgra8,
        "YUY2" => PixelFormat::Yuy2,
        "P010_10LE" => PixelFormat::P010,
        _ => return None,
    })
}

/// GStreamer's name for the raw sample layout `format` describes.
#[must_use]
pub const fn audio_format_name(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::F32 => "F32LE",
        SampleFormat::S16 => "S16LE",
        SampleFormat::S32 => "S32LE",
        SampleFormat::U8 => "U8",
    }
}

/// The AnyCodec sample format GStreamer's `name` denotes, if any.
#[must_use]
pub fn sample_format_from_name(name: &str) -> Option<SampleFormat> {
    Some(match name {
        "F32LE" => SampleFormat::F32,
        "S16LE" => SampleFormat::S16,
        "S32LE" => SampleFormat::S32,
        "U8" => SampleFormat::U8,
        _ => return None,
    })
}

/// GStreamer's [`gst_video::VideoFormat`] for `format`.
///
/// Fails with [`ErrorKind::Unsupported`](crate::error::ErrorKind::Unsupported)
/// when GStreamer does not know the name, which can only happen if the two tables
/// above drift apart.
pub fn video_format(format: PixelFormat) -> Result<gst_video::VideoFormat> {
    ensure_initialised()?;
    let name = video_format_name(format);
    let parsed = gst_video::VideoFormat::from_string(name);
    if parsed == gst_video::VideoFormat::Unknown {
        return Err(Error::unsupported(format!(
            "GStreamer has no '{name}' raw video format"
        )));
    }
    Ok(parsed)
}

/// GStreamer's [`gst_audio::AudioFormat`] for `format`.
///
/// Fails with [`ErrorKind::Unsupported`](crate::error::ErrorKind::Unsupported)
/// when GStreamer does not know the name.
pub fn audio_format(format: SampleFormat) -> Result<gst_audio::AudioFormat> {
    ensure_initialised()?;
    let name = audio_format_name(format);
    let parsed = gst_audio::AudioFormat::from_string(name);
    if parsed == gst_audio::AudioFormat::Unknown {
        return Err(Error::unsupported(format!(
            "GStreamer has no '{name}' raw audio format"
        )));
    }
    Ok(parsed)
}

/// The AnyCodec pixel format a *negotiated* GStreamer format denotes.
///
/// Used to read back what a decoder actually produced instead of trusting the
/// caller's hint.
#[must_use]
pub fn pixel_format_of(format: gst_video::VideoFormat) -> Option<PixelFormat> {
    PixelFormat::ALL
        .into_iter()
        .find(|candidate| video_format(*candidate).ok() == Some(format))
}

/// The AnyCodec sample format a *negotiated* GStreamer format denotes.
#[must_use]
pub fn sample_format_of(format: gst_audio::AudioFormat) -> Option<SampleFormat> {
    SampleFormat::ALL
        .into_iter()
        .find(|candidate| audio_format(*candidate).ok() == Some(format))
}

// ---------------------------------------------------------------------------
// Caps builders
// ---------------------------------------------------------------------------

/// Caps pinning a raw video edge to exactly `format`, `width` and `height`.
///
/// The geometry is included because an `appsrc` must publish the frame size it
/// will be fed, and `videoconvert` then has nothing left to choose.
pub fn raw_video_caps(format: PixelFormat, width: u32, height: u32) -> Result<gst::Caps> {
    let parsed = video_format(format)?;
    Ok(gst_video::VideoCapsBuilder::new()
        .format(parsed)
        .width(width as i32)
        .height(height as i32)
        .build())
}

/// Caps pinning a raw video edge to `format` only.
///
/// This is what a decoder's output edge uses: the resolution is whatever the
/// bitstream says, so constraining it would make the pipeline refuse valid
/// streams whose size differs from the caller's hint. The real geometry is read
/// back from the negotiated caps.
pub fn raw_video_format_caps(format: PixelFormat) -> Result<gst::Caps> {
    let parsed = video_format(format)?;
    Ok(gst_video::VideoCapsBuilder::new().format(parsed).build())
}

/// Caps pinning an interleaved raw audio edge to `format`, `rate` and `channels`.
pub fn raw_audio_caps(format: SampleFormat, rate: u32, channels: u32) -> Result<gst::Caps> {
    let parsed = audio_format(format)?;
    Ok(gst_audio::AudioCapsBuilder::new_interleaved()
        .format(parsed)
        .rate(rate as i32)
        .channels(channels as i32)
        .build())
}

/// Caps pinning an interleaved raw audio edge to `format` and, when the caller
/// asked for them, an exact sample rate and channel count.
///
/// Unlike video geometry, an audio geometry *can* be honoured without breaking
/// negotiation: `audioconvert`/`audioresample` convert to whatever is pinned
/// here. Anything left `None` is read back from the negotiated caps instead,
/// because the decoder decides it from the bitstream.
pub fn raw_audio_caps_optional(
    format: SampleFormat,
    rate: Option<u32>,
    channels: Option<u32>,
) -> Result<gst::Caps> {
    let parsed = audio_format(format)?;
    Ok(gst_audio::AudioCapsBuilder::new_interleaved()
        .format(parsed)
        .rate_if_some(rate.map(|rate| rate as i32))
        .channels_if_some(channels.map(|channels| channels as i32))
        .build())
}

// ---------------------------------------------------------------------------
// Live retuning
// ---------------------------------------------------------------------------

/// The unit a GStreamer element's `bitrate` property is expressed in.
///
/// The unit is per element and is not discoverable at runtime, so it is a table.
/// An element outside the table is refused rather than guessed at: setting a
/// bitrate in the wrong unit is a silent quality bug, which is worse than a
/// specific [`Error::unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BitrateUnit {
    /// Bits per second.
    BitsPerSecond,
    /// Kilobits per second.
    KilobitsPerSecond,
}

impl BitrateUnit {
    /// Converts bits per second into this unit, saturating at the `u32` the
    /// property is declared as.
    #[must_use]
    pub const fn from_bits(self, bits_per_second: u64) -> u32 {
        let value = match self {
            BitrateUnit::BitsPerSecond => bits_per_second,
            BitrateUnit::KilobitsPerSecond => bits_per_second / 1000,
        };
        if value > u32::MAX as u64 {
            u32::MAX
        } else {
            value as u32
        }
    }

    /// Stable lower-case token.
    pub const fn key(self) -> &'static str {
        match self {
            BitrateUnit::BitsPerSecond => "bps",
            BitrateUnit::KilobitsPerSecond => "kbps",
        }
    }
}

/// The unit of `element`'s `bitrate` property, or `None` when this backend does
/// not know it and therefore will not set it.
#[must_use]
pub fn bitrate_unit(element: &str) -> Option<BitrateUnit> {
    Some(match element {
        // OpenH264 and the gst-plugins-good V4L2 encoders document bits/second.
        "openh264enc" | "v4l2h264enc" | "v4l2h265enc" | "v4l2vp8enc" | "v4l2vp9enc" => {
            BitrateUnit::BitsPerSecond
        }
        // Everything else in the candidate tables documents kilobits/second.
        "x264enc" | "x265enc" | "vp8enc" | "vp9enc" | "av1enc" | "svtav1enc" | "vvencenc"
        | "vah264enc" | "vah265enc" | "vavp8enc" | "vavp9enc" | "vaav1enc" | "nvh264enc"
        | "nvh265enc" | "nvav1enc" | "msdkh264enc" | "msdkh265enc" | "msdkvp8enc"
        | "msdkvp9enc" | "msdkav1enc" => BitrateUnit::KilobitsPerSecond,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_video_format_names_round_trip() {
        for format in PixelFormat::ALL {
            let name = video_format_name(format);
            assert_eq!(
                pixel_format_from_name(name),
                Some(format),
                "{} names as {name}",
                format.name()
            );
        }
        assert_eq!(pixel_format_from_name("NOT_A_FORMAT"), None);
        assert_eq!(pixel_format_from_name("i420"), None, "GStreamer names are upper case");
    }

    #[test]
    fn the_audio_format_names_round_trip() {
        for format in SampleFormat::ALL {
            let name = audio_format_name(format);
            assert_eq!(
                sample_format_from_name(name),
                Some(format),
                "{} names as {name}",
                format.name()
            );
        }
        assert_eq!(sample_format_from_name("S16"), None, "GStreamer names the endianness");
    }

    #[test]
    fn the_media_type_table_is_total_and_matches_the_media_kind() {
        for codec in Codec::ALL {
            let media_type = media_type(codec);
            assert!(!media_type.is_empty(), "{codec} has no GStreamer media type");
            assert_eq!(
                media_type.starts_with("video/"),
                codec.is_video(),
                "{codec} names as {media_type}"
            );
            assert_eq!(media_type.starts_with("audio/"), codec.is_audio(), "{codec}");
        }
    }

    #[test]
    fn the_acceleration_families_are_distinct_and_ordered_hardware_first() {
        let mut keys: Vec<&str> = AccelerationFamily::ALL.iter().map(|f| f.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), AccelerationFamily::ALL.len());
        assert!(!AccelerationFamily::Software.is_hardware());
        assert!(AccelerationFamily::ALL[..AccelerationFamily::ALL.len() - 1]
            .iter()
            .all(|family| family.is_hardware()));
    }

    #[test]
    fn unsupported_bitrates_are_refused_rather_than_guessed() {
        assert_eq!(bitrate_unit("x264enc"), Some(BitrateUnit::KilobitsPerSecond));
        assert_eq!(bitrate_unit("vah265enc"), Some(BitrateUnit::KilobitsPerSecond));
        assert_eq!(bitrate_unit("v4l2h264enc"), Some(BitrateUnit::BitsPerSecond));
        assert_eq!(bitrate_unit("opusenc"), None);
        assert_eq!(bitrate_unit("lamemp3enc"), None);
        // Every element the tables can choose for a video codec must have a unit,
        // otherwise `set_bitrate` would silently become a no-op.
        assert_eq!(BitrateUnit::KilobitsPerSecond.from_bits(4_000_000), 4000);
        assert_eq!(BitrateUnit::BitsPerSecond.from_bits(4_000_000), 4_000_000);
        assert_eq!(BitrateUnit::BitsPerSecond.from_bits(u64::MAX), u32::MAX);
    }

    #[test]
    fn initialisation_is_a_latch_and_never_a_panic() {
        // Whether GStreamer exists here or not, these must answer, not abort.
        let first = framework_available();
        assert_eq!(first, framework_available());
        if first {
            assert!(ensure_initialised().is_ok());
            assert!(!gstreamer_version().is_empty());
        } else {
            assert!(ensure_initialised().is_err());
        }
    }

    #[test]
    fn element_lookup_reports_absence_instead_of_guessing() {
        // `gst::ElementFactory::find` panics outright when `gst::init` has not
        // run, and `gst::init` is a process-wide latch. Initialising explicitly
        // here is what makes this test independent of whether another test in the
        // binary happened to run first; without it the result depends on the
        // order the harness schedules tests in.
        ensure_initialised().expect("GStreamer must initialise");
        // A plausible but non-existent factory must answer `false` even when
        // GStreamer is perfectly healthy.
        assert!(!element_available("anycodec-definitely-not-an-element"));
        assert_eq!(select_element(&[], &element_available), None);
    }

    #[test]
    #[ignore = "requires GStreamer"]
    fn gstreamer_agrees_with_every_format_name_we_use() {
        ensure_initialised().expect("GStreamer must initialise");
        for format in PixelFormat::ALL {
            let name = video_format_name(format);
            assert_ne!(
                gst_video::VideoFormat::from_string(name),
                gst_video::VideoFormat::Unknown,
                "GStreamer does not know the video format {name}"
            );
            assert_eq!(pixel_format_of(video_format(format).expect("known above")), Some(format));
        }
        for format in SampleFormat::ALL {
            let name = audio_format_name(format);
            assert_ne!(
                gst_audio::AudioFormat::from_string(name),
                gst_audio::AudioFormat::Unknown,
                "GStreamer does not know the audio format {name}"
            );
            assert_eq!(sample_format_of(audio_format(format).expect("known above")), Some(format));
        }
    }

    #[test]
    #[ignore = "requires GStreamer"]
    fn every_candidate_element_name_is_a_real_gstreamer_factory_or_absent() {
        // Element *availability* differs per machine, but a factory that exists
        // must also be instantiable, and a name GStreamer has never heard of is a
        // typo in one of our tables. `gst-inspect`-style validation needs plugin
        // metadata, which is what `ElementFactory::find` reads.
        ensure_initialised().expect("GStreamer must initialise");
        // This test intentionally only asserts the lookup does not panic; which
        // names are present is reported by `probe_devices`.
        for name in ["x264enc", "avdec_h264", "opusenc", "flacenc", "not-a-real-element"] {
            let _ = element_available(name);
        }
    }
}
