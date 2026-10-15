//! Encoder and decoder configuration.

use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::{AudioCodec, Codec, VideoCodec};
use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};
use crate::ensure;

/// Encoder speed/quality trade-off hint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Preset {
    /// Lowest latency, largest output.
    Speed,
    /// Platform default.
    #[default]
    Balanced,
    /// Smallest output, highest latency.
    Quality,
}

impl Preset {
    /// Every preset, cheapest first.
    pub const ALL: [Preset; 3] = [Preset::Speed, Preset::Balanced, Preset::Quality];

    /// Stable lower-case token.
    pub const fn name(self) -> &'static str {
        match self {
            Preset::Speed => "speed",
            Preset::Balanced => "balanced",
            Preset::Quality => "quality",
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            Preset::Speed => 0,
            Preset::Balanced => 1,
            Preset::Quality => 2,
        }
    }

    /// Parses a C ABI preset value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            0 => Preset::Speed,
            1 => Preset::Balanced,
            2 => Preset::Quality,
            _ => return None,
        })
    }
}

/// Everything needed to start an encoder.
///
/// One flat struct serves both media kinds so that the C ABI stays a single
/// `#[repr(C)]` struct; the fields that do not apply to the chosen codec are
/// simply ignored by [`EncoderConfig::validate`].
#[derive(Clone, Debug, PartialEq)]
pub struct EncoderConfig {
    /// What to compress.
    pub codec: Codec,

    // ---- video ------------------------------------------------------------
    /// Luma width in pixels. Ignored for audio codecs.
    pub width: u32,
    /// Luma height in pixels. Ignored for audio codecs.
    pub height: u32,
    /// Layout of the frames that will be pushed.
    pub pixel_format: PixelFormat,
    /// Frames per second numerator.
    pub framerate_num: u32,
    /// Frames per second denominator.
    pub framerate_den: u32,
    /// Target distance between keyframes, in frames. `0` lets the backend choose.
    pub keyframe_interval: u32,

    // ---- audio ------------------------------------------------------------
    /// Samples per second. Ignored for video codecs.
    pub sample_rate: u32,
    /// Speaker arrangement. Ignored for video codecs.
    pub channels: ChannelLayout,
    /// Layout of the samples that will be pushed. Ignored for video codecs.
    pub sample_format: SampleFormat,
    /// Requested encoded frame length in milliseconds. `0` lets the backend choose.
    pub frame_duration_ms: u32,

    // ---- shared -----------------------------------------------------------
    /// Target bitrate in bits per second. `0` means "let `quality` decide".
    pub bitrate: u64,
    /// Quality hint in `0.0..=1.0`, or `0.0` for "unset, use `bitrate`".
    pub quality: f32,
    /// Speed/quality trade-off.
    pub preset: Preset,
    /// Backend-specific key/value options, e.g. `("profile", "high")`.
    pub options: Vec<(String, String)>,
}

impl EncoderConfig {
    /// A video encoder configuration with sensible defaults.
    pub fn video(codec: VideoCodec, width: u32, height: u32) -> Self {
        Self {
            codec: Codec::Video(codec),
            width,
            height,
            pixel_format: PixelFormat::I420,
            framerate_num: 30,
            framerate_den: 1,
            keyframe_interval: 0,
            sample_rate: 0,
            channels: ChannelLayout::Stereo,
            sample_format: SampleFormat::F32,
            frame_duration_ms: 0,
            bitrate: 4_000_000,
            quality: 0.0,
            preset: Preset::Balanced,
            options: Vec::new(),
        }
    }

    /// An audio encoder configuration with sensible defaults.
    pub fn audio(codec: AudioCodec, sample_rate: u32, channels: ChannelLayout) -> Self {
        Self {
            codec: Codec::Audio(codec),
            width: 0,
            height: 0,
            pixel_format: PixelFormat::I420,
            framerate_num: 0,
            framerate_den: 1,
            keyframe_interval: 0,
            sample_rate,
            channels,
            sample_format: SampleFormat::F32,
            frame_duration_ms: 0,
            bitrate: 128_000,
            quality: 0.0,
            preset: Preset::Balanced,
            options: Vec::new(),
        }
    }

    /// Sets the target bitrate.
    #[must_use]
    pub fn with_bitrate(mut self, bitrate: u64) -> Self {
        self.bitrate = bitrate;
        self
    }

    /// Sets the quality hint.
    #[must_use]
    pub fn with_quality(mut self, quality: f32) -> Self {
        self.quality = quality;
        self
    }

    /// Sets the speed/quality preset.
    #[must_use]
    pub fn with_preset(mut self, preset: Preset) -> Self {
        self.preset = preset;
        self
    }

    /// Sets the frame rate.
    #[must_use]
    pub fn with_framerate(mut self, num: u32, den: u32) -> Self {
        self.framerate_num = num;
        self.framerate_den = den;
        self
    }

    /// Sets the keyframe interval in frames.
    #[must_use]
    pub fn with_keyframe_interval(mut self, frames: u32) -> Self {
        self.keyframe_interval = frames;
        self
    }

    /// Sets the input layout of the frames that will be pushed.
    #[must_use]
    pub fn with_pixel_format(mut self, format: PixelFormat) -> Self {
        self.pixel_format = format;
        self
    }

    /// Sets the input layout of the samples that will be pushed.
    #[must_use]
    pub fn with_sample_format(mut self, format: SampleFormat) -> Self {
        self.sample_format = format;
        self
    }

    /// Appends one backend-specific option.
    #[must_use]
    pub fn with_option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.push((key.into(), value.into()));
        self
    }

    /// Looks up a backend-specific option.
    pub fn option(&self, key: &str) -> Option<&str> {
        self.options.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// The frame rate as a float, or `None` when unset.
    pub fn framerate(&self) -> Option<f64> {
        if self.framerate_den == 0 || self.framerate_num == 0 {
            None
        } else {
            Some(self.framerate_num as f64 / self.framerate_den as f64)
        }
    }

    /// The effective quality hint: `quality` when set, otherwise derived from
    /// `bitrate` and the input geometry.
    pub fn effective_quality(&self) -> f32 {
        if self.quality > 0.0 {
            return self.quality.clamp(0.0, 1.0);
        }
        // ~0.5 at 2 Mbit/s for 1280x720, scaled linearly with bits-per-pixel-per-frame.
        let pixels = (self.width as f64 * self.height as f64).max(1.0);
        let fps = self.framerate().unwrap_or(30.0).max(1.0);
        let bpp = self.bitrate as f64 / (pixels * fps);
        (bpp / 0.15).clamp(0.05, 1.0) as f32
    }

    /// Rejects combinations no backend can honour.
    pub fn validate(&self) -> Result<()> {
        match self.codec {
            Codec::Video(codec) => {
                ensure!(self.width > 0 && self.height > 0, "{codec} needs a non-zero frame size");
                ensure!(
                    self.width <= 16_384 && self.height <= 16_384,
                    "{codec} frame size {}x{} exceeds the 16384 pixel limit",
                    self.width,
                    self.height
                );
                ensure!(
                    self.width % 2 == 0 && self.height % 2 == 0,
                    "{codec} needs even dimensions, got {}x{}",
                    self.width,
                    self.height
                );
                if self.bitrate == 0 {
                    ensure!(
                        self.quality > 0.0,
                        "{codec} needs either a bitrate or a quality hint"
                    );
                }
                if codec == VideoCodec::H266 {
                    // VVC has no 8-bit 4:2:0 I420-only constraint we must enforce,
                    // but nothing ships a VVC hardware encoder yet, so make the
                    // software fallback explicit rather than silently choosing it.
                    log::debug!(
                        "h266 requested; no platform framework exposes a VVC encoder, \
                         expect the software fallback (see CodecPath::is_software)"
                    );
                }
            }
            Codec::Audio(codec) => {
                ensure!(self.sample_rate > 0, "{codec} needs a positive sample rate");
                ensure!(
                    self.channels.channels() >= 1 && self.channels.channels() <= 8,
                    "{codec} supports 1..=8 channels, got {}",
                    self.channels.channels()
                );
            }
        }
        Ok(())
    }
}

/// Everything needed to start a decoder.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DecoderConfig {
    /// What to decompress.
    pub codec: Codec,
    /// Codec configuration bytes, when the container supplies them
    /// (H.264 `avcC`, HEVC `hvcC`, AAC `AudioSpecificConfig`, Opus `OpusHead`...).
    pub extradata: Buffer,
    /// Hint: coded width.
    pub width: u32,
    /// Hint: coded height.
    pub height: u32,
    /// Requested output layout. `None` means "whatever the backend prefers".
    pub pixel_format: Option<PixelFormat>,
    /// Hint: samples per second.
    pub sample_rate: u32,
    /// Hint: speaker arrangement.
    pub channels: Option<ChannelLayout>,
    /// Requested output sample layout. `None` means "whatever the backend prefers".
    pub sample_format: Option<SampleFormat>,
    /// `true` when the incoming bitstream uses Annex-B start codes
    /// (`00 00 01`) rather than length-prefixed NAL units. Ignored by codecs
    /// that have no such distinction.
    pub annex_b: bool,
}

impl DecoderConfig {
    /// A decoder configuration for `codec` with no hints.
    pub fn new(codec: Codec) -> Self {
        Self { codec, ..Default::default() }
    }

    /// Attaches codec configuration bytes.
    #[must_use]
    pub fn with_extradata(mut self, extradata: Buffer) -> Self {
        self.extradata = extradata;
        self
    }

    /// Attaches a coded-size hint.
    #[must_use]
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Attaches an audio geometry hint.
    #[must_use]
    pub fn with_audio(mut self, sample_rate: u32, channels: ChannelLayout) -> Self {
        self.sample_rate = sample_rate;
        self.channels = Some(channels);
        self
    }

    /// Requests a particular output picture layout.
    #[must_use]
    pub fn with_pixel_format(mut self, format: PixelFormat) -> Self {
        self.pixel_format = Some(format);
        self
    }

    /// Requests a particular output sample layout.
    #[must_use]
    pub fn with_sample_format(mut self, format: SampleFormat) -> Self {
        self.sample_format = Some(format);
        self
    }

    /// Declares that the bitstream uses Annex-B start codes.
    #[must_use]
    pub fn with_annex_b(mut self, annex_b: bool) -> Self {
        self.annex_b = annex_b;
        self
    }

    /// Rejects combinations no backend can honour.
    pub fn validate(&self) -> Result<()> {
        if let Codec::Video(codec) = self.codec {
            ensure!(
                self.width <= 16_384 && self.height <= 16_384,
                "{codec} hints {}x{} exceed the 16384 pixel limit",
                self.width,
                self.height
            );
        }
        if let Codec::Audio(codec) = self.codec {
            if self.sample_rate > 384_000 {
                return Err(Error::invalid_argument(format!(
                    "{codec} sample rate {} is implausible",
                    self.sample_rate
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_config_rejects_odd_dimensions() {
        let config = EncoderConfig::video(VideoCodec::H264, 1921, 1080);
        assert!(config.validate().is_err());
    }

    #[test]
    fn video_config_accepts_typical_input() {
        let config = EncoderConfig::video(VideoCodec::H264, 1920, 1080)
            .with_framerate(30, 1)
            .with_bitrate(6_000_000);
        assert!(config.validate().is_ok());
        assert_eq!(config.framerate(), Some(30.0));
    }

    #[test]
    fn audio_config_requires_a_sample_rate() {
        let mut config = EncoderConfig::audio(AudioCodec::Opus, 48_000, ChannelLayout::Stereo);
        assert!(config.validate().is_ok());
        config.sample_rate = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn quality_falls_back_to_bitrate() {
        let config = EncoderConfig::video(VideoCodec::H264, 1280, 720)
            .with_framerate(30, 1)
            .with_bitrate(2_000_000);
        let quality = config.effective_quality();
        assert!(quality > 0.2 && quality < 0.8, "unexpected quality {quality}");
    }

    #[test]
    fn explicit_quality_wins() {
        let config = EncoderConfig::video(VideoCodec::H264, 1280, 720).with_quality(0.9);
        assert!((config.effective_quality() - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn options_are_looked_up() {
        let config = EncoderConfig::video(VideoCodec::H265, 640, 480)
            .with_option("profile", "main10");
        assert_eq!(config.option("profile"), Some("main10"));
        assert_eq!(config.option("missing"), None);
    }

    #[test]
    fn decoder_config_defaults_are_valid() {
        let config = DecoderConfig::new(Codec::Video(VideoCodec::H264))
            .with_extradata(Buffer::from_slice(&[1, 2, 3]));
        assert!(config.validate().is_ok());
        assert_eq!(config.extradata.len(), 3);
    }
}
