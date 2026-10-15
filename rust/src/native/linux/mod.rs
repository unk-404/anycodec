//! Linux platform backend: GStreamer.
//!
//! GStreamer is the one media framework every Linux desktop already ships, and
//! it is the only one that describes *itself*: an element is either present in
//! the registry or it is not, and its name says which acceleration path it uses
//! (`vah264enc` is VA-API, `x264enc` is the CPU). AnyCodec therefore never
//! claims a codec GStreamer cannot actually produce on this machine.
//!
//! ## Files
//!
//! | File | Responsibility |
//! |------|----------------|
//! | `mod.rs` (this file) | The required surface, the codec→ element tables, registration and device probing. |
//! | [`elements`] | Element selection, acceleration classification, caps mapping. |
//! | [`pipeline`] | The [`Encoder`](crate::engine::Encoder) / [`Decoder`](crate::engine::Decoder) implementations over a `gst::Pipeline`. |
//!
//! ## What Linux can and cannot do
//!
//! | Codec | Candidates, best first |
//! |-------|-----------------------|
//! | H.264 | VA-API → NVENC → V4L2 → Media SDK → `openh264enc` → `x264enc` |
//! | H.265 | VA-API → NVENC → V4L2 → Media SDK → `x265enc` |
//! | H.266 | `vvencenc` / `vvdec` only; no shipping Linux encoder is hardware accelerated |
//! | VP8, VP9 | VA-API → V4L2 → Media SDK → `vp8enc`/`vp9enc` |
//! | AV1 | VA-API → NV → Media SDK → `av1enc`/`svtav1enc`, `avdec_av1` |
//! | MP3, AAC | `lamemp3enc`, `fdkaacenc`/`avenc_aac`/`faac` |
//! | Vorbis, Opus, FLAC, ALAC | `vorbisenc`, `opusenc`, `flacenc`, `avenc_alac` |
//!
//! ## Honesty
//!
//! Every table below is a list of *candidates*. Which one is used is decided by
//! [`gst::ElementFactory::find`] at registration time, and
//! [`crate::engine::CodecPath::hardware_accelerated`] is `true` only when the
//! element that was actually chosen belongs to a hardware family. A machine with
//! the VA-API plugin installed but no usable render node still selects a
//! hardware element here; the negotiation failure then surfaces as a concrete
//! [`Error::backend`] from the live pipeline rather than as a silent fallback.

pub mod elements;
pub mod pipeline;

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, CodecKind, HwCodecSupport, HwDeviceInfo, Registry};
use crate::error::Result;
use crate::media::codec::{AudioCodec, Codec, VideoCodec};
use crate::media::config::{DecoderConfig, EncoderConfig};

// ---------------------------------------------------------------------------
// Required surface
// ---------------------------------------------------------------------------

/// Registers every encoder/decoder GStreamer can actually provide on this machine.
///
/// Registers nothing at all when GStreamer cannot be initialised, so the
/// capability report stays truthful instead of advertising codecs that would
/// fail on the first frame.
pub fn register(registry: &mut Registry) {
    if !available() {
        log::debug!("anycodec: GStreamer is not usable in this process; nothing registered");
        return;
    }
    register_with(registry, &elements::element_available);
}

/// `true` when GStreamer initialised successfully in this process.
///
/// Never panics: a host whose GStreamer fails to initialise reports `false` and
/// the software fallback takes over.
#[must_use]
pub fn available() -> bool {
    elements::framework_available()
}

/// Hardware engines, probed on demand (never during [`register`]).
///
/// One record per acceleration family that is *actually reachable*: a family is
/// reported only when at least one of its elements won the selection for some
/// codec and direction. The list is honest and therefore **possibly empty** — a
/// GStreamer installation with no plugins reports nothing, and one whose only
/// video encoders are software reports a single software record.
///
/// Resolution and bit-depth limits are reported as `0` ("unknown") because the
/// GStreamer element registry does not publish them.
///
/// Blocking: `gst::init` has already loaded the element registry, but enumerating
/// it consults every plugin on disk.
#[must_use]
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    if !available() {
        return Vec::new();
    }
    probe_devices_with(&elements::element_available)
}

// ---------------------------------------------------------------------------
// Codec → element tables
// ---------------------------------------------------------------------------

/// Encoder element candidates for `codec`, best first.
///
/// Hardware families come first (`va*`, `nv*`, `v4l2*`, `msdk*`), then the
/// software plugins. The order is a *preference*, not a promise: whether an entry
/// is usable is decided by [`gst::ElementFactory::find`].
fn encoder_candidates(codec: Codec) -> &'static [&'static str] {
    match codec {
        Codec::Video(VideoCodec::H264) => {
            &["vah264enc", "nvh264enc", "v4l2h264enc", "msdkh264enc", "openh264enc", "x264enc"]
        }
        Codec::Video(VideoCodec::H265) => {
            &["vah265enc", "nvh265enc", "v4l2h265enc", "msdkh265enc", "x265enc"]
        }
        // No shipping Linux stack encodes VVC in hardware, so `vvencenc` is the
        // only candidate and `CodecPath` correctly reports software.
        Codec::Video(VideoCodec::H266) => &["vvencenc"],
        Codec::Video(VideoCodec::Vp8) => &["vavp8enc", "v4l2vp8enc", "msdkvp8enc", "vp8enc"],
        Codec::Video(VideoCodec::Vp9) => &["vavp9enc", "v4l2vp9enc", "msdkvp9enc", "vp9enc"],
        Codec::Video(VideoCodec::Av1) => {
            &["vaav1enc", "nvav1enc", "msdkav1enc", "av1enc", "svtav1enc"]
        }
        Codec::Audio(AudioCodec::Mp3) => &["lamemp3enc", "avenc_mp3", "twolamemp3enc"],
        Codec::Audio(AudioCodec::Aac) => &["fdkaacenc", "avenc_aac", "faac"],
        Codec::Audio(AudioCodec::Vorbis) => &["vorbisenc"],
        Codec::Audio(AudioCodec::Opus) => &["opusenc"],
        Codec::Audio(AudioCodec::Flac) => &["flacenc"],
        Codec::Audio(AudioCodec::Alac) => &["avenc_alac"],
    }
}

/// Decoder element candidates for `codec`, best first.
fn decoder_candidates(codec: Codec) -> &'static [&'static str] {
    match codec {
        Codec::Video(VideoCodec::H264) => {
            &["vah264dec", "nvh264dec", "v4l2h264dec", "msdkh264dec", "avdec_h264", "openh264dec"]
        }
        Codec::Video(VideoCodec::H265) => {
            &["vah265dec", "nvh265dec", "v4l2h265dec", "msdkh265dec", "avdec_h265"]
        }
        Codec::Video(VideoCodec::H266) => &["vvdec"],
        Codec::Video(VideoCodec::Vp8) => {
            &["vavp8dec", "nvvp8dec", "v4l2vp8dec", "msdkvp8dec", "avdec_vp8", "vp8dec"]
        }
        Codec::Video(VideoCodec::Vp9) => {
            &["vavp9dec", "nvvp9dec", "v4l2vp9dec", "msdkvp9dec", "avdec_vp9", "vp9dec"]
        }
        Codec::Video(VideoCodec::Av1) => {
            &["vaav1dec", "nvav1dec", "v4l2av1dec", "msdkav1dec", "avdec_av1", "dav1ddec", "av1dec"]
        }
        Codec::Audio(AudioCodec::Mp3) => &["mpg123audiodec", "avdec_mp3", "mad"],
        Codec::Audio(AudioCodec::Aac) => &["avdec_aac", "faad"],
        Codec::Audio(AudioCodec::Vorbis) => &["vorbisdec", "avdec_vorbis"],
        Codec::Audio(AudioCodec::Opus) => &["opusdec", "avdec_opus"],
        Codec::Audio(AudioCodec::Flac) => &["flacdec", "avdec_flac"],
        Codec::Audio(AudioCodec::Alac) => &["alacdec", "avdec_alac"],
    }
}

/// The bitstream parser to place between `appsrc` and the decoder.
///
/// Most GStreamer decoders read their global headers out of the *caps* and refuse
/// to negotiate without them; the parser is what reads those headers out of the
/// byte stream and republishes them. GStreamer names the fix itself — `flacdec`
/// logs "no 'streamheader' field in input caps, try adding a flacparse element
/// upstream".
///
/// `None` for codecs whose access units are self-describing enough that no parser
/// is shipped for them.
fn parser_element(codec: Codec) -> Option<&'static str> {
    match codec {
        Codec::Video(VideoCodec::H264) => Some("h264parse"),
        Codec::Video(VideoCodec::H265) => Some("h265parse"),
        Codec::Video(VideoCodec::Vp8) => Some("vp8parse"),
        Codec::Video(VideoCodec::Vp9) => Some("vp9parse"),
        Codec::Video(VideoCodec::Av1) => Some("av1parse"),
        // No VVC parser is shipped yet; `vvdec` takes complete access units.
        Codec::Video(VideoCodec::H266) => None,
        Codec::Audio(AudioCodec::Mp3) => Some("mpegaudioparse"),
        Codec::Audio(AudioCodec::Aac) => Some("aacparse"),
        Codec::Audio(AudioCodec::Vorbis) => Some("vorbisparse"),
        Codec::Audio(AudioCodec::Opus) => Some("opusparse"),
        Codec::Audio(AudioCodec::Flac) => Some("flacparse"),
        Codec::Audio(AudioCodec::Alac) => Some("alacparse"),
    }
}

/// `true` when `codec`'s packets can be framed two different ways, so that the
/// caller's `annex_b` declaration decides whether a parser is *required* rather
/// than merely helpful.
///
/// Only H.264 and H.265 have this ambiguity: the same NAL units can be delimited
/// by start codes or by lengths. VP8/VP9/AV1/MP3/Opus/FLAC access units are the
/// same bytes either way.
fn has_framing_ambiguity(codec: Codec) -> bool {
    matches!(codec, Codec::Video(VideoCodec::H264) | Codec::Video(VideoCodec::H265))
}

/// The [`CodecPath`] a live session reports.
///
/// The hardware flag comes from the element that was *actually chosen* — never
/// from what the table hopes is installed — and the detail string names every
/// element in the chain, including `parser` when one was inserted.
pub(crate) fn codec_path(
    codec: Codec,
    kind: CodecKind,
    element: &str,
    parser: Option<&str>,
) -> CodecPath {
    let parser = parser.map(|name| format!("{name} ! ")).unwrap_or_default();
    let chain = match (codec.is_video(), kind) {
        (true, CodecKind::Encoder) => format!("appsrc ! videoconvert ! {element} ! appsink"),
        (false, CodecKind::Encoder) => {
            format!("appsrc ! audioconvert ! audioresample ! {element} ! appsink")
        }
        (true, CodecKind::Decoder) => {
            format!("appsrc ! {parser}{element} ! videoconvert ! appsink")
        }
        (false, CodecKind::Decoder) => format!("appsrc ! {parser}{element} ! audioconvert ! appsink"),
    };
    CodecPath::new(
        BackendKind::GStreamer,
        elements::is_hardware_element(element),
        format!("gst: {chain}"),
    )
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

/// The probe every registration uses: is there an element we could build?
///
/// Cheap and side-effect free after the first call — it reads the element
/// registry `gst::init` already loaded and never instantiates anything.
fn probe_codec(codec: Codec, kind: CodecKind) -> BackendAvailability {
    if let Err(error) = elements::ensure_initialised() {
        return BackendAvailability::Unavailable(error.message().to_string());
    }
    let candidates = match kind {
        CodecKind::Encoder => encoder_candidates(codec),
        CodecKind::Decoder => decoder_candidates(codec),
    };
    match elements::select_element(candidates, &elements::element_available) {
        Some(_) => BackendAvailability::Available,
        None => BackendAvailability::Unavailable(format!(
            "GStreamer exposes no {} {} element; tried {}",
            codec.name(),
            kind.name(),
            candidates.join(", ")
        )),
    }
}

/// Registers exactly the codecs for which `is_available` accepts a candidate.
///
/// Split out from [`register`] so that tests can drive registration with a
/// synthetic element set instead of depending on what this machine has
/// installed.
fn register_with(registry: &mut Registry, is_available: &dyn Fn(&str) -> bool) {
    for codec in Codec::ALL {
        if elements::select_element(encoder_candidates(codec), is_available).is_some() {
            let probe = move |codec: Codec| probe_codec(codec, CodecKind::Encoder);
            let factory = move |config: &EncoderConfig| -> Result<Option<Box<dyn Encoder>>> {
                Ok(Some(Box::new(pipeline::GstEncoder::new(config)?) as Box<dyn Encoder>))
            };
            // `PLATFORM_FRAMEWORK`, not `DYNAMIC_FRAMEWORK`: `gstreamer-rs` links
            // `libgstreamer-1.0` at build time, and GStreamer is a platform
            // framework that may or may not use hardware — the element choice,
            // reported through `CodecPath`, is what decides.
            registry.register_encoder(
                codec,
                BackendKind::GStreamer,
                super::priority::PLATFORM_FRAMEWORK,
                factory,
                probe,
            );
        }
        if elements::select_element(decoder_candidates(codec), is_available).is_some() {
            let probe = move |codec: Codec| probe_codec(codec, CodecKind::Decoder);
            let factory = move |config: &DecoderConfig| -> Result<Option<Box<dyn Decoder>>> {
                Ok(Some(Box::new(pipeline::GstDecoder::new(config)?) as Box<dyn Decoder>))
            };
            registry.register_decoder(
                codec,
                BackendKind::GStreamer,
                super::priority::PLATFORM_FRAMEWORK,
                factory,
                probe,
            );
        }
    }
}

/// Reports one device per acceleration family `is_available` can actually reach.
fn probe_devices_with(is_available: &dyn Fn(&str) -> bool) -> Vec<HwDeviceInfo> {
    let api_version = elements::gstreamer_version();
    let mut devices = Vec::new();
    for family in elements::AccelerationFamily::ALL {
        let mut codecs: Vec<HwCodecSupport> = Vec::new();
        for codec in Codec::ALL {
            let decode = elements::select_element(decoder_candidates(codec), is_available)
                .is_some_and(|element| elements::AccelerationFamily::of(element) == family);
            let encode = elements::select_element(encoder_candidates(codec), is_available)
                .is_some_and(|element| elements::AccelerationFamily::of(element) == family);
            if !decode && !encode {
                continue;
            }
            codecs.push(HwCodecSupport {
                codec,
                decode,
                encode,
                max_width: 0,
                max_height: 0,
                max_bit_depth: 0,
            });
        }
        if codecs.is_empty() {
            continue;
        }
        let mut device =
            HwDeviceInfo::new(BackendKind::GStreamer, family.label(), api_version.as_str());
        // `driver_version` stays empty: the VA-API, V4L2, NVENC and Media SDK
        // driver versions live behind elements this backend deliberately does not
        // instantiate while probing.
        device.codecs = codecs;
        devices.push(device);
    }
    devices
}

/// Keeps the "GStreamer is native, not software" classification under test.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::backend::Implementation;

    /// A synthetic "these elements exist" set, so nothing below depends on what
    /// this machine happens to have installed.
    fn only(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |name: &str| names.contains(&name)
    }

    /// Per codec: `(encode, decode)` as registered by the GStreamer backend.
    fn gstreamer_codecs(registry: &Registry) -> Vec<(Codec, bool, bool)> {
        registry
            .capabilities()
            .into_iter()
            .map(|report| {
                let encode = report
                    .encoders
                    .iter()
                    .any(|info| info.backend == BackendKind::GStreamer);
                let decode = report
                    .decoders
                    .iter()
                    .any(|info| info.backend == BackendKind::GStreamer);
                (report.codec, encode, decode)
            })
            .collect()
    }

    #[test]
    fn the_encoder_table_is_total_for_every_codec() {
        assert_eq!(Codec::ALL.len(), 12, "AnyCodec routes exactly 12 codecs");
        for codec in Codec::ALL {
            let candidates = encoder_candidates(codec);
            assert!(!candidates.is_empty(), "{codec} has no encoder candidate");
            for name in candidates {
                assert!(
                    !name.is_empty() && name.is_ascii(),
                    "{codec} has a malformed candidate {name:?}"
                );
            }
        }
    }

    #[test]
    fn the_decoder_table_is_total_for_every_codec() {
        for codec in Codec::ALL {
            let candidates = decoder_candidates(codec);
            assert!(!candidates.is_empty(), "{codec} has no decoder candidate");
            for name in candidates {
                assert!(
                    !name.is_empty() && name.is_ascii(),
                    "{codec} has a malformed candidate {name:?}"
                );
            }
        }
    }

    #[test]
    fn no_candidate_is_listed_twice_for_one_codec() {
        for codec in Codec::ALL {
            for candidates in [encoder_candidates(codec), decoder_candidates(codec)] {
                for (index, name) in candidates.iter().enumerate() {
                    assert!(
                        !candidates[index + 1..].contains(name),
                        "{codec} lists {name} twice"
                    );
                }
            }
        }
    }

    #[test]
    fn every_audio_candidate_is_software() {
        // Audio has no hardware acceleration path on Linux, so the element chosen
        // for every audio codec must classify as software.
        for codec in Codec::ALL.iter().filter(|codec| codec.is_audio()) {
            for name in encoder_candidates(*codec).iter().chain(decoder_candidates(*codec)) {
                assert!(
                    !elements::is_hardware_element(name),
                    "{codec} candidate {name} would report hardware acceleration"
                );
            }
        }
    }

    #[test]
    fn the_hardware_heuristic_follows_the_plugin_name_prefixes() {
        for (element, hardware) in [
            ("vah264enc", true),
            ("vah265dec", true),
            ("vaav1dec", true),
            ("vavp9dec", true),
            ("nvh264enc", true),
            ("nvh265dec", true),
            ("v4l2h264enc", true),
            ("v4l2vp9dec", true),
            ("msdkh264enc", true),
            ("msdkav1dec", true),
            ("x264enc", false),
            ("openh264enc", false),
            ("x265enc", false),
            ("avdec_h264", false),
            ("avdec_vp9", false),
            ("vvdec", false),
            ("vvencenc", false),
            ("vp8enc", false),
            ("vp9enc", false),
            ("av1enc", false),
            ("svtav1enc", false),
            ("lamemp3enc", false),
            ("avdec_aac", false),
            ("faac", false),
            ("fdkaacenc", false),
            ("opusenc", false),
            ("vorbisenc", false),
            ("flacenc", false),
            ("avenc_alac", false),
            ("mpg123audiodec", false),
            ("h264parse", false),
        ] {
            assert_eq!(
                elements::is_hardware_element(element),
                hardware,
                "{element} must classify as {}",
                if hardware { "hardware" } else { "software" }
            );
        }
    }

    #[test]
    fn gstreamer_is_a_native_platform_framework() {
        assert_eq!(BackendKind::GStreamer.implementation(), Implementation::Native);
        assert!(BackendKind::GStreamer.is_platform());
        assert!(BackendKind::GStreamer.is_hardware_capable());
    }

    #[test]
    fn selection_takes_the_first_available_candidate_in_preference_order() {
        let candidates: &[&'static str] = &["vah264enc", "openh264enc", "x264enc"];
        assert_eq!(
            elements::select_element(candidates, &only(&["vah264enc", "openh264enc", "x264enc"])),
            Some("vah264enc")
        );
        assert_eq!(
            elements::select_element(candidates, &only(&["openh264enc", "x264enc"])),
            Some("openh264enc")
        );
        assert_eq!(
            elements::select_element(candidates, &only(&["x264enc"])),
            Some("x264enc")
        );
        assert_eq!(elements::select_element(candidates, &only(&[])), None);
    }

    #[test]
    fn the_chosen_element_decides_hardware_not_the_table() {
        // The same codec reports hardware or software purely on which candidate
        // won, which is the property `CodecPath` exists to guarantee.
        let candidates = encoder_candidates(Codec::Video(VideoCodec::H264));
        let chosen = elements::select_element(candidates, &only(&["openh264enc", "x264enc"]))
            .expect("the software candidates are present");
        assert_eq!(chosen, "openh264enc");
        assert!(!elements::is_hardware_element(chosen));

        let chosen = elements::select_element(candidates, &only(&["vah264enc", "x264enc"]))
            .expect("the VA-API candidate is present");
        assert_eq!(chosen, "vah264enc");
        assert!(elements::is_hardware_element(chosen));
    }

    #[test]
    fn registration_covers_exactly_the_codecs_with_a_candidate_element() {
        let mut registry = Registry::new();
        register_with(
            &mut registry,
            &only(&["vah264enc", "avdec_h264", "x265enc", "opusenc", "opusdec", "flacdec"]),
        );

        // Hand-written expectations for exactly that synthetic element set: the
        // H.264 encoder and decoder, the H.265 encoder, Opus both ways, and the
        // FLAC decoder. Nothing else may appear.
        let expected = [
            (Codec::Video(VideoCodec::H264), true, true),
            (Codec::Video(VideoCodec::H265), true, false),
            (Codec::Video(VideoCodec::H266), false, false),
            (Codec::Video(VideoCodec::Vp8), false, false),
            (Codec::Video(VideoCodec::Vp9), false, false),
            (Codec::Video(VideoCodec::Av1), false, false),
            (Codec::Audio(AudioCodec::Mp3), false, false),
            (Codec::Audio(AudioCodec::Aac), false, false),
            (Codec::Audio(AudioCodec::Vorbis), false, false),
            (Codec::Audio(AudioCodec::Opus), true, true),
            (Codec::Audio(AudioCodec::Flac), false, true),
            (Codec::Audio(AudioCodec::Alac), false, false),
        ];
        assert_eq!(gstreamer_codecs(&registry), expected.to_vec());
    }

    #[test]
    fn a_software_only_installation_still_registers_the_software_codecs() {
        // The pessimistic case: no VA-API, no NVENC, no V4L2, no Media SDK.
        let mut registry = Registry::new();
        register_with(
            &mut registry,
            &only(&["x264enc", "x265enc", "avdec_h264", "avdec_h265", "opusenc", "opusdec"]),
        );
        let registered = gstreamer_codecs(&registry);
        assert!(registered
            .iter()
            .any(|(codec, encode, decode)| *codec == Codec::Video(VideoCodec::H264)
                && *encode
                && *decode));
        // ... but nothing claims hardware: no hardware-capable family was reachable.
        let devices = probe_devices_with(&only(&["x264enc", "avdec_h264"]));
        assert!(devices.iter().all(|device| device.name.contains("software")));
    }

    #[test]
    fn an_empty_element_set_registers_nothing() {
        let mut registry = Registry::new();
        register_with(&mut registry, &only(&[]));
        assert!(registry.capabilities().iter().all(|report| {
            report
                .encoders
                .iter()
                .chain(report.decoders.iter())
                .all(|info| info.backend != BackendKind::GStreamer)
        }));
    }

    #[test]
    fn registration_never_panics_without_gstreamer() {
        // On a host without GStreamer this is a no-op; with it, it registers.
        // Either way the report must stay complete and well formed.
        let mut registry = Registry::new();
        register(&mut registry);
        assert_eq!(registry.capabilities().len(), Codec::ALL.len());
    }

    #[test]
    fn the_probe_explains_itself_specifically() {
        for codec in Codec::ALL {
            for kind in CodecKind::ALL {
                if let BackendAvailability::Unavailable(reason) = probe_codec(codec, kind) {
                    assert!(reason.contains(codec.name()), "{codec} {kind:?}: {reason}");
                    assert!(reason.len() > 30, "{codec} {kind:?}: {reason}");
                }
            }
        }
    }

    #[test]
    fn probing_devices_never_panics() {
        let devices = probe_devices();
        assert!(devices.len() <= elements::AccelerationFamily::ALL.len());
    }

    #[test]
    fn a_probed_device_only_claims_the_families_that_won_a_selection() {
        let devices =
            probe_devices_with(&only(&["vah264enc", "avdec_h264", "x264enc", "flacenc"]));

        let va = devices
            .iter()
            .find(|device| device.name.contains("VA-API"))
            .expect("a VA-API encode must produce a VA-API record");
        let h264 = va
            .support(Codec::Video(VideoCodec::H264))
            .expect("h264 must be listed under VA-API");
        // `vah264enc` won the encode, but the decode went to `avdec_h264`, so the
        // VA-API record must not claim the decoder.
        assert!(h264.encode);
        assert!(!h264.decode);

        let software = devices
            .iter()
            .find(|device| device.name.contains("software"))
            .expect("software elements won some selections");
        let h264 = software
            .support(Codec::Video(VideoCodec::H264))
            .expect("h264 decode went to avdec_h264");
        assert!(h264.decode);
        assert!(!h264.encode, "the encode went to VA-API, not to x264enc");
        assert!(software
            .support(Codec::Audio(AudioCodec::Flac))
            .is_some_and(|support| support.encode && !support.decode));

        for device in &devices {
            assert_eq!(device.backend, BackendKind::GStreamer);
            assert!(!device.codecs.is_empty());
        }
    }

    #[test]
    fn a_device_record_never_lists_a_codec_without_a_direction_or_a_guessed_limit() {
        let devices = probe_devices_with(&only(&["x264enc", "opusdec", "opusenc"]));
        assert!(!devices.is_empty());
        for device in &devices {
            for support in &device.codecs {
                assert!(
                    support.decode || support.encode,
                    "{} is listed with no direction",
                    support.codec
                );
                assert_eq!(support.max_width, 0, "GStreamer publishes no width limit");
                assert_eq!(support.max_height, 0);
                assert_eq!(support.max_bit_depth, 0);
            }
            assert!(!device.api_version.is_empty());
        }
    }

    #[test]
    fn the_codec_path_names_gstreamer_and_the_chosen_element() {
        let path =
            codec_path(Codec::Video(VideoCodec::H264), CodecKind::Encoder, "vah264enc", None);
        assert_eq!(path.backend, BackendKind::GStreamer);
        assert_eq!(path.backend.name(), "gstreamer");
        assert!(path.hardware_accelerated);
        assert!(path.is_native());
        assert!(!path.is_software());
        assert_eq!(path.implementation, Implementation::Native);
        assert_eq!(path.detail, "gst: appsrc ! videoconvert ! vah264enc ! appsink");
        assert!(path.fallback_reason.is_none(), "the registry owns fallback_reason");

        let software =
            codec_path(Codec::Video(VideoCodec::H264), CodecKind::Decoder, "avdec_h264", None);
        assert!(!software.hardware_accelerated, "avdec_h264 is not a hardware element");
        assert_eq!(software.detail, "gst: appsrc ! avdec_h264 ! videoconvert ! appsink");
    }

    #[test]
    fn the_codec_path_names_every_element_including_the_parser() {
        let path = codec_path(
            Codec::Video(VideoCodec::H265),
            CodecKind::Decoder,
            "vah265dec",
            Some("h265parse"),
        );
        assert_eq!(path.detail, "gst: appsrc ! h265parse ! vah265dec ! videoconvert ! appsink");
        assert!(path.hardware_accelerated);
    }

    #[test]
    fn the_codec_path_audio_shapes_match_the_pipelines() {
        assert_eq!(
            codec_path(Codec::Audio(AudioCodec::Opus), CodecKind::Encoder, "opusenc", None).detail,
            "gst: appsrc ! audioconvert ! audioresample ! opusenc ! appsink"
        );
        assert_eq!(
            codec_path(Codec::Audio(AudioCodec::Opus), CodecKind::Decoder, "opusdec", None).detail,
            "gst: appsrc ! opusdec ! audioconvert ! appsink"
        );
    }

    #[test]
    fn every_codec_but_vvc_has_a_parser_candidate() {
        assert_eq!(parser_element(Codec::Video(VideoCodec::H264)), Some("h264parse"));
        assert_eq!(parser_element(Codec::Video(VideoCodec::H265)), Some("h265parse"));
        assert_eq!(parser_element(Codec::Audio(AudioCodec::Flac)), Some("flacparse"));
        assert_eq!(parser_element(Codec::Audio(AudioCodec::Mp3)), Some("mpegaudioparse"));
        for codec in Codec::ALL {
            if codec == Codec::Video(VideoCodec::H266) {
                assert_eq!(parser_element(codec), None, "no VVC parser is shipped");
                continue;
            }
            let name = parser_element(codec).unwrap_or_else(|| panic!("{codec} has no parser"));
            assert!(!name.is_empty() && name.ends_with("parse"), "{codec} names {name}");
        }
    }

    #[test]
    fn only_h264_and_h265_have_a_framing_ambiguity() {
        for codec in Codec::ALL {
            assert_eq!(
                has_framing_ambiguity(codec),
                matches!(codec, Codec::Video(VideoCodec::H264) | Codec::Video(VideoCodec::H265)),
                "{codec}"
            );
        }
    }
}
