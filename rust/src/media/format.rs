//! Pixel / sample formats and channel layouts.

use crate::error::{Error, Result};

/// How the samples of a raw picture are arranged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 3 planes: full-rate luma, half-rate chroma, half-rate chroma.
    I420,
    /// 2 planes: full-rate luma, interleaved half-rate chroma (U then V).
    Nv12,
    /// 2 planes: full-rate luma, interleaved half-rate chroma (V then U).
    Nv21,
    /// 1 plane, 8 bits per channel, R first.
    Rgba8,
    /// 1 plane, 8 bits per channel, B first.
    Bgra8,
    /// 1 plane, 4:2:2 packed as Y0 U Y1 V.
    Yuy2,
    /// 3 planes, 10-bit little-endian in 16-bit words.
    P010,
}

impl PixelFormat {
    /// Every format, in registration order.
    pub const ALL: [PixelFormat; 7] = [
        PixelFormat::I420,
        PixelFormat::Nv12,
        PixelFormat::Nv21,
        PixelFormat::Rgba8,
        PixelFormat::Bgra8,
        PixelFormat::Yuy2,
        PixelFormat::P010,
    ];

    /// Number of independently addressed planes a frame of this format carries.
    ///
    /// `Nv12`/`Nv21` are semi-planar (luma + one interleaved chroma plane) and
    /// `Yuy2` keeps both chroma components inside the luma plane, so it is a
    /// single-plane format.
    pub const fn plane_count(self) -> usize {
        match self {
            PixelFormat::I420 | PixelFormat::P010 => 3,
            PixelFormat::Nv12 | PixelFormat::Nv21 => 2,
            PixelFormat::Rgba8 | PixelFormat::Bgra8 | PixelFormat::Yuy2 => 1,
        }
    }

    /// Bits per pixel of a single luma sample.
    pub const fn bit_depth(self) -> u8 {
        match self {
            PixelFormat::P010 => 10,
            _ => 8,
        }
    }

    /// `true` when the format uses a separate alpha plane or channel.
    pub const fn has_alpha(self) -> bool {
        matches!(self, PixelFormat::Rgba8 | PixelFormat::Bgra8)
    }

    /// `true` for formats that carry full chroma resolution.
    pub const fn is_rgb(self) -> bool {
        matches!(self, PixelFormat::Rgba8 | PixelFormat::Bgra8)
    }

    /// Canonical lower-case name.
    pub const fn name(self) -> &'static str {
        match self {
            PixelFormat::I420 => "i420",
            PixelFormat::Nv12 => "nv12",
            PixelFormat::Nv21 => "nv21",
            PixelFormat::Rgba8 => "rgba8",
            PixelFormat::Bgra8 => "bgra8",
            PixelFormat::Yuy2 => "yuy2",
            PixelFormat::P010 => "p010",
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            PixelFormat::I420 => 1,
            PixelFormat::Nv12 => 2,
            PixelFormat::Nv21 => 3,
            PixelFormat::Rgba8 => 4,
            PixelFormat::Bgra8 => 5,
            PixelFormat::Yuy2 => 6,
            PixelFormat::P010 => 7,
        }
    }

    /// Parses a C ABI pixel format value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            1 => PixelFormat::I420,
            2 => PixelFormat::Nv12,
            3 => PixelFormat::Nv21,
            4 => PixelFormat::Rgba8,
            5 => PixelFormat::Bgra8,
            6 => PixelFormat::Yuy2,
            7 => PixelFormat::P010,
            _ => return None,
        })
    }

    /// Parses a canonical name.
    pub fn from_name(name: &str) -> Option<Self> {
        let lowered = name.to_ascii_lowercase();
        Some(match lowered.replace(['.', '-', '_', ' '], "").as_str() {
            "i420" | "yuv420p" | "i420p" => PixelFormat::I420,
            "nv12" => PixelFormat::Nv12,
            "nv21" => PixelFormat::Nv21,
            "rgba8" | "rgba" => PixelFormat::Rgba8,
            "bgra8" | "bgra" => PixelFormat::Bgra8,
            "yuy2" | "yuyv" => PixelFormat::Yuy2,
            "p010" | "p010le" => PixelFormat::P010,
            _ => return None,
        })
    }

    /// Width/height of the given plane, or an error when the index is invalid.
    pub fn plane_size(self, width: u32, height: u32, plane: usize) -> Result<(u32, u32)> {
        if plane >= self.plane_count() {
            return Err(Error::invalid_argument(format!(
                "{} has {} planes, requested index {plane}",
                self.name(),
                self.plane_count()
            )));
        }
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        Ok(match (self, plane) {
            (PixelFormat::I420 | PixelFormat::P010, 0) => (width, height),
            (PixelFormat::I420 | PixelFormat::P010, _) => (cw, ch),
            (PixelFormat::Nv12 | PixelFormat::Nv21, 0) => (width, height),
            (PixelFormat::Nv12 | PixelFormat::Nv21, _) => (cw * 2, ch),
            (PixelFormat::Yuy2, 0) => (width * 2, height),
            (PixelFormat::Yuy2, _) => (0, 0),
            (PixelFormat::Rgba8 | PixelFormat::Bgra8, _) => (width * 4, height),
        })
    }

    /// Minimum tightly packed stride of the given plane for `width` pixels.
    pub fn default_stride(self, width: u32, plane: usize) -> Result<usize> {
        let (w, _) = self.plane_size(width, 1, plane)?;
        let bytes_per_sample = if self.bit_depth() > 8 { 2 } else { 1 };
        Ok(w as usize * bytes_per_sample)
    }
}

/// How the samples of a raw audio frame are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// 32-bit IEEE-754, nominally -1.0..=1.0.
    F32,
    /// Signed 16-bit little-endian.
    S16,
    /// Signed 32-bit little-endian.
    S32,
    /// Unsigned 8-bit offset by 128.
    U8,
}

impl SampleFormat {
    /// Every format, in registration order.
    pub const ALL: [SampleFormat; 4] =
        [SampleFormat::F32, SampleFormat::S16, SampleFormat::S32, SampleFormat::U8];

    /// Bytes used by one sample of one channel.
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            SampleFormat::F32 | SampleFormat::S32 => 4,
            SampleFormat::S16 => 2,
            SampleFormat::U8 => 1,
        }
    }

    /// Canonical lower-case name.
    pub const fn name(self) -> &'static str {
        match self {
            SampleFormat::F32 => "f32",
            SampleFormat::S16 => "s16",
            SampleFormat::S32 => "s32",
            SampleFormat::U8 => "u8",
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            SampleFormat::F32 => 1,
            SampleFormat::S16 => 2,
            SampleFormat::S32 => 3,
            SampleFormat::U8 => 4,
        }
    }

    /// Parses a C ABI sample format value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            1 => SampleFormat::F32,
            2 => SampleFormat::S16,
            3 => SampleFormat::S32,
            4 => SampleFormat::U8,
            _ => return None,
        })
    }

    /// Parses a canonical name.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "f32" | "float" | "flt" | "fltp" => SampleFormat::F32,
            "s16" | "i16" | "s16le" | "short" => SampleFormat::S16,
            "s32" | "i32" | "s32le" | "int" => SampleFormat::S32,
            "u8" | "u8p" => SampleFormat::U8,
            _ => return None,
        })
    }
}

/// Speaker arrangement of an audio frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    /// One channel.
    Mono,
    /// Two channels, left then right.
    Stereo,
    /// Three channels: front-left, front-right, low frequency.
    Stereo21,
    /// Four channels: front-left, front-right, back-left, back-right.
    Quad,
    /// Six channels (5.1): FL, FR, FC, LFE, BL, BR.
    FiveOne,
    /// Eight channels (7.1): FL, FR, FC, LFE, BL, BR, SL, SR.
    SevenOne,
}

impl ChannelLayout {
    /// Every layout, in registration order.
    pub const ALL: [ChannelLayout; 6] = [
        ChannelLayout::Mono,
        ChannelLayout::Stereo,
        ChannelLayout::Stereo21,
        ChannelLayout::Quad,
        ChannelLayout::FiveOne,
        ChannelLayout::SevenOne,
    ];

    /// Number of channels.
    pub const fn channels(self) -> u32 {
        match self {
            ChannelLayout::Mono => 1,
            ChannelLayout::Stereo => 2,
            ChannelLayout::Stereo21 => 3,
            ChannelLayout::Quad => 4,
            ChannelLayout::FiveOne => 6,
            ChannelLayout::SevenOne => 8,
        }
    }

    /// Canonical lower-case name.
    pub const fn name(self) -> &'static str {
        match self {
            ChannelLayout::Mono => "mono",
            ChannelLayout::Stereo => "stereo",
            ChannelLayout::Stereo21 => "2.1",
            ChannelLayout::Quad => "quad",
            ChannelLayout::FiveOne => "5.1",
            ChannelLayout::SevenOne => "7.1",
        }
    }

    /// The `u32` value used by the C ABI.
    pub const fn raw(self) -> u32 {
        match self {
            ChannelLayout::Mono => 1,
            ChannelLayout::Stereo => 2,
            ChannelLayout::Stereo21 => 3,
            ChannelLayout::Quad => 4,
            ChannelLayout::FiveOne => 5,
            ChannelLayout::SevenOne => 6,
        }
    }

    /// Parses a C ABI channel layout value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            3 => ChannelLayout::Stereo21,
            4 => ChannelLayout::Quad,
            5 => ChannelLayout::FiveOne,
            6 => ChannelLayout::SevenOne,
            _ => return None,
        })
    }

    /// Maps a plain channel count onto the closest known layout.
    pub const fn from_channel_count(count: u32) -> Option<Self> {
        Some(match count {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            3 => ChannelLayout::Stereo21,
            4 => ChannelLayout::Quad,
            6 => ChannelLayout::FiveOne,
            8 => ChannelLayout::SevenOne,
            _ => return None,
        })
    }

    /// Interleaved byte size of `frames` sample frames.
    pub const fn byte_len(self, frames: u32, format: SampleFormat) -> usize {
        self.channels() as usize * frames as usize * format.bytes_per_sample()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_formats_roundtrip() {
        for format in PixelFormat::ALL {
            assert_eq!(PixelFormat::from_raw(format.raw()), Some(format));
            assert_eq!(PixelFormat::from_name(format.name()), Some(format));
        }
    }

    #[test]
    fn plane_geometry_is_correct() {
        assert_eq!(PixelFormat::I420.plane_size(1920, 1080, 0).unwrap(), (1920, 1080));
        assert_eq!(PixelFormat::I420.plane_size(1921, 1081, 1).unwrap(), (961, 541));
        assert_eq!(PixelFormat::Nv12.plane_size(1920, 1080, 1).unwrap(), (960 * 2, 540));
        assert!(PixelFormat::I420.plane_size(16, 16, 3).is_err());
    }

    #[test]
    fn sample_formats_roundtrip() {
        for format in SampleFormat::ALL {
            assert_eq!(SampleFormat::from_raw(format.raw()), Some(format));
            assert_eq!(SampleFormat::from_name(format.name()), Some(format));
        }
    }

    #[test]
    fn channel_layouts_roundtrip() {
        for layout in ChannelLayout::ALL {
            assert_eq!(ChannelLayout::from_raw(layout.raw()), Some(layout));
            assert_eq!(ChannelLayout::from_channel_count(layout.channels()), Some(layout));
        }
        assert_eq!(ChannelLayout::from_channel_count(5), None);
    }

    #[test]
    fn interleaved_length_matches() {
        assert_eq!(ChannelLayout::Stereo.byte_len(1024, SampleFormat::S16), 4096);
    }
}
