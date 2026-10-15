//! Codec identifiers and their platform MIME names.

use std::fmt;

/// Whether a codec compresses pictures or sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Compressed pictures.
    Video,
    /// Compressed samples.
    Audio,
}

/// The video codecs AnyCodec knows how to route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VideoCodec {
    /// ITU-T H.264 / ISO-IEC 14496-10 (AVC).
    H264,
    /// ITU-T H.265 / ISO-IEC 23008-2 (HEVC).
    H265,
    /// ITU-T H.266 / ISO-IEC 23090-3 (VVC).
    H266,
    /// Google VP8 (RFC 6386).
    Vp8,
    /// Google VP9.
    Vp9,
    /// AOMedia AV1.
    Av1,
}

/// The audio codecs AnyCodec knows how to route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AudioCodec {
    /// MPEG-1/2 Audio Layer III.
    Mp3,
    /// MPEG-4 Advanced Audio Coding (LC/HE/HEv2).
    Aac,
    /// Xiph Vorbis.
    Vorbis,
    /// IETF Opus (RFC 6716).
    Opus,
    /// Xiph FLAC (lossless).
    Flac,
    /// Apple Lossless Audio Codec.
    Alac,
}

/// Either a video or an audio codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Codec {
    /// A picture codec.
    Video(VideoCodec),
    /// A sound codec.
    Audio(AudioCodec),
}

impl VideoCodec {
    /// Every video codec, in registration order.
    pub const ALL: [VideoCodec; 6] = [
        VideoCodec::H264,
        VideoCodec::H265,
        VideoCodec::H266,
        VideoCodec::Vp8,
        VideoCodec::Vp9,
        VideoCodec::Av1,
    ];

    /// Lower-case canonical name.
    pub const fn name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "h264",
            VideoCodec::H265 => "h265",
            VideoCodec::H266 => "h266",
            VideoCodec::Vp8 => "vp8",
            VideoCodec::Vp9 => "vp9",
            VideoCodec::Av1 => "av1",
        }
    }

    /// Android / MediaCodec MIME type.
    pub const fn mime(self) -> &'static str {
        match self {
            VideoCodec::H264 => "video/avc",
            VideoCodec::H265 => "video/hevc",
            VideoCodec::H266 => "video/vvc",
            VideoCodec::Vp8 => "video/x-vnd.on2.vp8",
            VideoCodec::Vp9 => "video/x-vnd.on2.vp9",
            VideoCodec::Av1 => "video/av01",
        }
    }

    /// `libavcodec` short name.
    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "h264",
            VideoCodec::H265 => "hevc",
            VideoCodec::H266 => "vvc",
            VideoCodec::Vp8 => "vp8",
            VideoCodec::Vp9 => "vp9",
            VideoCodec::Av1 => "av1",
        }
    }

    /// Human readable label.
    pub const fn label(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264/AVC",
            VideoCodec::H265 => "H.265/HEVC",
            VideoCodec::H266 => "H.266/VVC",
            VideoCodec::Vp8 => "VP8",
            VideoCodec::Vp9 => "VP9",
            VideoCodec::Av1 => "AV1",
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            VideoCodec::H264 => 1,
            VideoCodec::H265 => 2,
            VideoCodec::H266 => 3,
            VideoCodec::Vp8 => 4,
            VideoCodec::Vp9 => 5,
            VideoCodec::Av1 => 6,
        }
    }
}

impl AudioCodec {
    /// Every audio codec, in registration order.
    pub const ALL: [AudioCodec; 6] = [
        AudioCodec::Mp3,
        AudioCodec::Aac,
        AudioCodec::Vorbis,
        AudioCodec::Opus,
        AudioCodec::Flac,
        AudioCodec::Alac,
    ];

    /// Lower-case canonical name.
    pub const fn name(self) -> &'static str {
        match self {
            AudioCodec::Mp3 => "mp3",
            AudioCodec::Aac => "aac",
            AudioCodec::Vorbis => "vorbis",
            AudioCodec::Opus => "opus",
            AudioCodec::Flac => "flac",
            AudioCodec::Alac => "alac",
        }
    }

    /// Android / MediaCodec MIME type.
    pub const fn mime(self) -> &'static str {
        match self {
            AudioCodec::Mp3 => "audio/mpeg",
            AudioCodec::Aac => "audio/mp4a-latm",
            AudioCodec::Vorbis => "audio/vorbis",
            AudioCodec::Opus => "audio/opus",
            AudioCodec::Flac => "audio/flac",
            AudioCodec::Alac => "audio/alac",
        }
    }

    /// `libavcodec` short name.
    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            AudioCodec::Mp3 => "mp3",
            AudioCodec::Aac => "aac",
            AudioCodec::Vorbis => "vorbis",
            AudioCodec::Opus => "opus",
            AudioCodec::Flac => "flac",
            AudioCodec::Alac => "alac",
        }
    }

    /// Human readable label.
    pub const fn label(self) -> &'static str {
        match self {
            AudioCodec::Mp3 => "MP3",
            AudioCodec::Aac => "AAC",
            AudioCodec::Vorbis => "Vorbis",
            AudioCodec::Opus => "Opus",
            AudioCodec::Flac => "FLAC",
            AudioCodec::Alac => "ALAC",
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            AudioCodec::Mp3 => 0x101,
            AudioCodec::Aac => 0x102,
            AudioCodec::Vorbis => 0x103,
            AudioCodec::Opus => 0x104,
            AudioCodec::Flac => 0x105,
            AudioCodec::Alac => 0x106,
        }
    }
}

impl Codec {
    /// Every codec AnyCodec routes, video first.
    pub const ALL: [Codec; 12] = [
        Codec::Video(VideoCodec::H264),
        Codec::Video(VideoCodec::H265),
        Codec::Video(VideoCodec::H266),
        Codec::Video(VideoCodec::Vp8),
        Codec::Video(VideoCodec::Vp9),
        Codec::Video(VideoCodec::Av1),
        Codec::Audio(AudioCodec::Mp3),
        Codec::Audio(AudioCodec::Aac),
        Codec::Audio(AudioCodec::Vorbis),
        Codec::Audio(AudioCodec::Opus),
        Codec::Audio(AudioCodec::Flac),
        Codec::Audio(AudioCodec::Alac),
    ];

    /// Canonical lower-case name (`"h265"`, `"opus"`, ...).
    pub const fn name(self) -> &'static str {
        match self {
            Codec::Video(c) => c.name(),
            Codec::Audio(c) => c.name(),
        }
    }

    /// Human readable label.
    pub const fn label(self) -> &'static str {
        match self {
            Codec::Video(c) => c.label(),
            Codec::Audio(c) => c.label(),
        }
    }

    /// Android / MediaCodec MIME type.
    pub const fn mime(self) -> &'static str {
        match self {
            Codec::Video(c) => c.mime(),
            Codec::Audio(c) => c.mime(),
        }
    }

    /// `libavcodec` short name.
    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            Codec::Video(c) => c.ffmpeg_name(),
            Codec::Audio(c) => c.ffmpeg_name(),
        }
    }

    /// Whether this codec compresses pictures or sound.
    pub const fn kind(self) -> MediaKind {
        match self {
            Codec::Video(_) => MediaKind::Video,
            Codec::Audio(_) => MediaKind::Audio,
        }
    }

    /// `true` for picture codecs.
    pub const fn is_video(self) -> bool {
        matches!(self, Codec::Video(_))
    }

    /// `true` for sound codecs.
    pub const fn is_audio(self) -> bool {
        matches!(self, Codec::Audio(_))
    }

    /// The picture codec, if this is one.
    pub const fn as_video(self) -> Option<VideoCodec> {
        match self {
            Codec::Video(c) => Some(c),
            Codec::Audio(_) => None,
        }
    }

    /// The sound codec, if this is one.
    pub const fn as_audio(self) -> Option<AudioCodec> {
        match self {
            Codec::Audio(c) => Some(c),
            Codec::Video(_) => None,
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            Codec::Video(c) => c.raw(),
            Codec::Audio(c) => c.raw(),
        }
    }

    /// Parses a C ABI codec value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            1 => Codec::Video(VideoCodec::H264),
            2 => Codec::Video(VideoCodec::H265),
            3 => Codec::Video(VideoCodec::H266),
            4 => Codec::Video(VideoCodec::Vp8),
            5 => Codec::Video(VideoCodec::Vp9),
            6 => Codec::Video(VideoCodec::Av1),
            0x101 => Codec::Audio(AudioCodec::Mp3),
            0x102 => Codec::Audio(AudioCodec::Aac),
            0x103 => Codec::Audio(AudioCodec::Vorbis),
            0x104 => Codec::Audio(AudioCodec::Opus),
            0x105 => Codec::Audio(AudioCodec::Flac),
            0x106 => Codec::Audio(AudioCodec::Alac),
            _ => return None,
        })
    }

    /// Parses a canonical lower-case name (`"h264"`, `"opus"`, ...).
    pub fn from_name(name: &str) -> Option<Self> {
        let lowered = name.to_ascii_lowercase();
        let normalized = lowered.replace(['.', '-', '_', ' '], "");
        Some(match normalized.as_str() {
            "h264" | "avc" | "avc1" | "x264" => Codec::Video(VideoCodec::H264),
            "h265" | "hevc" | "hvc1" | "hev1" | "x265" => Codec::Video(VideoCodec::H265),
            "h266" | "vvc" | "vvc1" | "h266vvc" => Codec::Video(VideoCodec::H266),
            "vp8" => Codec::Video(VideoCodec::Vp8),
            "vp9" => Codec::Video(VideoCodec::Vp9),
            "av1" | "av01" => Codec::Video(VideoCodec::Av1),
            "mp3" | "mpeg3" | "mpga" => Codec::Audio(AudioCodec::Mp3),
            "aac" | "mp4a" | "aaclc" => Codec::Audio(AudioCodec::Aac),
            "vorbis" => Codec::Audio(AudioCodec::Vorbis),
            "opus" => Codec::Audio(AudioCodec::Opus),
            "flac" => Codec::Audio(AudioCodec::Flac),
            "alac" => Codec::Audio(AudioCodec::Alac),
            _ => return None,
        })
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Defaults to H.264, the codec every targeted platform is guaranteed to handle.
impl Default for Codec {
    fn default() -> Self {
        Codec::Video(VideoCodec::H264)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_roundtrip_is_total() {
        for codec in Codec::ALL {
            assert_eq!(Codec::from_raw(codec.raw()), Some(codec), "{codec:?}");
        }
        assert_eq!(Codec::from_raw(0), None);
        assert_eq!(Codec::from_raw(7), None);
        assert_eq!(Codec::from_raw(0x107), None);
    }

    #[test]
    fn names_roundtrip() {
        for codec in Codec::ALL {
            assert_eq!(Codec::from_name(codec.name()), Some(codec), "{codec:?}");
        }
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(Codec::from_name("video/avc"), None);
        assert_eq!(Codec::from_name("HEVC"), Some(Codec::Video(VideoCodec::H265)));
        assert_eq!(Codec::from_name("H.264"), Some(Codec::Video(VideoCodec::H264)));
        assert_eq!(Codec::from_name("av01"), Some(Codec::Video(VideoCodec::Av1)));
    }

    #[test]
    fn kinds_are_consistent() {
        for codec in Codec::ALL {
            assert_eq!(codec.is_video(), codec.kind() == MediaKind::Video);
            assert_eq!(codec.is_audio(), codec.kind() == MediaKind::Audio);
        }
    }
}
