use core::fmt;

use crate::CapacityError;

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
    /// An ETag condition cannot be represented as one HTTP header value.
    InvalidCondition,
    /// A bounded range is empty or reversed.
    InvalidRange,
    /// Azure cannot represent the requested operation.
    Unsupported(&'static str),
    /// A caller-provided extent is too small.
    Capacity(CapacityError),
    /// A non-success response classified from Azure status and error metadata.
    Azure(AzureError),
    /// A successful response omitted or malformed required metadata.
    Protocol(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => f.write_str("invalid Azure Blob endpoint"),
            Self::InvalidContainer => f.write_str("invalid Azure container name"),
            Self::InvalidToken => f.write_str("invalid bearer token"),
            Self::InvalidKey => f.write_str("invalid object key"),
            Self::InvalidCondition => f.write_str("invalid GET condition"),
            Self::InvalidRange => f.write_str("invalid byte range"),
            Self::Unsupported(operation) => write!(f, "{operation} is not supported"),
            Self::Capacity(error) => fmt::Display::fmt(error, f),
            Self::Azure(error) => fmt::Display::fmt(error, f),
            Self::Protocol(detail) => write!(f, "invalid Azure response: {detail}"),
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

/// Stable classification of an Azure service error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AzureErrorKind {
    /// The object does not exist.
    NotFound,
    /// The container does not exist.
    NoSuchContainer,
    /// The target already exists.
    AlreadyExists,
    /// Azure rejected the credentials or authorization.
    Unauthorized,
    /// A request precondition did not hold.
    Precondition,
    /// The object was not modified.
    NotModified,
    /// Azure could not satisfy the requested byte range.
    RangeNotSatisfiable,
    /// Azure throttled the request.
    Throttled,
    /// Azure timed out while processing the request.
    Timeout,
    /// Azure reported an internal or unavailable service.
    Service,
    /// The response did not map to a known classification.
    Unrecognized,
}

impl fmt::Display for AzureErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("object not found"),
            Self::NoSuchContainer => f.write_str("container not found"),
            Self::AlreadyExists => f.write_str("object already exists"),
            Self::Unauthorized => f.write_str("Azure rejected the bearer token"),
            Self::Precondition => f.write_str("precondition failed"),
            Self::NotModified => f.write_str("not modified"),
            Self::RangeNotSatisfiable => f.write_str("range not satisfiable"),
            Self::Throttled => f.write_str("Azure throttled the request"),
            Self::Timeout => f.write_str("Azure timed out"),
            Self::Service => f.write_str("Azure service error"),
            Self::Unrecognized => f.write_str("unrecognized Azure error"),
        }
    }
}

/// Azure request identifier copied into fixed storage.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RequestId {
    bytes: [u8; 40],
    len: u8,
}

impl RequestId {
    pub(crate) fn new(value: &str) -> Self {
        let mut end = value.len().min(40);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0; 40];
        bytes[..end].copy_from_slice(&value.as_bytes()[..end]);
        Self {
            bytes,
            len: end as u8,
        }
    }

    /// Returns the copied request identifier.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).expect("copied from a string")
    }
}

impl fmt::Debug for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Structured Azure service error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AzureError {
    kind: AzureErrorKind,
    status: u16,
    request_id: RequestId,
}

impl AzureError {
    pub(crate) fn new(kind: AzureErrorKind, status: u16, request_id: &str) -> Self {
        Self {
            kind,
            status,
            request_id: RequestId::new(request_id),
        }
    }

    /// Returns the stable error classification.
    pub fn kind(&self) -> AzureErrorKind {
        self.kind
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Returns Azure's copied request identifier when present.
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }
}

impl fmt::Display for AzureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (HTTP {})", self.kind, self.status)?;
        if !self.request_id.as_str().is_empty() {
            write!(f, ", request {}", self.request_id.as_str())?;
        }
        Ok(())
    }
}
