//! A real, complete codec used to exercise the whole pipeline without a
//! production codec: it stores each frame verbatim in a length-prefixed packet
//! and reconstructs it on the way back.
//!
//! It is deliberately *not* registered for any of the twelve production codecs,
//! because claiming `Rust` support for H.264 would make the capability report
//! lie. It is exposed so that tests, the example app and embedders can validate
//! plumbing (timestamps, buffer ownership, isolate hand-off, flush semantics)
//! independently of codec correctness.

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::{Codec, MediaKind};
use crate::media::config::EncoderConfig;
use crate::media::format::{ChannelLayout, PixelFormat, SampleFormat};
use crate::media::frame::{Frame, Plane, VideoFrame};
use crate::media::packet::{flags, Packet};

/// The diagnostic codec's [`Codec`] identifier.
///
/// It reuses the H.266 slot because [`Codec`] only models the twelve production
/// formats, but the codec is reached through its own C ABI entry points
/// (`anycodec_passthrough_encoder_new` / `..._decoder_new`) and is **not**
/// registered in the production registry, so it can never be selected for H.266.
pub(crate) const PASSTHROUGH_CODEC: Codec = Codec::Video(crate::media::codec::VideoCodec::H266);

/// A verbatim store/retrieve encoder.
#[derive(Debug)]
pub struct PassthroughEncoder {
    codec: Codec,
    path: CodecPath,
    pending: Vec<Packet>,
}

impl PassthroughEncoder {
    /// Builds an encoder for `codec`.
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            path: CodecPath::new(BackendKind::Rust, false, "rust: passthrough (verbatim store)"),
            pending: Vec::new(),
        }
    }

    /// Builds an encoder from a configuration, rejecting mismatched media kinds.
    pub fn from_config(config: &EncoderConfig) -> Result<Self> {
        Ok(Self::new(config.codec))
    }
}

impl Encoder for PassthroughEncoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        let pts = frame.pts();
        let (payload, kind) = match &frame {
            Frame::Video(v) => {
                let mut bytes = Vec::new();
                bytes.extend_from_slice(&v.width.to_le_bytes());
                bytes.extend_from_slice(&v.height.to_le_bytes());
                bytes.extend_from_slice(&v.format.raw().to_le_bytes());
                bytes.extend_from_slice(&(v.planes.len() as u32).to_le_bytes());
                for plane in &v.planes {
                    bytes.extend_from_slice(&(plane.stride() as u32).to_le_bytes());
                    bytes.extend_from_slice(&(plane.len() as u32).to_le_bytes());
                }
                for plane in &v.planes {
                    bytes.extend_from_slice(plane.data());
                }
                (bytes, MediaKind::Video)
            }
            Frame::Audio(a) => {
                let mut bytes = Vec::new();
                bytes.extend_from_slice(&a.sample_rate.to_le_bytes());
                bytes.extend_from_slice(&a.channels.raw().to_le_bytes());
                bytes.extend_from_slice(&a.format.raw().to_le_bytes());
                bytes.extend_from_slice(&a.frames.to_le_bytes());
                bytes.extend_from_slice(&a.samples);
                (bytes, MediaKind::Audio)
            }
        };
        let mut framed = Vec::with_capacity(payload.len() + 5);
        framed.push(u8::from(kind == MediaKind::Video));
        framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        framed.extend_from_slice(&payload);
        self.pending.push(Packet::new(
            self.codec,
            Buffer::from_vec(framed),
            pts,
            flags::KEYFRAME,
        ));
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        if self.pending.is_empty() {
            Ok(None)
        } else {
            Ok(Some(self.pending.remove(0)))
        }
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        Ok(std::mem::take(&mut self.pending))
    }
}

/// The inverse of [`PassthroughEncoder`].
#[derive(Debug)]
pub struct PassthroughDecoder {
    codec: Codec,
    path: CodecPath,
    pending: Vec<Frame>,
}

impl PassthroughDecoder {
    /// Builds a decoder for `codec`.
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            path: CodecPath::new(BackendKind::Rust, false, "rust: passthrough (verbatim load)"),
            pending: Vec::new(),
        }
    }
}

impl Decoder for PassthroughDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        let data = packet.data.as_slice();
        if data.len() < 5 {
            return Err(Error::backend("passthrough packet is shorter than its header"));
        }
        let is_video = data[0] != 0;
        let len = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
        if data.len() < 5 + len {
            return Err(Error::backend(format!(
                "passthrough packet declares {len} bytes but carries {}",
                data.len() - 5
            )));
        }
        let body = &data[5..5 + len];
        let frame = if is_video {
            Frame::Video(decode_video(body, packet.pts)?)
        } else {
            Frame::Audio(decode_audio(body, packet.pts)?)
        };
        self.pending.push(frame);
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        if self.pending.is_empty() {
            Ok(None)
        } else {
            Ok(Some(self.pending.remove(0)))
        }
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        Ok(std::mem::take(&mut self.pending))
    }
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32> {
    if *cursor + 4 > bytes.len() {
        return Err(Error::backend("truncated passthrough header"));
    }
    let value = u32::from_le_bytes([
        bytes[*cursor],
        bytes[*cursor + 1],
        bytes[*cursor + 2],
        bytes[*cursor + 3],
    ]);
    *cursor += 4;
    Ok(value)
}

fn decode_video(body: &[u8], pts: i64) -> Result<VideoFrame> {
    let mut cursor = 0usize;
    let width = take_u32(body, &mut cursor)?;
    let height = take_u32(body, &mut cursor)?;
    let format = PixelFormat::from_raw(take_u32(body, &mut cursor)?)
        .ok_or_else(|| Error::backend("unknown pixel format in passthrough packet"))?;
    let plane_count = take_u32(body, &mut cursor)? as usize;
    let mut geometry = Vec::with_capacity(plane_count);
    for _ in 0..plane_count {
        let stride = take_u32(body, &mut cursor)? as usize;
        let len = take_u32(body, &mut cursor)? as usize;
        geometry.push((stride, len));
    }
    let mut planes = Vec::with_capacity(plane_count);
    for (stride, len) in geometry {
        if cursor + len > body.len() {
            return Err(Error::backend("truncated passthrough plane"));
        }
        let data = Buffer::from_slice(&body[cursor..cursor + len]);
        cursor += len;
        planes.push(Plane::new(data, stride)?);
    }
    VideoFrame::new(format, width, height, planes, pts)
}

fn decode_audio(body: &[u8], pts: i64) -> Result<crate::media::frame::AudioFrame> {
    let mut cursor = 0usize;
    let sample_rate = take_u32(body, &mut cursor)?;
    let channels = ChannelLayout::from_raw(take_u32(body, &mut cursor)?)
        .ok_or_else(|| Error::backend("unknown channel layout in passthrough packet"))?;
    let format = SampleFormat::from_raw(take_u32(body, &mut cursor)?)
        .ok_or_else(|| Error::backend("unknown sample format in passthrough packet"))?;
    let frames = take_u32(body, &mut cursor)?;
    crate::media::frame::AudioFrame::new(
        format,
        sample_rate,
        channels,
        frames,
        Buffer::from_slice(&body[cursor..]),
        pts,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_video_frame() -> Frame {
        let y = Plane::packed(Buffer::from_slice(&[16u8; 16])).unwrap();
        let u = Plane::packed(Buffer::from_slice(&[128u8; 4])).unwrap();
        let v = Plane::packed(Buffer::from_slice(&[128u8; 4])).unwrap();
        Frame::Video(VideoFrame::new(PixelFormat::I420, 4, 4, vec![y, u, v], 7).unwrap())
    }

    #[test]
    fn video_round_trip_preserves_geometry() {
        let mut encoder = PassthroughEncoder::new(PASSTHROUGH_CODEC);
        let mut decoder = PassthroughDecoder::new(PASSTHROUGH_CODEC);
        encoder.push(sample_video_frame()).unwrap();
        let packet = encoder.receive().unwrap().expect("one packet");
        decoder.push(packet).unwrap();
        let frame = decoder.receive().unwrap().expect("one frame");
        let video = frame.as_video().expect("video frame");
        assert_eq!((video.width, video.height), (4, 4));
        assert_eq!(video.format, PixelFormat::I420);
        assert_eq!(video.planes.len(), 3);
        assert_eq!(video.pts, 7);
    }

    #[test]
    fn flush_drains_pending_packets() {
        let mut encoder = PassthroughEncoder::new(PASSTHROUGH_CODEC);
        encoder.push(sample_video_frame()).unwrap();
        assert_eq!(encoder.flush().unwrap().len(), 1);
        assert!(encoder.receive().unwrap().is_none());
    }

    #[test]
    fn truncated_packet_is_rejected() {
        let mut decoder = PassthroughDecoder::new(PASSTHROUGH_CODEC);
        let error = decoder
            .push(Packet::new(PASSTHROUGH_CODEC, Buffer::from_slice(&[1, 2]), 0, 0))
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Backend);
    }

    #[test]
    fn audio_round_trip() {
        let mut encoder = PassthroughEncoder::new(PASSTHROUGH_CODEC);
        let audio = crate::media::frame::AudioFrame::new(
            SampleFormat::S16,
            48_000,
            ChannelLayout::Stereo,
            4,
            Buffer::from_vec(vec![0u8; 16]),
            3,
        )
        .unwrap();
        encoder.push(Frame::Audio(audio.clone())).unwrap();
        let packet = encoder.receive().unwrap().unwrap();
        let mut decoder = PassthroughDecoder::new(PASSTHROUGH_CODEC);
        decoder.push(packet).unwrap();
        let frame = decoder.receive().unwrap().unwrap();
        assert_eq!(frame.as_audio().unwrap(), &audio);
    }
}
