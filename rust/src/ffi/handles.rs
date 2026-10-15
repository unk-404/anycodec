//! Opaque, caller-owned handles: the "class" layer of the C ABI.
//!
//! Each type here has **no public fields**, which is what makes cbindgen emit it
//! as an incomplete type (`typedef struct AnyCodecEncoder AnyCodecEncoder;`). The
//! C side can only hold the pointer and pass it back, so it is structurally
//! impossible for a caller to read or corrupt Rust memory through these handles.

use crate::engine::device::HwDeviceInfo;
use crate::engine::{Decoder, Encoder};
use crate::media::buffer::Buffer;
use crate::media::frame::Frame;
use crate::media::packet::Packet;

/// A live encoder session.
///
/// `pending` buffers whatever `Encoder::flush` produced so that the C-side drain
/// loop (`anycodec_encoder_receive`) can deliver it in order: the trait returns
/// the tail synchronously, while the C ABI is pull-only.
pub struct AnyCodecEncoder {
    pub(crate) inner: Box<dyn Encoder>,
    pub(crate) pending: std::collections::VecDeque<Packet>,
}

impl std::fmt::Debug for AnyCodecEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyCodecEncoder")
            .field("codec", &self.inner.codec().name())
            .field("path", &self.inner.path().to_string())
            .field("pending", &self.pending.len())
            .finish()
    }
}

/// A live decoder session; `pending` mirrors [`AnyCodecEncoder::pending`].
pub struct AnyCodecDecoder {
    pub(crate) inner: Box<dyn Decoder>,
    pub(crate) pending: std::collections::VecDeque<Frame>,
}

impl std::fmt::Debug for AnyCodecDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyCodecDecoder")
            .field("codec", &self.inner.codec().name())
            .field("path", &self.inner.path().to_string())
            .field("pending", &self.pending.len())
            .finish()
    }
}

/// A compressed packet.
pub struct AnyCodecPacket {
    pub(crate) inner: Packet,
}

impl std::fmt::Debug for AnyCodecPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyCodecPacket")
            .field("codec", &self.inner.codec.name())
            .field("len", &self.inner.len())
            .field("pts", &self.inner.pts)
            .field("flags", &self.inner.flags)
            .finish()
    }
}

/// A raw frame.
pub struct AnyCodecFrame {
    pub(crate) inner: Frame,
}

impl std::fmt::Debug for AnyCodecFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Frame::Video(video) => f
                .debug_struct("AnyCodecFrame::Video")
                .field("format", &video.format.name())
                .field("size", &(video.width, video.height))
                .field("planes", &video.planes.len())
                .field("pts", &video.pts)
                .finish(),
            Frame::Audio(audio) => f
                .debug_struct("AnyCodecFrame::Audio")
                .field("format", &audio.format.name())
                .field("rate", &audio.sample_rate)
                .field("channels", &audio.channels.name())
                .field("frames", &audio.frames)
                .field("pts", &audio.pts)
                .finish(),
        }
    }
}

/// An owned byte buffer.
pub struct AnyCodecBuffer {
    pub(crate) inner: Buffer,
}

impl std::fmt::Debug for AnyCodecBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyCodecBuffer").field("len", &self.inner.len()).finish()
    }
}

/// A snapshot of the machine's hardware media engines.
pub struct AnyCodecDeviceList {
    pub(crate) devices: Vec<HwDeviceInfo>,
}

impl std::fmt::Debug for AnyCodecDeviceList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyCodecDeviceList").field("len", &self.devices.len()).finish()
    }
}

/// Allocates an [`AnyCodecEncoder`] on the heap and hands ownership to the caller.
pub(crate) fn into_encoder_handle(inner: Box<dyn Encoder>) -> *mut AnyCodecEncoder {
    Box::into_raw(Box::new(AnyCodecEncoder { inner, pending: std::collections::VecDeque::new() }))
}

/// Reclaims an [`AnyCodecEncoder`] previously produced by
/// [`into_encoder_handle`].
///
/// # Safety
///
/// `pointer` must be NULL or a pointer returned by this module and not yet freed.
pub(crate) unsafe fn drop_encoder_handle(pointer: *mut AnyCodecEncoder) {
    if !pointer.is_null() {
        // SAFETY: the caller guarantees the pointer came from `Box::into_raw` and
        // has not been freed yet.
        drop(unsafe { Box::from_raw(pointer) });
    }
}

/// Allocates an [`AnyCodecDecoder`] on the heap and hands ownership to the caller.
pub(crate) fn into_decoder_handle(inner: Box<dyn Decoder>) -> *mut AnyCodecDecoder {
    Box::into_raw(Box::new(AnyCodecDecoder { inner, pending: std::collections::VecDeque::new() }))
}

/// Reclaims an [`AnyCodecDecoder`].
///
/// # Safety
///
/// `pointer` must be NULL or a pointer returned by this module and not yet freed.
pub(crate) unsafe fn drop_decoder_handle(pointer: *mut AnyCodecDecoder) {
    if !pointer.is_null() {
        // SAFETY: as documented.
        drop(unsafe { Box::from_raw(pointer) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BackendKind;
    use crate::soft::builtin::passthrough::PassthroughEncoder;

    #[test]
    fn encoder_handles_round_trip() {
        let encoder = PassthroughEncoder::new(crate::media::codec::Codec::Video(
            crate::media::codec::VideoCodec::H264,
        ));
        let handle = into_encoder_handle(Box::new(encoder));
        assert!(!handle.is_null());
        // SAFETY: `handle` came from `into_encoder_handle` and is freed once.
        unsafe {
            assert_eq!(
                (*handle).inner.path().backend,
                BackendKind::Rust,
                "the passthrough encoder reports the Rust backend"
            );
            drop_encoder_handle(handle);
        }
    }

    #[test]
    fn dropping_a_null_handle_is_a_no_op() {
        // SAFETY: NULL is explicitly allowed.
        unsafe { drop_encoder_handle(std::ptr::null_mut()) };
    }
}
