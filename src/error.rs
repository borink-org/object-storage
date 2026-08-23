use core::fmt;

use crate::CapacityError;

// TODO(doc-review): Public API rustdoc is an initial scaffold for manual review.

/// Result type returned by this crate.
pub type Result<T> = core::result::Result<T, Error>;

/// Failure while validating, building, or interpreting an Azure GET request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The endpoint is not an ASCII HTTP(S) origin.
    InvalidEndpoint,
    /// The container is empty or contains request-structural bytes.
    InvalidContainer,
    /// The bearer token cannot be represented as one HTTP header value.
    InvalidToken,
    /// The object key is empty or exceeds Azure's character limit.
    InvalidKey,
    /// A caller-provided extent is too small.
    Capacity(CapacityError),
    /// Azure reported that the object does not exist.
    NotFound,
    /// Azure rejected the bearer token.
    Unauthorized,
    /// Azure returned another non-success status.
    Status(u16),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => f.write_str("invalid Azure Blob endpoint"),
            Self::InvalidContainer => f.write_str("invalid Azure container name"),
            Self::InvalidToken => f.write_str("invalid bearer token"),
            Self::InvalidKey => f.write_str("invalid object key"),
            Self::Capacity(error) => fmt::Display::fmt(error, f),
            Self::NotFound => f.write_str("object not found"),
            Self::Unauthorized => f.write_str("Azure rejected the bearer token"),
            Self::Status(status) => write!(f, "Azure returned HTTP {status}"),
        }
    }
}

impl core::error::Error for Error {}

impl From<CapacityError> for Error {
    fn from(value: CapacityError) -> Self {
        Self::Capacity(value)
    }
}

impl Error {
    /// Returns structured capacity information when this is a capacity error.
    pub fn capacity(&self) -> Option<CapacityError> {
        match *self {
            Self::Capacity(error) => Some(error),
            _ => None,
        }
    }
}
