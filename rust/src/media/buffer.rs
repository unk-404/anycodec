//! Reference-counted immutable byte storage: the bottom of the memory-safety
//! story.
//!
//! Every buffer handed between AnyCodec layers is an [`Buffer`]. It is
//! immutable, cheap to clone (an atomic increment) and never aliased mutably, so
//! a frame handed to an encoder can be retained by the backend for as long as it
//! needs without any lifetime gymnastics in the C ABI.

use std::fmt;
use std::ops::{Deref, Range};
use std::sync::Arc;

use crate::error::{Error, Result};

/// An immutable, reference-counted byte buffer.
#[derive(Clone, PartialEq, Eq)]
pub struct Buffer {
    inner: Arc<[u8]>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    /// An empty buffer.
    pub fn new() -> Self {
        Self { inner: Arc::from(Vec::<u8>::new().into_boxed_slice()) }
    }

    /// Copies `bytes` into a fresh buffer.
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self { inner: Arc::from(bytes.to_vec().into_boxed_slice()) }
    }

    /// Takes ownership of `bytes` without copying.
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self { inner: Arc::from(bytes.into_boxed_slice()) }
    }

    /// Number of bytes.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when there is not a single byte.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.inner
    }

    /// Pointer to the first byte. Stable for the lifetime of this clone.
    pub fn as_ptr(&self) -> *const u8 {
        self.inner.as_ptr()
    }

    /// A copy of `range`, or an error when the range is out of bounds.
    pub fn slice(&self, range: Range<usize>) -> Result<Buffer> {
        if range.start > range.end || range.end > self.len() {
            return Err(Error::invalid_argument(format!(
                "buffer slice {range:?} out of bounds for {} bytes",
                self.len()
            )));
        }
        Ok(Buffer::from_slice(&self.inner[range]))
    }

    /// A copy of the contents as a `Vec`.
    pub fn to_vec(&self) -> Vec<u8> {
        self.inner.to_vec()
    }

    /// Number of live references (including the global empty buffer).
    pub fn ref_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
}

impl Deref for Buffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl AsRef<[u8]> for Buffer {
    fn as_ref(&self) -> &[u8] {
        &self.inner
    }
}

impl From<Vec<u8>> for Buffer {
    fn from(value: Vec<u8>) -> Self {
        Buffer::from_vec(value)
    }
}

impl From<&[u8]> for Buffer {
    fn from(value: &[u8]) -> Self {
        Buffer::from_slice(value)
    }
}

impl From<Box<[u8]>> for Buffer {
    fn from(value: Box<[u8]>) -> Self {
        Self { inner: Arc::from(value) }
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Buffer({} bytes, refs={})", self.len(), self.ref_count())
    }
}

/// Growable builder used by the FFI layer to assemble one buffer from several
/// caller-supplied chunks without ever exposing a mutable alias into Rust.
#[derive(Default, Debug)]
pub struct BufferBuilder {
    data: Vec<u8>,
}

impl BufferBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }

    /// A builder that can hold at least `capacity` bytes without reallocating.
    pub fn with_capacity(capacity: usize) -> Self {
        Self { data: Vec::with_capacity(capacity) }
    }

    /// Number of bytes appended so far.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// `true` when nothing has been appended.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Appends `bytes`.
    pub fn push(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
    }

    /// Appends `count` copies of `value`.
    pub fn push_repeat(&mut self, value: u8, count: usize) {
        self.data.resize(self.data.len() + count, value);
    }

    /// Freezes the builder into an immutable [`Buffer`].
    pub fn finish(self) -> Buffer {
        Buffer::from_vec(self.data)
    }
}

impl fmt::Write for BufferBuilder {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloning_shares_storage() {
        let a = Buffer::from_slice(b"hello");
        let b = a.clone();
        assert_eq!(a.ref_count(), 2);
        assert_eq!(b.as_slice(), b"hello");
        assert!(std::ptr::eq(a.as_ptr(), b.as_ptr()));
    }

    #[test]
    fn slicing_rejects_out_of_bounds() {
        let a = Buffer::from_slice(b"hello");
        assert_eq!(a.slice(1..3).unwrap().as_slice(), b"el");
        assert!(a.slice(0..6).is_err(), "past the end");
        // A reversed range is exactly what is being rejected here, so the lint
        // that flags it has nothing to say.
        #[allow(clippy::reversed_empty_ranges)]
        let reversed = 4..2;
        assert!(a.slice(reversed).is_err(), "start after end");
    }

    #[test]
    fn builder_concatenates() {
        let mut builder = BufferBuilder::with_capacity(8);
        builder.push(b"ab");
        builder.push(b"cd");
        builder.push_repeat(0, 2);
        assert_eq!(builder.finish().as_slice(), b"abcd\0\0");
    }

    #[test]
    fn empty_buffer_is_cheap() {
        let buffer = Buffer::new();
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }
}
