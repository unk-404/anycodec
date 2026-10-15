//! Compressed packets.

use crate::media::buffer::Buffer;
use crate::media::codec::Codec;

/// Bit flags attached to a [`Packet`].
///
/// `pub(crate)` rather than `pub`: cbindgen emits every public constant in the
/// crate, and a bare `KEYFRAME` in the C header next to
/// `ANYCODEC_PACKET_FLAG_KEYFRAME` is exactly the kind of ambiguity the ABI
/// should not have. The ABI spelling lives in `crate::ffi::types`.
///
/// `HARDWARE` and `CORRUPT` are currently never set by a backend; they are part
/// of the vocabulary the flag mask is defined in.
#[allow(dead_code)]
pub(crate) mod flags {
    /// The packet can be decoded without any earlier packet.
    pub(crate) const KEYFRAME: u32 = 1 << 0;
    /// The packet carries codec extradata (decoder configuration) rather than a picture.
    pub(crate) const HEADER: u32 = 1 << 1;
    /// The packet marks the end of the stream.
    pub(crate) const EOS: u32 = 1 << 2;
    /// The packet was produced by a hardware accelerated pipeline.
    pub(crate) const HARDWARE: u32 = 1 << 3;
    /// The packet is known to be corrupt / partially decoded.
    pub(crate) const CORRUPT: u32 = 1 << 4;
}

/// One compressed unit of a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// The codec that produced the payload.
    pub codec: Codec,
    /// The compressed bytes.
    pub data: Buffer,
    /// Presentation timestamp in the stream timebase.
    pub pts: i64,
    /// Decode timestamp in the stream timebase.
    pub dts: i64,
    /// Duration in the stream timebase, `0` when unknown.
    pub duration: i64,
    /// A combination of the [`flags`] constants.
    pub flags: u32,
}

impl Packet {
    /// Builds a packet with `dts == pts` and no duration.
    pub fn new(codec: Codec, data: Buffer, pts: i64, flags: u32) -> Self {
        Self { codec, data, pts, dts: pts, duration: 0, flags }
    }

    /// Builds a packet that only carries decoder configuration.
    pub fn header(codec: Codec, data: Buffer) -> Self {
        Self::new(codec, data, 0, flags::HEADER)
    }

    /// A zero-length packet marking end of stream.
    pub fn eos(codec: Codec) -> Self {
        Self::new(codec, Buffer::new(), 0, flags::EOS)
    }

    /// Sets the decode timestamp.
    #[must_use]
    pub fn with_dts(mut self, dts: i64) -> Self {
        self.dts = dts;
        self
    }

    /// Sets the duration.
    #[must_use]
    pub fn with_duration(mut self, duration: i64) -> Self {
        self.duration = duration;
        self
    }

    /// Adds flags.
    #[must_use]
    pub fn with_flags(mut self, flags: u32) -> Self {
        self.flags |= flags;
        self
    }

    /// `true` when [`flags::KEYFRAME`] is set.
    pub fn is_keyframe(&self) -> bool {
        self.flags & flags::KEYFRAME != 0
    }

    /// `true` when [`flags::HEADER`] is set.
    pub fn is_header(&self) -> bool {
        self.flags & flags::HEADER != 0
    }

    /// `true` when [`flags::EOS`] is set.
    pub fn is_eos(&self) -> bool {
        self.flags & flags::EOS != 0
    }

    /// Number of compressed bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// `true` when the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::codec::VideoCodec;

    #[test]
    fn keyframe_flag_is_visible() {
        let packet = Packet::new(
            Codec::Video(VideoCodec::H264),
            Buffer::from_slice(&[0, 0, 0, 1]),
            7,
            flags::KEYFRAME,
        );
        assert!(packet.is_keyframe());
        assert!(!packet.is_header());
        assert_eq!(packet.dts, 7);
    }

    #[test]
    fn header_packets_are_empty() {
        let packet = Packet::header(Codec::Audio(crate::media::codec::AudioCodec::Aac), Buffer::new());
        assert!(packet.is_header());
        assert!(packet.is_empty());
    }
}
