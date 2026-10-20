//! Raw (uncompressed) media frames.

use crate::media::buffer::{Buffer, BufferBuilder};
use crate::error::{Error, Result};
use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};
use crate::ensure;

/// One plane of a raw picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    data: Buffer,
    stride: usize,
}

impl Plane {
    /// Builds a plane, rejecting strides that are too small to be meaningful.
    pub fn new(data: Buffer, stride: usize) -> Result<Self> {
        ensure!(
            stride > 0 || data.is_empty(),
            "plane stride must be positive when the plane is not empty"
        );
        ensure!(data.is_empty() || stride <= data.len(), "plane stride {stride} exceeds {} bytes", data.len());
        Ok(Self { data, stride })
    }

    /// A plane whose bytes form exactly one row.
    ///
    /// The stride equals the buffer length, so this is only correct for a plane
    /// that is a single row tall. For anything taller use [`Plane::rows`]; using
    /// this for a multi-row plane makes [`Plane::row`] read past the end of row 0.
    pub fn packed(data: Buffer) -> Result<Self> {
        let stride = if data.is_empty() { 1 } else { data.len() };
        Self::new(data, stride)
    }

    /// A plane of `row_bytes`-wide rows, with no padding between them.
    ///
    /// Rejects a length that is not a whole number of rows, because that is
    /// always a caller bug and silently rounding here produces a skewed picture
    /// several layers away.
    pub fn rows(data: Buffer, row_bytes: usize) -> Result<Self> {
        ensure!(row_bytes > 0, "plane row width must be positive");
        ensure!(
            data.is_empty() || data.len() % row_bytes == 0,
            "plane of {} bytes is not a whole number of {row_bytes}-byte rows",
            data.len()
        );
        Self::new(data, row_bytes)
    }

    /// The plane bytes, row padding included.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Distance in bytes between the start of two consecutive rows.
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Number of bytes in the plane.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// `true` when the plane carries no bytes.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// A copy of one row, with padding removed.
    pub fn row(&self, row: usize, row_bytes: usize) -> Result<&[u8]> {
        let start = row * self.stride;
        let end = start + row_bytes;
        ensure!(end <= self.data.len(), "row {row} of {row_bytes} bytes exceeds plane length {}", self.data.len());
        Ok(&self.data[start..end])
    }
}

/// An uncompressed picture plus its presentation timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    /// Sample arrangement of every plane.
    pub format: PixelFormat,
    /// Luma width in pixels.
    pub width: u32,
    /// Luma height in pixels.
    pub height: u32,
    /// One entry per plane of `format`, in `format` order.
    pub planes: Vec<Plane>,
    /// Presentation timestamp in the stream timebase.
    pub pts: i64,
    /// Duration in the stream timebase, `0` when unknown.
    pub duration: i64,
    /// `true` when this picture can be decoded without any earlier picture.
    pub keyframe: bool,
}

impl VideoFrame {
    /// Builds a frame, validating the plane count and every plane's geometry.
    pub fn new(
        format: PixelFormat,
        width: u32,
        height: u32,
        planes: Vec<Plane>,
        pts: i64,
    ) -> Result<Self> {
        ensure!(width > 0 && height > 0, "frame size must be non-zero, got {width}x{height}");
        ensure!(
            planes.len() == format.plane_count(),
            "{} needs {} planes, got {}",
            format.name(),
            format.plane_count(),
            planes.len()
        );
        for (index, plane) in planes.iter().enumerate() {
            let (_, ph) = format.plane_size(width, height, index)?;
            let minimum = format.default_stride(width, index)? * ph as usize;
            if !plane.is_empty() && plane.len() < minimum {
                return Err(Error::invalid_argument(format!(
                    "plane {index} of {}x{} {} is {} bytes, needs at least {minimum}",
                    width,
                    height,
                    format.name(),
                    plane.len()
                )));
            }
        }
        Ok(Self { format, width, height, planes, pts, duration: 0, keyframe: true })
    }

    /// Builds an interleaved-RGB frame from one packed buffer.
    pub fn from_rgb(format: PixelFormat, width: u32, height: u32, data: Buffer, pts: i64) -> Result<Self> {
        ensure!(format.is_rgb(), "{} is not an RGB format", format.name());
        let stride = width as usize * 4;
        let plane = Plane::new(data, stride)?;
        Self::new(format, width, height, vec![plane], pts)
    }

    /// Duration in the stream timebase.
    #[must_use]
    pub fn with_duration(mut self, duration: i64) -> Self {
        self.duration = duration;
        self
    }

    /// Marks the picture as a keyframe (the default).
    #[must_use]
    pub fn with_keyframe(mut self, keyframe: bool) -> Self {
        self.keyframe = keyframe;
        self
    }

    /// Total number of bytes across all planes.
    pub fn byte_len(&self) -> usize {
        self.planes.iter().map(Plane::len).sum()
    }
}

/// An uncompressed block of audio samples plus its presentation timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFrame {
    /// Encoding of every sample.
    pub format: SampleFormat,
    /// Samples per second.
    pub sample_rate: u32,
    /// Speaker arrangement.
    pub channels: ChannelLayout,
    /// Number of sample frames (one per channel), not the sample count.
    pub frames: u32,
    /// Interleaved sample bytes.
    pub samples: Buffer,
    /// Presentation timestamp in the stream timebase.
    pub pts: i64,
}

impl AudioFrame {
    /// Builds an audio frame, validating the payload length.
    pub fn new(
        format: SampleFormat,
        sample_rate: u32,
        channels: ChannelLayout,
        frames: u32,
        samples: Buffer,
        pts: i64,
    ) -> Result<Self> {
        ensure!(sample_rate > 0, "sample rate must be positive");
        let expected = channels.byte_len(frames, format);
        ensure!(
            samples.len() >= expected,
            "{} frame of {} x {} needs {expected} bytes, got {}",
            format.name(),
            frames,
            channels.name(),
            samples.len()
        );
        Ok(Self { format, sample_rate, channels, frames, samples, pts })
    }

    /// Builds an audio frame from one planar `f32` slice per channel.
    ///
    /// The result is always interleaved, which is what
    /// [`AudioFrame::samples`] is defined to hold: one sample from every channel,
    /// in channel order, per sample frame.
    pub fn from_f32_planar(
        sample_rate: u32,
        channels: ChannelLayout,
        frames: u32,
        mut planes: Vec<Vec<f32>>,
        pts: i64,
    ) -> Result<Self> {
        ensure!(
            planes.len() == channels.channels() as usize,
            "{} needs {} planes, got {}",
            channels.name(),
            channels.channels(),
            planes.len()
        );
        let mut builder = BufferBuilder::with_capacity(channels.byte_len(frames, SampleFormat::F32));
        for plane in &mut planes {
            ensure!(plane.len() >= frames as usize, "planar channel is shorter than {frames} frames");
            plane.truncate(frames as usize);
        }
        // Interleave: one sample from every channel, in channel order, per frame.
        for frame_index in 0..frames as usize {
            for plane in &planes {
                builder.push(&plane[frame_index].to_le_bytes());
            }
        }
        Self::new(SampleFormat::F32, sample_rate, channels, frames, builder.finish(), pts)
    }

    /// Total number of bytes in the payload.
    pub fn byte_len(&self) -> usize {
        self.samples.len()
    }

    /// Duration of the frame in seconds.
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames as f64 / self.sample_rate as f64
        }
    }
}

/// Either a raw picture or a raw block of audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A raw picture.
    Video(VideoFrame),
    /// A raw block of audio.
    Audio(AudioFrame),
}

impl Frame {
    /// The `pts` of the inner frame.
    pub fn pts(&self) -> i64 {
        match self {
            Frame::Video(f) => f.pts,
            Frame::Audio(f) => f.pts,
        }
    }

    /// The picture, if this is one.
    pub fn as_video(&self) -> Option<&VideoFrame> {
        match self {
            Frame::Video(f) => Some(f),
            Frame::Audio(_) => None,
        }
    }

    /// The audio block, if this is one.
    pub fn as_audio(&self) -> Option<&AudioFrame> {
        match self {
            Frame::Audio(f) => Some(f),
            Frame::Video(_) => None,
        }
    }

    /// `true` for pictures.
    pub fn is_video(&self) -> bool {
        matches!(self, Frame::Video(_))
    }

    /// `true` for audio.
    pub fn is_audio(&self) -> bool {
        matches!(self, Frame::Audio(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_frame_validates_plane_count() {
        let plane = Plane::packed(Buffer::from_slice(&[0u8; 4])).unwrap();
        assert!(VideoFrame::new(PixelFormat::I420, 2, 2, vec![plane], 0).is_err());
    }

    #[test]
    fn video_frame_accepts_tightly_packed_i420() {
        let y = Plane::packed(Buffer::from_slice(&[16u8; 16])).unwrap();
        let u = Plane::packed(Buffer::from_slice(&[128u8; 4])).unwrap();
        let v = Plane::packed(Buffer::from_slice(&[128u8; 4])).unwrap();
        let frame = VideoFrame::new(PixelFormat::I420, 4, 4, vec![y, u, v], 42).unwrap();
        assert_eq!(frame.pts, 42);
        assert_eq!(frame.byte_len(), 24);
    }

    #[test]
    fn plane_row_extraction_honours_stride() {
        let data = Buffer::from_vec(vec![1, 2, 3, 9, 4, 5, 6, 9]);
        let plane = Plane::new(data, 4).unwrap();
        assert_eq!(plane.row(0, 3).unwrap(), &[1, 2, 3]);
        assert_eq!(plane.row(1, 3).unwrap(), &[4, 5, 6]);
        assert!(plane.row(2, 3).is_err());
    }

    #[test]
    fn audio_frame_rejects_short_payload() {
        let err = AudioFrame::new(
            SampleFormat::S16,
            48_000,
            ChannelLayout::Stereo,
            512,
            Buffer::from_slice(&[0u8; 16]),
            0,
        );
        assert!(err.is_err());
    }

    #[test]
    fn planar_f32_is_interleaved() {
        let frame = AudioFrame::from_f32_planar(
            48_000,
            ChannelLayout::Stereo,
            2,
            vec![vec![1.0, 3.0], vec![2.0, 4.0]],
            0,
        )
        .unwrap();
        assert_eq!(frame.byte_len(), 16);
        assert_eq!(&frame.samples[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&frame.samples[4..8], &2.0f32.to_le_bytes());
        assert_eq!(&frame.samples[8..12], &3.0f32.to_le_bytes());
    }
}
