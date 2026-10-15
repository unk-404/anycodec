//! The contracts every backend implements.
//!
//! Both traits use the same push/pull split, because that is the shape every
//! hardware media API has:
//!
//! * MediaCodec — `dequeueInputBuffer` / `queueInputBuffer` / `dequeueOutputBuffer`
//! * VideoToolbox — `VTCompressionSessionEncodeFrame` / output callback
//! * Media Foundation — `ProcessInput` / `ProcessOutput`
//! * GStreamer — `push-buffer` / pull `appsink`
//!
//! `receive()` returning `Ok(None)` means *"give me more input"*, never
//! *"stream finished"*. End of stream is expressed by [`Encoder::flush`] /
//! [`Decoder::flush`], or by pushing a packet with the `EOS` flag.

use crate::engine::backend::CodecPath;
use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::Codec;
use crate::media::frame::Frame;
use crate::media::packet::Packet;

/// A live encoder, either a platform framework or a software codec.
pub trait Encoder: Send {
    /// The codec this encoder produces.
    fn codec(&self) -> Codec;

    /// How this encoder is doing its work.
    ///
    /// Check [`CodecPath::is_software`] to find out whether a software fallback
    /// was used, and [`CodecPath::fallback_reason`] to find out why.
    fn path(&self) -> CodecPath;

    /// Submits one raw frame.
    fn push(&mut self, frame: Frame) -> Result<()>;

    /// Drains one finished packet, or `None` when more input is required.
    fn receive(&mut self) -> Result<Option<Packet>>;

    /// Signals end of stream and returns every remaining packet.
    fn flush(&mut self) -> Result<Vec<Packet>>;

    /// Changes the target bitrate on the fly. Not every backend supports it.
    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        let _ = bitrate;
        Err(Error::unsupported(format!("{} set_bitrate", self.codec())))
    }

    /// Asks the backend for a keyframe as soon as possible.
    fn request_keyframe(&mut self) -> Result<()> {
        Err(Error::unsupported(format!("{} request_keyframe", self.codec())))
    }

    /// Codec configuration bytes to be stored next to the stream, if any
    /// (H.264 `avcC`, AAC `AudioSpecificConfig`, ...).
    fn extradata(&self) -> Option<Buffer> {
        None
    }
}

/// A live decoder; the push/pull contract matches [`Encoder`].
pub trait Decoder: Send {
    /// The codec this decoder consumes.
    fn codec(&self) -> Codec;

    /// How this decoder is doing its work.
    fn path(&self) -> CodecPath;

    /// Submits one compressed packet, a header packet, or an end-of-stream marker.
    fn push(&mut self, packet: Packet) -> Result<()>;

    /// Drains one finished frame, or `None` when more input is required.
    fn receive(&mut self) -> Result<Option<Frame>>;

    /// Signals end of stream and returns every remaining frame.
    fn flush(&mut self) -> Result<Vec<Frame>>;
}

// ---------------------------------------------------------------------------
// Blanket forwarding implementations
// ---------------------------------------------------------------------------
//
// These let a `Box<dyn Encoder>` behave as an `Encoder`. `Registry` relies on
// that: when it has to stamp a `fallback_reason` onto a live session it wraps it
// in an adapter, and the wrapped value is already a boxed trait object.

impl<E: Encoder + ?Sized> Encoder for Box<E> {
    fn codec(&self) -> Codec {
        (**self).codec()
    }

    fn path(&self) -> CodecPath {
        (**self).path()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        (**self).push(frame)
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        (**self).receive()
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        (**self).flush()
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        (**self).set_bitrate(bitrate)
    }

    fn request_keyframe(&mut self) -> Result<()> {
        (**self).request_keyframe()
    }

    fn extradata(&self) -> Option<Buffer> {
        (**self).extradata()
    }
}

impl<D: Decoder + ?Sized> Decoder for Box<D> {
    fn codec(&self) -> Codec {
        (**self).codec()
    }

    fn path(&self) -> CodecPath {
        (**self).path()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        (**self).push(packet)
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        (**self).receive()
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        (**self).flush()
    }
}
