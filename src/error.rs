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
#[repr(u16)]
pub enum InvalidPlan {
    /// The object key is empty, or it is longer than the service allows.
    Key = 1,
    /// A bounded range is empty, or its end is before its start.
    Range = 2,
    /// The service does not accept this form of range.
    UnsupportedRange = 3,
    /// A metadata plan carries a byte range, which the service cannot answer.
    RangedMetadata = 4,
    /// The condition kind and the condition value do not agree.
    ///
    /// A kind without a value, and a value without a kind, are both invalid.
    /// The value must also be usable as one HTTP header value.
    Condition = 5,
    /// The content is longer than the service writes in one request.
    PayloadTooLarge = 6,
    /// A field of the plan holds a discriminant that this crate does not
    /// define.
    ///
    /// A plan built in another language carries each enum as its number. A
    /// number that names no value here is refused rather than read as the
    /// value that happens to be oldest.
    Unknown = 7,
}

impl InvalidPlan {
    /// Returns the sentence that [`Display`](fmt::Display) writes for this
    /// reason.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Key => "invalid object key",
            Self::Range => "invalid byte range",
            Self::UnsupportedRange => {
                "the service does not support Range: bytes=-N suffix requests"
            }
            Self::RangedMetadata => "a metadata plan cannot carry a byte range",
            Self::Condition => "invalid condition",
            Self::PayloadTooLarge => "the content is too long to write in one request",
            Self::Unknown => "the plan holds a value that this crate does not define",
        }
    }

    /// Returns the reason with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Key,
            2 => Self::Range,
            3 => Self::UnsupportedRange,
            4 => Self::RangedMetadata,
            5 => Self::Condition,
            6 => Self::PayloadTooLarge,
            7 => Self::Unknown,
            _ => return None,
        })
    }
}

impl fmt::Display for InvalidPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What is wrong with a response head.
///
/// Each of these is a head that Azure does not send. Some contradict
/// themselves, such as a length that disagrees with a range. Others carry a
/// status that the operation does not use. Neither has a meaning to read, so
/// this crate reports the head instead of choosing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum ProtocolFault {
    /// The `Content-Length` header is not a number.
    ContentLength = 1,
    /// The `Content-Range` header is malformed, or its arithmetic does not
    /// hold.
    ContentRange = 2,
    /// `Content-Length` and `Content-Range` state different lengths.
    ContentLengthDisagrees = 3,
    /// The head carries `Content-Range: bytes */N` outside a 416.
    UnsizedRangeOutside416 = 4,
    /// The head reports status 416 with no readable `Content-Range`.
    UnsatisfiedRangeHead = 5,
    /// A read was answered with a success status that it does not answer with.
    UnexpectedSuccess = 6,
    /// A write was answered with a success status other than 201.
    WriteNot201 = 7,
    /// A removal was answered with a success status other than 202.
    DeleteNot202 = 8,
    /// Status 304 answered a plan that carries no `If-None-Match` condition.
    NotModifiedWithoutCondition = 9,
    /// Status 412 answered a read that carries no `If-Match` condition.
    PreconditionWithoutCondition = 10,
}

impl ProtocolFault {
    /// Returns the sentence that [`Display`](fmt::Display) writes for this
    /// fault.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContentLength => "invalid content-length",
            Self::ContentRange => "invalid content-range",
            Self::ContentLengthDisagrees => "content-length disagrees with content-range",
            Self::UnsizedRangeOutside416 => "bytes */N is valid only in a 416",
            Self::UnsatisfiedRangeHead => "invalid 416 content-range",
            Self::UnexpectedSuccess => "unexpected success status",
            Self::WriteNot201 => "a write returns 201, not another success",
            Self::DeleteNot202 => "a removal returns 202, not another success",
            Self::NotModifiedWithoutCondition => {
                "304 answered a plan without an If-None-Match condition"
            }
            Self::PreconditionWithoutCondition => {
                "412 answered a plan without an If-Match condition"
            }
        }
    }

    /// Returns the fault with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::ContentLength,
            2 => Self::ContentRange,
            3 => Self::ContentLengthDisagrees,
            4 => Self::UnsizedRangeOutside416,
            5 => Self::UnsatisfiedRangeHead,
            6 => Self::UnexpectedSuccess,
            7 => Self::WriteNot201,
            8 => Self::DeleteNot202,
            9 => Self::NotModifiedWithoutCondition,
            10 => Self::PreconditionWithoutCondition,
            _ => return None,
        })
    }
}

impl fmt::Display for ProtocolFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a response head disagrees with the plan that it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum Mismatch {
    /// Status 206 answered a plan that requests no range.
    UnrangedAnsweredWith206 = 1,
    /// A status other than 206 answered a plan that requests a range.
    RangedAnsweredWithout206 = 2,
    /// Status 206 arrived without a `Content-Range` header.
    RangeWithoutContentRange = 3,
    /// The served range starts somewhere other than the plan asked.
    RangeStartsElsewhere = 4,
    /// The service served less than the satisfiable part of the range.
    RangeServedShort = 5,
    /// Status 412 answered a write that carries no condition.
    WriteWithoutCondition = 6,
    /// Status 412 answered a removal that carries no condition.
    DeleteWithoutCondition = 7,
}

impl Mismatch {
    /// Returns the sentence that [`Display`](fmt::Display) writes for this
    /// mismatch.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnrangedAnsweredWith206 => "an unranged plan was answered with 206",
            Self::RangedAnsweredWithout206 => "a ranged plan was answered without 206",
            Self::RangeWithoutContentRange => "206 without a content-range",
            Self::RangeStartsElsewhere => "the served range starts elsewhere",
            Self::RangeServedShort => "the service served less than the satisfiable range",
            Self::WriteWithoutCondition => "412 answered a write without a condition",
            Self::DeleteWithoutCondition => "412 answered a removal without a condition",
        }
    }

    /// Returns the mismatch with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::UnrangedAnsweredWith206,
            2 => Self::RangedAnsweredWithout206,
            3 => Self::RangeWithoutContentRange,
            4 => Self::RangeStartsElsewhere,
            5 => Self::RangeServedShort,
            6 => Self::WriteWithoutCondition,
            7 => Self::DeleteWithoutCondition,
            _ => return None,
        })
    }
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which kind of failure an [`Error`] is.
///
/// This is the discriminant of [`Error`] on its own, as a number that does not
/// change between versions. Read it with [`Error::code`], and read the
/// discriminant of the value inside with [`Error::detail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum ErrorCode {
    /// [`Error::InvalidEndpoint`].
    InvalidEndpoint = 1,
    /// [`Error::InvalidContainer`].
    InvalidContainer = 2,
    /// [`Error::InvalidToken`].
    InvalidToken = 3,
    /// [`Error::InvalidPlan`].
    InvalidPlan = 4,
    /// [`Error::Capacity`].
    Capacity = 5,
    /// [`Error::Protocol`].
    Protocol = 6,
    /// [`Error::ResponseMismatch`].
    ResponseMismatch = 7,
}

impl ErrorCode {
    /// Returns a sentence naming this kind of failure.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "invalid endpoint",
            Self::InvalidContainer => "invalid container name",
            Self::InvalidToken => "invalid bearer token",
            Self::InvalidPlan => "the plan cannot become a request",
            Self::Capacity => "the request buffer is too small",
            Self::Protocol => "invalid response",
            Self::ResponseMismatch => "the response does not answer the plan",
        }
    }

    /// Returns the code with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::InvalidEndpoint,
            2 => Self::InvalidContainer,
            3 => Self::InvalidToken,
            4 => Self::InvalidPlan,
            5 => Self::Capacity,
            6 => Self::Protocol,
            7 => Self::ResponseMismatch,
            _ => return None,
        })
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failure to validate, to encode, or to read a request.
///
/// This type reports only your own mistakes and invalid responses. A response
/// that the service sends in normal operation, such as a missing object or a
/// failed precondition, is not an error here. It is a
/// [`GetHeadOutcome`](crate::GetHeadOutcome) or a
/// [`PutHeadOutcome`](crate::PutHeadOutcome) instead.
///
/// No value of this type carries text. [`Error::code`] and [`Error::detail`]
/// describe every value as two numbers, so you can carry an error across a
/// boundary that takes no strings.
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
    Protocol(ProtocolFault),
    /// The response head does not answer the plan that you passed in.
    ResponseMismatch(Mismatch),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint | Self::InvalidContainer | Self::InvalidToken => {
                f.write_str(self.code().as_str())
            }
            Self::InvalidPlan(plan) => fmt::Display::fmt(plan, f),
            Self::Capacity(error) => fmt::Display::fmt(error, f),
            Self::Protocol(fault) => write!(f, "invalid response: {fault}"),
            Self::ResponseMismatch(mismatch) => {
                write!(f, "the response does not answer the plan: {mismatch}")
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

impl From<ProtocolFault> for Error {
    fn from(value: ProtocolFault) -> Self {
        Self::Protocol(value)
    }
}

impl From<Mismatch> for Error {
    fn from(value: Mismatch) -> Self {
        Self::ResponseMismatch(value)
    }
}

impl Error {
    /// Returns the capacity details, if this is a capacity error.
    pub const fn capacity(&self) -> Option<CapacityError> {
        match *self {
            Self::Capacity(error) => Some(error),
            _ => None,
        }
    }

    /// Returns which kind of failure this is.
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidEndpoint => ErrorCode::InvalidEndpoint,
            Self::InvalidContainer => ErrorCode::InvalidContainer,
            Self::InvalidToken => ErrorCode::InvalidToken,
            Self::InvalidPlan(_) => ErrorCode::InvalidPlan,
            Self::Capacity(_) => ErrorCode::Capacity,
            Self::Protocol(_) => ErrorCode::Protocol,
            Self::ResponseMismatch(_) => ErrorCode::ResponseMismatch,
        }
    }

    /// Returns the discriminant of the value inside, or 0 if there is none.
    ///
    /// [`Self::Capacity`] carries two sizes rather than a discriminant, and
    /// returns 0 here. Read those sizes with [`Self::capacity`].
    pub const fn detail(&self) -> u16 {
        match *self {
            Self::InvalidPlan(plan) => plan as u16,
            Self::Protocol(fault) => fault as u16,
            Self::ResponseMismatch(mismatch) => mismatch as u16,
            _ => 0,
        }
    }

    /// Rebuilds an error from a [`Self::code`] and a [`Self::detail`].
    ///
    /// Returns [`None`] if `detail` is not a discriminant that `code` defines,
    /// and for [`ErrorCode::Capacity`], whose two sizes this pair does not
    /// carry. A code that carries no value inside accepts only a `detail` of
    /// 0.
    pub const fn from_parts(code: ErrorCode, detail: u16) -> Option<Self> {
        Some(match code {
            ErrorCode::InvalidEndpoint if detail == 0 => Self::InvalidEndpoint,
            ErrorCode::InvalidContainer if detail == 0 => Self::InvalidContainer,
            ErrorCode::InvalidToken if detail == 0 => Self::InvalidToken,
            ErrorCode::InvalidEndpoint | ErrorCode::InvalidContainer | ErrorCode::InvalidToken => {
                return None;
            }
            ErrorCode::InvalidPlan => match InvalidPlan::from_discriminant(detail) {
                Some(plan) => Self::InvalidPlan(plan),
                None => return None,
            },
            ErrorCode::Protocol => match ProtocolFault::from_discriminant(detail) {
                Some(fault) => Self::Protocol(fault),
                None => return None,
            },
            ErrorCode::ResponseMismatch => match Mismatch::from_discriminant(detail) {
                Some(mismatch) => Self::ResponseMismatch(mismatch),
                None => return None,
            },
            ErrorCode::Capacity => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{CapacityError, Error, ErrorCode, InvalidPlan, Mismatch, ProtocolFault};
    use std::string::ToString;
    use std::vec::Vec;

    // Every fallible call in this crate pays for this size. Two `usize`
    // capacity fields set it, so a field added later shows up here.
    #[test]
    fn a_result_stays_small() {
        assert_eq!(size_of::<Error>(), 3 * size_of::<usize>());
        assert!(size_of::<Result<(), Error>>() <= 4 * size_of::<usize>());
    }

    // Every error except a capacity error, which carries sizes rather than a
    // discriminant.
    fn every_error() -> Vec<Error> {
        let mut errors = std::vec![
            Error::InvalidEndpoint,
            Error::InvalidContainer,
            Error::InvalidToken,
        ];
        for detail in 1..=u16::MAX {
            if let Some(plan) = InvalidPlan::from_discriminant(detail) {
                errors.push(Error::InvalidPlan(plan));
            }
            if let Some(fault) = ProtocolFault::from_discriminant(detail) {
                errors.push(Error::Protocol(fault));
            }
            if let Some(mismatch) = Mismatch::from_discriminant(detail) {
                errors.push(Error::ResponseMismatch(mismatch));
            }
        }
        errors
    }

    #[test]
    fn two_numbers_describe_every_error() {
        for error in every_error() {
            let rebuilt = Error::from_parts(error.code(), error.detail());
            assert_eq!(rebuilt, Some(error), "{error:?}");
            assert_eq!(rebuilt.unwrap().to_string(), error.to_string(), "{error:?}");
            assert_ne!(error.code() as u16, 0, "{error:?}");
        }
    }

    #[test]
    fn a_capacity_error_carries_its_sizes_instead_of_a_discriminant() {
        let error = Error::Capacity(CapacityError {
            required: 96,
            available: 64,
        });
        assert_eq!(error.code(), ErrorCode::Capacity);
        assert_eq!(error.detail(), 0);
        assert_eq!(Error::from_parts(ErrorCode::Capacity, 0), None);
        assert_eq!(error.capacity().unwrap().required, 96);
    }

    #[test]
    fn an_unknown_discriminant_rebuilds_nothing() {
        assert_eq!(Error::from_parts(ErrorCode::InvalidPlan, 0), None);
        assert_eq!(Error::from_parts(ErrorCode::Protocol, 4095), None);
        // A code that carries no value inside takes no detail either.
        assert_eq!(Error::from_parts(ErrorCode::InvalidToken, 1), None);
    }
}
