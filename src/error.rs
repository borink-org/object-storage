use core::fmt;

/// The result type that this crate returns.
pub type Result<T> = core::result::Result<T, Error>;

/// The exact capacity that your request buffer needs.
///
/// Grow the buffer to `required` bytes and call the same method again. To
/// learn the requirement before the first call, use
/// [`layered::get_requirements`](crate::layered::get_requirements).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityError {
    /// The smallest buffer that the call accepts, in bytes.
    pub required: usize,
    /// The size of the buffer that you supplied, in bytes.
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

/// The reason that a plan cannot become a request.
///
/// The encoding methods report these before they write any byte, and never
/// confuse them with a capacity error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidPlan {
    /// The object key is empty, or it is longer than the service allows.
    Key,
    /// A bounded range is empty, or its end is before its start.
    Range,
    /// The service does not accept this form of range.
    UnsupportedRange,
    /// A metadata plan carries a byte range, which the service cannot answer.
    RangedMetadata,
    /// The condition kind and the condition value do not agree.
    ///
    /// A kind without a value, and a value without a kind, are both invalid.
    /// The value must also be usable as one HTTP header value.
    Condition,
    /// The content is longer than the service writes in one request.
    PayloadTooLarge,
}

impl fmt::Display for InvalidPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key => f.write_str("invalid object key"),
            Self::Range => f.write_str("invalid byte range"),
            Self::UnsupportedRange => {
                f.write_str("the service does not support Range: bytes=-N suffix requests")
            }
            Self::RangedMetadata => f.write_str("a metadata plan cannot carry a byte range"),
            Self::Condition => f.write_str("invalid condition"),
            Self::PayloadTooLarge => f.write_str("the content is too long to write in one request"),
        }
    }
}

/// A failure to validate, to encode, or to read a request.
///
/// This type reports only your own mistakes and invalid responses. A response
/// that the service sends in normal operation, such as a missing object or a
/// failed precondition, is not an error here. It is a
/// [`GetHeadOutcome`](crate::GetHeadOutcome) or a
/// [`PutHeadOutcome`](crate::PutHeadOutcome) instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The endpoint is not an ASCII HTTP or HTTPS origin.
    InvalidEndpoint,
    /// The container name is empty, or it contains bytes that would change the
    /// structure of the request.
    InvalidContainer,
    /// The bearer token is not usable as one HTTP header value.
    InvalidToken,
    /// The plan cannot become a request.
    InvalidPlan(InvalidPlan),
    /// Your request buffer is too small.
    Capacity(CapacityError),
    /// The response head is invalid, or it contradicts itself.
    Protocol(&'static str),
    /// The response head does not answer the plan that you passed in.
    ResponseMismatch(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => f.write_str("invalid endpoint"),
            Self::InvalidContainer => f.write_str("invalid container name"),
            Self::InvalidToken => f.write_str("invalid bearer token"),
            Self::InvalidPlan(plan) => fmt::Display::fmt(plan, f),
            Self::Capacity(error) => fmt::Display::fmt(error, f),
            Self::Protocol(detail) => write!(f, "invalid response: {detail}"),
            Self::ResponseMismatch(detail) => {
                write!(f, "the response does not answer the plan: {detail}")
            }
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
    /// Returns the capacity details, if this is a capacity error.
    pub fn capacity(&self) -> Option<CapacityError> {
        match *self {
            Self::Capacity(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    // Every fallible call in this crate pays for this size. Clippy warns at
    // 128 bytes; a field added later that crosses the line shows up here.
    #[test]
    fn a_result_stays_small() {
        assert!(size_of::<Result<(), Error>>() <= 128);
    }
}
