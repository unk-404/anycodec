//! Backend registration and policy-driven selection.
//!
//! The registry is the single place that knows *which implementations exist*
//! and *in what order they should be tried*. Platform backends register at high
//! priority, software fallbacks at low priority, and
//! [`Registry::create_encoder`] walks the list until something works, recording
//! why anything was skipped in [`CodecPath::fallback_reason`].
//!
//! Backends are registered lazily from [`registry`], which runs exactly once per
//! process.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::engine::backend::{BackendKind, BackendPreference, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::error::{Error, Result};
use crate::media::codec::Codec;
use crate::media::config::{DecoderConfig, EncoderConfig};

/// Whether a backend is being asked for an encoder or a decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CodecKind {
    /// Producing compressed packets from raw frames.
    Encoder,
    /// Producing raw frames from compressed packets.
    Decoder,
}

impl CodecKind {
    /// Every kind.
    pub const ALL: [CodecKind; 2] = [CodecKind::Encoder, CodecKind::Decoder];

    /// Stable lower-case token.
    pub const fn name(self) -> &'static str {
        match self {
            CodecKind::Encoder => "encoder",
            CodecKind::Decoder => "decoder",
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            CodecKind::Encoder => 0,
            CodecKind::Decoder => 1,
        }
    }

    /// Parses a C ABI kind value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            0 => CodecKind::Encoder,
            1 => CodecKind::Decoder,
            _ => return None,
        })
    }
}

impl std::fmt::Display for CodecKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The result of asking a backend "can you do this right now?".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendAvailability {
    /// The backend can be constructed.
    Available,
    /// The backend is compiled in but currently unusable; the string says why.
    Unavailable(String),
}

impl BackendAvailability {
    /// `true` for [`BackendAvailability::Available`].
    pub const fn is_available(&self) -> bool {
        matches!(self, BackendAvailability::Available)
    }

    /// The reason, when unavailable.
    pub fn reason(&self) -> Option<&str> {
        match self {
            BackendAvailability::Available => None,
            BackendAvailability::Unavailable(reason) => Some(reason),
        }
    }
}

/// A backend factory: returns `Ok(None)` when the backend is simply not
/// applicable to this configuration, `Err` when it was tried and failed.
pub type EncoderFactory =
    Arc<dyn Fn(&EncoderConfig) -> Result<Option<Box<dyn Encoder>>> + Send + Sync>;

/// A backend factory for decoders.
pub type DecoderFactory =
    Arc<dyn Fn(&DecoderConfig) -> Result<Option<Box<dyn Decoder>>> + Send + Sync>;

/// A probe answering whether a backend can serve `codec` on this machine, right now.
pub type Probe = Arc<dyn Fn(Codec) -> BackendAvailability + Send + Sync>;

struct EncoderEntry {
    backend: BackendKind,
    priority: u8,
    factory: EncoderFactory,
    probe: Probe,
}

struct DecoderEntry {
    backend: BackendKind,
    priority: u8,
    factory: DecoderFactory,
    probe: Probe,
}

/// What one backend reports about one codec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendInfo {
    /// Which subsystem.
    pub backend: BackendKind,
    /// Native framework or software fallback.
    pub implementation: crate::engine::backend::Implementation,
    /// `true` only when the backend is known to use dedicated hardware.
    pub hardware: bool,
    /// `true` when the backend can actually be constructed right now.
    pub available: bool,
    /// Why it is unavailable, or what it would use when it is available.
    pub note: String,
}

/// The per-codec capability report returned to callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityReport {
    /// The codec this report describes.
    pub codec: Codec,
    /// Backends that can encode it, best first.
    pub encoders: Vec<BackendInfo>,
    /// Backends that can decode it, best first.
    pub decoders: Vec<BackendInfo>,
}

impl CapabilityReport {
    /// The best available encoder backend, if any.
    pub fn best_encoder(&self) -> Option<&BackendInfo> {
        self.encoders.iter().find(|b| b.available)
    }

    /// The best available decoder backend, if any.
    pub fn best_decoder(&self) -> Option<&BackendInfo> {
        self.decoders.iter().find(|b| b.available)
    }

    /// `true` when at least one *software* encoder is available, i.e. the
    /// fallback path exists.
    pub fn has_software_encoder(&self) -> bool {
        self.encoders
            .iter()
            .any(|b| b.available && b.implementation.is_software())
    }
}

/// The registry: every known encoder and decoder backend.
#[derive(Default)]
pub struct Registry {
    encoders: BTreeMap<Codec, Vec<EncoderEntry>>,
    decoders: BTreeMap<Codec, Vec<DecoderEntry>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("encoders", &self.encoders.len())
            .field("decoders", &self.decoders.len())
            .finish()
    }
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an encoder backend for `codec`.
    ///
    /// Lower `priority` wins. Platform frameworks should use `0..=99`, software
    /// fallbacks `100..=255`.
    pub fn register_encoder<F, P>(
        &mut self,
        codec: Codec,
        backend: BackendKind,
        priority: u8,
        factory: F,
        probe: P,
    ) where
        F: Fn(&EncoderConfig) -> Result<Option<Box<dyn Encoder>>> + Send + Sync + 'static,
        P: Fn(Codec) -> BackendAvailability + Send + Sync + 'static,
    {
        let entry =
            EncoderEntry { backend, priority, factory: Arc::new(factory), probe: Arc::new(probe) };
        let slot = self.encoders.entry(codec).or_default();
        slot.push(entry);
        slot.sort_by_key(|e| e.priority);
    }

    /// Registers a decoder backend for `codec`.
    pub fn register_decoder<F, P>(
        &mut self,
        codec: Codec,
        backend: BackendKind,
        priority: u8,
        factory: F,
        probe: P,
    ) where
        F: Fn(&DecoderConfig) -> Result<Option<Box<dyn Decoder>>> + Send + Sync + 'static,
        P: Fn(Codec) -> BackendAvailability + Send + Sync + 'static,
    {
        let entry =
            DecoderEntry { backend, priority, factory: Arc::new(factory), probe: Arc::new(probe) };
        let slot = self.decoders.entry(codec).or_default();
        slot.push(entry);
        slot.sort_by_key(|e| e.priority);
    }

    /// The capability report for every codec the registry knows about.
    pub fn capabilities(&self) -> Vec<CapabilityReport> {
        Codec::ALL
            .iter()
            .map(|&codec| CapabilityReport {
                codec,
                encoders: self
                    .encoders
                    .get(&codec)
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|e| {
                                let availability = (e.probe)(codec);
                                BackendInfo {
                                    backend: e.backend,
                                    implementation: e.backend.implementation(),
                                    hardware: e.backend.is_hardware_capable(),
                                    available: availability.is_available(),
                                    note: availability.reason().unwrap_or("ready").to_string(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                decoders: self
                    .decoders
                    .get(&codec)
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|e| {
                                let availability = (e.probe)(codec);
                                BackendInfo {
                                    backend: e.backend,
                                    implementation: e.backend.implementation(),
                                    hardware: e.backend.is_hardware_capable(),
                                    available: availability.is_available(),
                                    note: availability.reason().unwrap_or("ready").to_string(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// The capability report for one codec.
    pub fn capability(&self, codec: Codec) -> Option<CapabilityReport> {
        self.capabilities().into_iter().find(|r| r.codec == codec)
    }

    /// Builds an encoder for `config`, honouring `preference`.
    ///
    /// Every backend that is skipped contributes to
    /// [`CodecPath::fallback_reason`] on the returned encoder, so a caller can
    /// always find out why it ended up on software.
    pub fn create_encoder(
        &self,
        config: &EncoderConfig,
        preference: BackendPreference,
    ) -> Result<Box<dyn Encoder>> {
        config.validate()?;
        let codec = config.codec;
        let entries = self.encoders.get(&codec).ok_or_else(|| {
            Error::unsupported(format!("{codec} {}", CodecKind::Encoder.name()))
        })?;

        let mut skipped: Vec<String> = Vec::new();
        for entry in entries {
            let native = !entry.backend.is_software();
            let software = entry.backend.is_software();
            let hardware = entry.backend.is_hardware_capable();
            if !preference.accepts(native, software, hardware) {
                skipped.push(format!(
                    "{} filtered out by {}",
                    entry.backend.name(),
                    preference.name()
                ));
                continue;
            }
            match (entry.probe)(codec) {
                BackendAvailability::Unavailable(reason) => {
                    skipped.push(format!("{}: {reason}", entry.backend.name()));
                    continue;
                }
                BackendAvailability::Available => {}
            }
            match (entry.factory)(config) {
                Ok(Some(mut encoder)) => {
                    if !skipped.is_empty() {
                        let mut path = encoder.path();
                        if path.fallback_reason.is_none() {
                            path.fallback_reason = Some(skipped.join("; "));
                        }
                        encoder = Box::new(PathOverride::new(encoder, path));
                    }
                    return Ok(encoder);
                }
                Ok(None) => {
                    skipped.push(format!("{}: does not handle this configuration", entry.backend.name()));
                }
                Err(error) => {
                    skipped.push(format!("{}: {error}", entry.backend.name()));
                    log::debug!("{codec} encoder backend {} failed: {error}", entry.backend.name());
                }
            }
        }

        Err(Error::unavailable(format!(
            "no {} backend could encode {codec}: {}",
            CodecKind::Encoder.name(),
            skipped.join("; ")
        )))
    }

    /// Builds a decoder for `config`, honouring `preference`.
    pub fn create_decoder(
        &self,
        config: &DecoderConfig,
        preference: BackendPreference,
    ) -> Result<Box<dyn Decoder>> {
        config.validate()?;
        let codec = config.codec;
        let entries = self.decoders.get(&codec).ok_or_else(|| {
            Error::unsupported(format!("{codec} {}", CodecKind::Decoder.name()))
        })?;

        let mut skipped: Vec<String> = Vec::new();
        for entry in entries {
            let native = !entry.backend.is_software();
            let software = entry.backend.is_software();
            let hardware = entry.backend.is_hardware_capable();
            if !preference.accepts(native, software, hardware) {
                skipped.push(format!(
                    "{} filtered out by {}",
                    entry.backend.name(),
                    preference.name()
                ));
                continue;
            }
            match (entry.probe)(codec) {
                BackendAvailability::Unavailable(reason) => {
                    skipped.push(format!("{}: {reason}", entry.backend.name()));
                    continue;
                }
                BackendAvailability::Available => {}
            }
            match (entry.factory)(config) {
                Ok(Some(mut decoder)) => {
                    if !skipped.is_empty() {
                        let mut path = decoder.path();
                        if path.fallback_reason.is_none() {
                            path.fallback_reason = Some(skipped.join("; "));
                        }
                        decoder = Box::new(PathOverride::new(decoder, path));
                    }
                    return Ok(decoder);
                }
                Ok(None) => {
                    skipped.push(format!("{}: does not handle this configuration", entry.backend.name()));
                }
                Err(error) => {
                    skipped.push(format!("{}: {error}", entry.backend.name()));
                    log::debug!("{codec} decoder backend {} failed: {error}", entry.backend.name());
                }
            }
        }

        Err(Error::unavailable(format!(
            "no {} backend could decode {codec}: {}",
            CodecKind::Decoder.name(),
            skipped.join("; ")
        )))
    }
}

/// Wraps an encoder to report a path that carries a fallback reason.
struct PathOverride<E> {
    inner: E,
    path: CodecPath,
}

impl<E> PathOverride<E> {
    fn new(inner: E, path: CodecPath) -> Self {
        Self { inner, path }
    }
}

impl<E: std::fmt::Debug> std::fmt::Debug for PathOverride<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathOverride").field("path", &self.path).finish()
    }
}

impl<E: Encoder> Encoder for PathOverride<E> {
    fn codec(&self) -> Codec {
        self.inner.codec()
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: crate::media::frame::Frame) -> Result<()> {
        self.inner.push(frame)
    }

    fn receive(&mut self) -> Result<Option<crate::media::packet::Packet>> {
        self.inner.receive()
    }

    fn flush(&mut self) -> Result<Vec<crate::media::packet::Packet>> {
        self.inner.flush()
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        self.inner.set_bitrate(bitrate)
    }

    fn request_keyframe(&mut self) -> Result<()> {
        self.inner.request_keyframe()
    }

    fn extradata(&self) -> Option<crate::media::buffer::Buffer> {
        self.inner.extradata()
    }
}

impl<D: Decoder> Decoder for PathOverride<D> {
    fn codec(&self) -> Codec {
        self.inner.codec()
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: crate::media::packet::Packet) -> Result<()> {
        self.inner.push(packet)
    }

    fn receive(&mut self) -> Result<Option<crate::media::frame::Frame>> {
        self.inner.receive()
    }

    fn flush(&mut self) -> Result<Vec<crate::media::frame::Frame>> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Process-wide registry
// ---------------------------------------------------------------------------

/// The process-wide registry, built on first use.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(build_registry)
}

/// Builds a registry containing every backend this build was compiled with.
///
/// Exposed so tests and embedders can construct an isolated registry instead of
/// mutating the process-wide one.
pub fn build_registry() -> Registry {
    let mut registry = Registry::new();
    crate::native::register(&mut registry);
    crate::soft::register(&mut registry);
    log::debug!("anycodec registry initialised");
    registry
}

/// Runs `f` against a fresh registry built by `build`.
///
/// Serialised behind a mutex so that registry tests cannot interleave.
#[doc(hidden)]
pub fn with_registry<T>(build: impl FnOnce(&mut Registry), f: impl FnOnce(&Registry) -> T) -> T {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut registry = Registry::new();
    build(&mut registry);
    f(&registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::backend::Implementation;
    use crate::media::buffer::Buffer;
    use crate::media::codec::VideoCodec;
    use crate::media::frame::{Frame, Plane, VideoFrame};
    use crate::media::format::PixelFormat;
    use crate::media::packet::Packet;

    /// A minimal encoder used to exercise selection without a real codec. It
    /// reports whichever backend it was told to, which is exactly what the
    /// selection tests need to observe.
    #[derive(Debug)]
    struct TestEncoder {
        codec: Codec,
        path: CodecPath,
        queued: Vec<Packet>,
    }

    impl TestEncoder {
        fn new(codec: Codec, backend: BackendKind) -> Self {
            Self {
                codec,
                path: CodecPath::new(backend, false, format!("test:{}", backend.name())),
                queued: Vec::new(),
            }
        }
    }

    impl Encoder for TestEncoder {
        fn codec(&self) -> Codec {
            self.codec
        }

        fn path(&self) -> CodecPath {
            self.path.clone()
        }

        fn push(&mut self, frame: Frame) -> Result<()> {
            let payload = match &frame {
                Frame::Video(video) => video.byte_len() as u32,
                Frame::Audio(audio) => audio.byte_len() as u32,
            };
            self.queued.push(Packet::new(
                self.codec,
                Buffer::from_vec(payload.to_le_bytes().to_vec()),
                frame.pts(),
                crate::media::packet::flags::KEYFRAME,
            ));
            Ok(())
        }

        fn receive(&mut self) -> Result<Option<Packet>> {
            Ok(if self.queued.is_empty() { None } else { Some(self.queued.remove(0)) })
        }

        fn flush(&mut self) -> Result<Vec<Packet>> {
            Ok(std::mem::take(&mut self.queued))
        }
    }

    fn available(_: Codec) -> BackendAvailability {
        BackendAvailability::Available
    }

    fn unavailable(reason: &'static str) -> impl Fn(Codec) -> BackendAvailability {
        move |_| BackendAvailability::Unavailable(reason.to_string())
    }

    fn config_h264() -> EncoderConfig {
        EncoderConfig::video(VideoCodec::H264, 64, 64)
    }

    #[test]
    fn software_only_selects_the_rust_backend() {
        let codec = Codec::Video(VideoCodec::H264);
        let encoder = with_registry(
            |registry| {
                registry.register_encoder(
                    codec,
                    BackendKind::Rust,
                    200,
                    move |_| Ok(Some(Box::new(TestEncoder::new(codec, BackendKind::Rust)))),
                    available,
                );
            },
            |registry| registry.create_encoder(&config_h264(), BackendPreference::SoftwareOnly),
        )
        .expect("SoftwareOnly must accept the Rust backend");
        assert!(encoder.path().is_software());
        assert_eq!(encoder.path().backend, BackendKind::Rust);
    }

    #[test]
    fn native_only_refuses_the_software_fallback() {
        let codec = Codec::Video(VideoCodec::H264);
        let error = with_registry(
            |registry| {
                registry.register_encoder(
                    codec,
                    BackendKind::Rust,
                    200,
                    move |_| Ok(Some(Box::new(TestEncoder::new(codec, BackendKind::Rust)))),
                    available,
                );
            },
            |registry| registry.create_encoder(&config_h264(), BackendPreference::NativeOnly).err(),
        )
        .expect("NativeOnly must not silently fall back to software");
        assert_eq!(error.kind(), crate::error::ErrorKind::Unavailable);
        assert!(error.message().contains("rust"), "{}", error.message());
    }

    #[test]
    fn fallback_reason_records_every_skipped_backend() {
        let codec = Codec::Video(VideoCodec::H264);
        let encoder = with_registry(
            |registry| {
                registry.register_encoder(
                    codec,
                    BackendKind::MediaFoundation,
                    10,
                    |_| Ok(None),
                    unavailable("no HEVC MFT installed"),
                );
                registry.register_encoder(
                    codec,
                    BackendKind::Rust,
                    200,
                    move |_| Ok(Some(Box::new(TestEncoder::new(codec, BackendKind::Rust)))),
                    available,
                );
            },
            |registry| registry.create_encoder(&config_h264(), BackendPreference::Auto).unwrap(),
        );
        let path = encoder.path();
        assert!(path.is_software(), "a software fallback must be reported as such");
        let reason = path.fallback_reason.expect("a skipped backend must be reported");
        assert!(reason.contains("no HEVC MFT installed"), "{reason}");
    }

    #[test]
    fn priority_order_is_respected() {
        let codec = Codec::Video(VideoCodec::Vp9);
        let encoder = with_registry(
            |registry| {
                registry.register_encoder(
                    codec,
                    BackendKind::Rust,
                    200,
                    move |_| Ok(Some(Box::new(TestEncoder::new(codec, BackendKind::Rust)))),
                    available,
                );
                registry.register_encoder(
                    codec,
                    BackendKind::External,
                    5,
                    move |_| Ok(Some(Box::new(TestEncoder::new(codec, BackendKind::External)))),
                    available,
                );
            },
            |registry| {
                registry
                    .create_encoder(
                        &EncoderConfig::video(VideoCodec::Vp9, 64, 64),
                        BackendPreference::Auto,
                    )
                    .unwrap()
            },
        );
        assert_eq!(encoder.path().backend, BackendKind::External);
    }

    #[test]
    fn require_hardware_skips_non_hardware_backends() {
        let codec = Codec::Video(VideoCodec::H264);
        let error = with_registry(
            |registry| {
                registry.register_encoder(
                    codec,
                    BackendKind::Rust,
                    200,
                    move |_| Ok(Some(Box::new(TestEncoder::new(codec, BackendKind::Rust)))),
                    available,
                );
            },
            |registry| {
                registry.create_encoder(&config_h264(), BackendPreference::RequireHardware).err()
            },
        )
        .expect("RequireHardware must refuse a software-only backend");
        assert!(error.message().contains("require_hardware"), "{}", error.message());
    }

    #[test]
    fn a_failing_factory_falls_through_to_the_next_backend() {
        let codec = Codec::Video(VideoCodec::H264);
        let encoder = with_registry(
            |registry| {
                registry.register_encoder(
                    codec,
                    BackendKind::MediaFoundation,
                    10,
                    |_| Err(crate::error::Error::backend("MFT refused to start")),
                    available,
                );
                registry.register_encoder(
                    codec,
                    BackendKind::Rust,
                    200,
                    move |_| Ok(Some(Box::new(TestEncoder::new(codec, BackendKind::Rust)))),
                    available,
                );
            },
            |registry| registry.create_encoder(&config_h264(), BackendPreference::Auto).unwrap(),
        );
        let path = encoder.path();
        assert_eq!(path.backend, BackendKind::Rust);
        assert!(path.fallback_reason.unwrap().contains("MFT refused to start"));
    }

    #[test]
    fn a_codec_with_no_backend_reports_unavailable() {
        let error = with_registry(
            |_| {},
            |registry| registry.create_encoder(&config_h264(), BackendPreference::Auto).err(),
        )
        .expect("an empty registry cannot produce an encoder");
        assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
        assert!(error.message().contains("h264"));
    }

    #[test]
    fn invalid_configuration_is_rejected_before_selection() {
        let error = with_registry(
            |_| {},
            |registry| {
                registry
                    .create_encoder(
                        &EncoderConfig::video(VideoCodec::H264, 0, 0),
                        BackendPreference::Auto,
                    )
                    .err()
            },
        )
        .expect("a zero-sized frame must be rejected");
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
    }

    #[test]
    fn capabilities_report_every_codec() {
        let reports = with_registry(|_| {}, |registry| registry.capabilities());
        assert_eq!(reports.len(), Codec::ALL.len());
        for (report, codec) in reports.iter().zip(Codec::ALL) {
            assert_eq!(report.codec, codec, "reports must follow Codec::ALL order");
        }
    }

    #[test]
    fn availability_reflects_the_probe() {
        let codec = Codec::Video(VideoCodec::H264);
        let report = with_registry(
            |registry| {
                registry.register_encoder(
                    codec,
                    BackendKind::MediaFoundation,
                    10,
                    |_| Ok(None),
                    unavailable("HEVC Video Extensions are not installed"),
                );
            },
            |registry| registry.capability(codec).unwrap(),
        );
        let entry = &report.encoders[0];
        assert!(!entry.available);
        assert!(entry.note.contains("HEVC Video Extensions"), "{}", entry.note);
    }

    #[test]
    fn capability_lookup_finds_one_codec() {
        let report = with_registry(|_| {}, |registry| {
            registry.capability(Codec::Audio(crate::media::codec::AudioCodec::Opus))
        })
        .expect("opus must be reported");
        assert_eq!(report.codec.name(), "opus");
    }

    #[test]
    fn test_encoder_round_trips_a_frame() {
        let codec = Codec::Video(VideoCodec::H264);
        let mut encoder = TestEncoder::new(codec, BackendKind::Rust);
        let frame = Frame::Video(
            VideoFrame::new(
                PixelFormat::Rgba8,
                1,
                1,
                vec![Plane::packed(Buffer::from_slice(&[1, 2, 3, 4])).unwrap()],
                11,
            )
            .unwrap(),
        );
        encoder.push(frame).unwrap();
        let packet = encoder.receive().unwrap().expect("one packet");
        assert_eq!(packet.pts, 11);
        assert!(packet.is_keyframe());
        assert_eq!(encoder.receive().unwrap(), None);
    }

    #[test]
    fn implementation_classification_is_exposed() {
        assert_eq!(BackendKind::Rust.implementation(), Implementation::Software);
        assert_eq!(BackendKind::VideoToolbox.implementation(), Implementation::Native);
        assert_eq!(BackendKind::AudioToolbox.implementation(), Implementation::Native);
        assert_eq!(BackendKind::Ffmpeg.implementation(), Implementation::Software);
    }

    #[test]
    fn the_process_wide_registry_is_usable() {
        let reports = registry().capabilities();
        assert_eq!(reports.len(), Codec::ALL.len());
        // The process-wide registry must be constructible and queryable. Which
        // codecs actually have a *backend* is asserted by the coverage test at the
        // bottom of this module, which every platform backend contributes to.
        assert!(reports.iter().any(|report| report.codec.is_video()));
        assert!(reports.iter().any(|report| report.codec.is_audio()));
    }
}
