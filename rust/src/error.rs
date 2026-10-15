//! Error type shared by every AnyCodec backend.
//!
//! The variants are deliberately coarse: the FFI layer maps [`ErrorKind`] onto a
//! stable `i32` status code, so adding a new *message* is free while adding a new
//! *kind* is an ABI change.

use std::fmt;

/// Stable, ABI-visible classification of a failure.
///
/// The numeric values are part of the C ABI (`AnyCodecStatus`); never renumber
/// an existing variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// A caller-supplied argument was malformed (NULL, out of range, ...).
    InvalidArgument,
    /// The requested codec/operation is not implemented by this build.
    Unsupported,
    /// The operation is implemented but no backend is currently usable
    /// (missing hardware, missing system codec, missing shared library).
    Unavailable,
    /// The underlying platform API reported a failure.
    Backend,
    /// The object is not in a state that permits the call.
    State,
    /// Memory allocation failed.
    OutOfMemory,
    /// Reading/writing a file or stream failed.
    Io,
    /// An internal invariant was violated. This is a bug.
    Internal,
}

impl ErrorKind {
    /// The `i32` status code exposed through the C ABI.
    pub const fn status(self) -> i32 {
        match self {
            ErrorKind::InvalidArgument => -1,
            ErrorKind::Unsupported => -2,
            ErrorKind::Unavailable => -3,
            ErrorKind::Backend => -4,
            ErrorKind::State => -5,
            ErrorKind::OutOfMemory => -6,
            ErrorKind::Io => -7,
            ErrorKind::Internal => -8,
        }
    }

    /// Short stable token, handy for logs and for `errors.json` style reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorKind::InvalidArgument => "invalid_argument",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Unavailable => "unavailable",
            ErrorKind::Backend => "backend",
            ErrorKind::State => "state",
            ErrorKind::OutOfMemory => "out_of_memory",
            ErrorKind::Io => "io",
            ErrorKind::Internal => "internal",
        }
    }
}

/// An AnyCodec failure: a kind, a human readable message and optional context.
#[derive(Clone, Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    backend: Option<&'static str>,
}

impl Error {
    /// Builds an error with an explicit kind.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), backend: None }
    }

    /// Tags the error with the backend that produced it.
    #[must_use]
    pub fn with_backend(mut self, backend: &'static str) -> Self {
        self.backend = Some(backend);
        self
    }

    /// `ErrorKind::InvalidArgument`.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, message)
    }

    /// `ErrorKind::Unsupported`.
    pub fn unsupported(what: impl fmt::Display) -> Self {
        Self::new(ErrorKind::Unsupported, format!("{what} is not supported by this build"))
    }

    /// `ErrorKind::Unavailable`.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }

    /// `ErrorKind::Backend`.
    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Backend, message)
    }

    /// `ErrorKind::State`.
    pub fn state(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::State, message)
    }

    /// `ErrorKind::Internal`.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    /// The failure classification.
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The `i32` status code exposed through the C ABI.
    pub const fn status(&self) -> i32 {
        self.kind.status()
    }

    /// The human readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The backend that produced the failure, if it was tagged.
    pub const fn backend_name(&self) -> Option<&'static str> {
        self.backend
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.backend {
            Some(b) => write!(f, "[{}] {}: {}", self.kind.as_str(), b, self.message),
            None => write!(f, "[{}] {}", self.kind.as_str(), self.message),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Error::new(ErrorKind::Io, value.to_string())
    }
}

impl From<std::num::TryFromIntError> for Error {
    fn from(value: std::num::TryFromIntError) -> Self {
        Error::invalid_argument(format!("integer out of range: {value}"))
    }
}

/// Convenience alias used throughout the workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// Ensures a condition holds, otherwise returns an [`Error::invalid_argument`].
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if ::core::ops::Not::not($cond) {
            return ::core::result::Result::Err($crate::error::Error::invalid_argument(
                ::std::format!($($arg)*),
            ));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_are_stable() {
        assert_eq!(ErrorKind::InvalidArgument.status(), -1);
        assert_eq!(ErrorKind::Unsupported.status(), -2);
        assert_eq!(ErrorKind::Unavailable.status(), -3);
        assert_eq!(ErrorKind::Backend.status(), -4);
        assert_eq!(ErrorKind::State.status(), -5);
        assert_eq!(ErrorKind::OutOfMemory.status(), -6);
        assert_eq!(ErrorKind::Io.status(), -7);
        assert_eq!(ErrorKind::Internal.status(), -8);
    }

    #[test]
    fn backend_tag_is_displayed() {
        let err = Error::unavailable("no HEVC decoder").with_backend("mediafoundation");
        assert!(err.to_string().contains("mediafoundation"));
        assert_eq!(err.kind(), ErrorKind::Unavailable);
    }
}
