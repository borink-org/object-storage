use core::fmt;

/// The result type that this crate returns.
pub type Result<T> = core::result::Result<T, Error>;

/// The exact capacity that your buffer needs.
///
/// For an encoding method the two counts are bytes of the request buffer.
/// Grow the buffer to `required` bytes and call the same method again. To
/// learn the requirement before the first call, use
/// [`layered::get_requirements`](crate::layered::get_requirements).
///
/// For [`Blobs::fill_listing`](crate::Blobs::fill_listing) the two counts are
/// entries of the array, and `required` is the number that the page holds.
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
            "the buffer needs {} but has {}",
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
    // 8, 9 and 13 name the part operations, which this crate does not write
    // yet. Every number here is assigned once, so the holes stay open.
    /// The listing prefix is longer than an object key may be.
    Prefix = 10,
    /// The listing marker is empty, or it is not UTF-8.
    ///
    /// A page that starts at the beginning of the container carries no marker
    /// at all.
    Marker = 11,
    /// The listing asks for zero entries.
    MaxResults = 12,
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
            Self::Prefix => "invalid listing prefix",
            Self::Marker => "invalid listing marker",
            Self::MaxResults => "a listing cannot ask for zero entries",
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
            10 => Self::Prefix,
            11 => Self::Marker,
            12 => Self::MaxResults,
            _ => return None,
        })
    }
}

impl fmt::Display for InvalidPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a response cannot be read.
///
/// Azure sends none of these. Each one means that the response was changed on
/// the way to you, or that the service did not behave as it documents. This
/// crate cannot recover from any of them, and neither can you: retry the
/// request, or report it. These four values are for the message you write.
///
/// # Reading the exact value that was wrong
///
/// This crate names the part of the response, not the value. If your program
/// needs the value, read it yourself: you hold the
/// [`ResponseHead`](crate::ResponseHead) you passed in, and the shape you
/// planned with. Both are [`Copy`] and both have public fields, and every
/// fault here is decided from those two.
///
/// ```
/// use borink_object_storage_proto::{
///     Blobs, Container, Error, GetShape, RequestedRange, ResponseFault, ResponseHead,
/// };
///
/// # fn main() -> borink_object_storage_proto::Result<()> {
/// let blobs = Blobs::new(Container::new("https://account.blob.core.windows.net", "c")?, "t")?;
/// let shape = GetShape {
///     range: RequestedRange::Bounded { start: 2, end: 6 },
///     ..GetShape::default()
/// };
/// let head = ResponseHead::from_headers(206, [("Content-Range", b"bytes 2-4/10".as_slice())]);
///
/// let fault = blobs.accept_get_head(shape, head);
/// assert_eq!(fault, Err(Error::Response(ResponseFault::Range)));
///
/// // The head is still yours, so the exact values are too.
/// assert_eq!(head.content_range, Some(b"bytes 2-4/10".as_slice()));
/// assert_eq!(shape.range, RequestedRange::Bounded { start: 2, end: 6 });
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum ResponseFault {
    /// A value in the head is missing, is not a number, or disagrees with
    /// another value in the same head.
    Head = 1,
    /// The status does not answer the request that was sent.
    Status = 2,
    /// The service served a range other than the one that the plan requested.
    Range = 3,
    /// The response body is not well formed, or it is not the document that
    /// answers the request.
    Body = 4,
}

impl ResponseFault {
    /// Returns the sentence that [`Display`](fmt::Display) writes for this
    /// fault.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Head => "the response head is unreadable, or it contradicts itself",
            Self::Status => "the status does not answer the request",
            Self::Range => "the service served another range than the plan requested",
            Self::Body => "the response body is not the document that answers the request",
        }
    }

    /// Returns the fault with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Head,
            2 => Self::Status,
            3 => Self::Range,
            4 => Self::Body,
            _ => return None,
        })
    }
}

impl fmt::Display for ResponseFault {
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
    /// [`Error::Response`].
    Response = 6,
}

impl ErrorCode {
    /// Returns a sentence naming this kind of failure.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "invalid endpoint",
            Self::InvalidContainer => "invalid container name",
            Self::InvalidToken => "invalid bearer token",
            Self::InvalidPlan => "the plan cannot become a request",
            Self::Capacity => "the buffer is too small",
            Self::Response => "the response cannot be read",
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
            6 => Self::Response,
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
    /// Your request buffer or your entry array is too small.
    Capacity(CapacityError),
    /// The response cannot be read.
    Response(ResponseFault),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint | Self::InvalidContainer | Self::InvalidToken => {
                f.write_str(self.code().as_str())
            }
            Self::InvalidPlan(plan) => fmt::Display::fmt(plan, f),
            Self::Capacity(error) => fmt::Display::fmt(error, f),
            Self::Response(fault) => fmt::Display::fmt(fault, f),
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

impl From<ResponseFault> for Error {
    fn from(value: ResponseFault) -> Self {
        Self::Response(value)
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
            Self::Response(_) => ErrorCode::Response,
        }
    }

    /// Returns the discriminant of the value inside, or 0 if there is none.
    ///
    /// [`Self::Capacity`] carries two sizes rather than a discriminant, and
    /// returns 0 here. Read those sizes with [`Self::capacity`].
    pub const fn detail(&self) -> u16 {
        match *self {
            Self::InvalidPlan(plan) => plan as u16,
            Self::Response(fault) => fault as u16,
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
            ErrorCode::Response => match ResponseFault::from_discriminant(detail) {
                Some(fault) => Self::Response(fault),
                None => return None,
            },
            ErrorCode::Capacity => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{CapacityError, Error, ErrorCode, InvalidPlan, ResponseFault};
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
            if let Some(fault) = ResponseFault::from_discriminant(detail) {
                errors.push(Error::Response(fault));
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
        assert_eq!(Error::from_parts(ErrorCode::Response, 4095), None);
        // A code that carries no value inside takes no detail either.
        assert_eq!(Error::from_parts(ErrorCode::InvalidToken, 1), None);
    }
}
