//! Layer 1 — the memory-safe media model.
//!
//! Everything in this module is an *owned value*: no raw pointers, no lifetimes,
//! no `unsafe`. Buffers are immutable and reference-counted, which is what lets
//! the C ABI above hand a frame to a backend and forget about it.
//!
//! The module tree mirrors the media itself:
//!
//! * [`codec`] — *what* is being compressed ([`Codec`], [`VideoCodec`], [`AudioCodec`]).
//! * [`format`] — *how* raw data is laid out ([`PixelFormat`], [`SampleFormat`], [`ChannelLayout`]).
//! * [`buffer`] — the byte storage everything else is built from ([`Buffer`]).
//! * [`frame`] — uncompressed input/output ([`Frame`], [`VideoFrame`], [`AudioFrame`]).
//! * [`packet`] — compressed input/output ([`Packet`]).
//! * [`config`] — how to configure an encoder or decoder ([`EncoderConfig`], [`DecoderConfig`]).

pub mod buffer;
pub mod codec;
pub mod config;
pub mod format;
pub mod frame;
pub mod packet;

pub use buffer::{Buffer, BufferBuilder};
pub use codec::{AudioCodec, Codec, MediaKind, VideoCodec};
pub use config::{DecoderConfig, EncoderConfig, Preset};
pub use format::{ChannelLayout, PixelFormat, SampleFormat};
pub use frame::{AudioFrame, Frame, Plane, VideoFrame};
pub use packet::Packet;
