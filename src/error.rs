use core::fmt;

/// Result type returned by this crate.
pub type Result<T> = core::result::Result<T, Error>;

/// The exact capacity the caller's request buffer needs.
///
/// This refusal is the crate's entire storage contract: the host grows its own
/// storage to `required` bytes and calls again, or asks for the requirement up
/// front with [`layered::requirements`](crate::layered::requirements).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityError {
    /// The minimum capacity required by the attempted operation.
    pub required: usize,
    /// The capacity available during the attempted operation.
    pub available: usize,
}

impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the request buffer needs {} bytes but has {}",
            self.required, self.available
        )
    }
}

impl core::error::Error for CapacityError {}

/// A plan that no Azure request can express.
///
/// Invalid use is never conflated with capacity: these are reported before the
/// encoder writes any byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidPlan {
    /// The object key is empty or exceeds Azure's character limit.
    Key,
    /// A bounded range is empty or reversed.
    Range,
    /// Azure cannot express this range form.
    UnsupportedRange,
    /// A metadata plan carries a byte range.
    RangedMetadata,
    /// The condition kind and value disagree, or the value is not a header value.
    Condition,
}

impl fmt::Display for InvalidPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key => f.write_str("invalid object key"),
            Self::Range => f.write_str("invalid byte range"),
            Self::UnsupportedRange => {
                f.write_str("Azure does not support Range: bytes=-N suffix requests")
            }
            Self::RangedMetadata => f.write_str("a metadata plan cannot carry a byte range"),
            Self::Condition => f.write_str("invalid GET condition"),
        }
    }
}

/// Failure while validating, encoding, or interpreting an Azure GET request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The endpoint is not an ASCII HTTP(S) origin.
    InvalidEndpoint,
    /// The container is empty or contains request-structural bytes.
    InvalidContainer,
    /// The bearer token cannot be represented as one HTTP header value.
    InvalidToken,
    /// The plan cannot be expressed as an Azure request.
    InvalidPlan(InvalidPlan),
    /// The caller's request buffer is too small.
    Capacity(CapacityError),
    /// Azure reported that the object does not exist.
    NotFound,
    /// Azure rejected the bearer token.
    Unauthorized,
    /// An `If-Match` condition did not hold.
    Precondition,
    /// An `If-None-Match` condition did not hold.
    NotModified,
    /// Azure could not satisfy the requested byte range.
    RangeNotSatisfiable,
    /// A successful response omitted or malformed required metadata.
    Protocol(&'static str),
    /// Azure returned another non-success status.
    Status(u16),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => f.write_str("invalid Azure Blob endpoint"),
            Self::InvalidContainer => f.write_str("invalid Azure container name"),
            Self::InvalidToken => f.write_str("invalid bearer token"),
            Self::InvalidPlan(plan) => fmt::Display::fmt(plan, f),
            Self::Capacity(error) => fmt::Display::fmt(error, f),
            Self::NotFound => f.write_str("object not found"),
            Self::Unauthorized => f.write_str("Azure rejected the bearer token"),
            Self::Precondition => f.write_str("precondition failed"),
            Self::NotModified => f.write_str("not modified"),
            Self::RangeNotSatisfiable => f.write_str("range not satisfiable"),
            Self::Protocol(detail) => write!(f, "invalid Azure response: {detail}"),
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

impl From<InvalidPlan> for Error {
    fn from(value: InvalidPlan) -> Self {
        Self::InvalidPlan(value)
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
