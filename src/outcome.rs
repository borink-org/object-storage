use core::fmt;

/// Object metadata borrowed from a response head.
///
/// Each field holds the bytes that Azure sent. To read `last_modified` as an
/// instant, use [`layered::http_date_ms`](crate::layered::http_date_ms).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObjectMeta<'h> {
    /// The size of the whole object, if the head states it.
    ///
    /// This is not the length of the returned range. For that length, read
    /// [`BodyWindow::expected_len`].
    pub size: Option<u64>,
    /// The entity tag, if Azure returned one.
    pub e_tag: Option<&'h [u8]>,
    /// The value of the `Last-Modified` header, if Azure returned one.
    pub last_modified: Option<&'h [u8]>,
    /// The Azure blob version identifier, if Azure returned one.
    pub version: Option<&'h [u8]>,
    /// The value of the `Content-Encoding` header, if Azure returned one.
    ///
    /// This crate does not decode the body. It returns this value so that you
    /// know how the bytes are encoded.
    pub content_encoding: Option<&'h [u8]>,
}

/// Where the bytes of the response body belong in the object.
///
/// The offsets count the stored bytes of the object.
///
/// # Transport contract
///
/// Your HTTP client must remove the transfer encoding but keep the content
/// encoding. Turn off automatic decompression: a client that decompresses the
/// body changes the bytes and usually removes the headers that record it. The
/// offsets here are then wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyWindow {
    /// The offset in the object of the first byte of the response body.
    pub object_offset: u64,
    /// The exact length of the response body, if the head states it.
    pub expected_len: Option<u64>,
    /// The size of the whole object, if the head states it.
    pub object_size: Option<u64>,
}

/// The category of a service failure.
///
/// Use this to decide whether to retry a request, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureClass {
    /// Azure rejected the credentials or the authorization.
    Auth,
    /// Azure throttled the request. You can retry it later.
    Throttled,
    /// Azure failed, or the service was unavailable.
    Server,
    /// Azure answered with a redirect.
    ///
    /// This crate does not follow redirects. It reports them to you.
    Redirect,
    /// Any other failure, such as a malformed request.
    Other,
}

/// The result of reading a response head.
///
/// Every head that Azure sends becomes one of these values, including the
/// heads that report a failure. Branch on this value to drive the request.
///
/// [`Blobs::accept_get_head`](crate::Blobs::accept_get_head) returns an
/// [`Err`] only if the head is invalid: see [`Error`](crate::Error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GetHeadOutcome<'h> {
    /// A body follows. Read it and put the bytes at `body`.
    Body {
        /// The metadata from the head.
        meta: ObjectMeta<'h>,
        /// Where the bytes of the body belong.
        body: BodyWindow,
    },
    /// No body follows and the request is complete.
    ///
    /// A metadata plan ends here.
    Complete(ObjectMeta<'h>),
    /// The `If-None-Match` condition held, so Azure sent no body.
    NotModified {
        /// The entity tag, if Azure repeated it.
        e_tag: Option<&'h [u8]>,
    },
    /// The `If-Match` condition did not hold, so Azure sent no body.
    PreconditionFailed,
    /// The object does not exist.
    NotFound,
    /// Azure cannot serve the requested range.
    RangeNotSatisfiable {
        /// The size of the object, if `Content-Range: bytes */N` states it.
        object_size: Option<u64>,
    },
    /// Azure refused the request, or failed to serve it.
    ServiceFailure {
        /// The HTTP status code.
        status: u16,
        /// The category of the failure. Use it to decide whether to retry.
        class: FailureClass,
        /// The value of the `x-ms-request-id` header, if Azure sent one.
        request_id: Option<&'h [u8]>,
    },
}

/// The result of [`classify_error`](crate::classify_error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Azure named an error that this crate recognizes.
    Classified(AzureErrorKind),
    /// Your read limit cut the body short before the error code appeared.
    ///
    /// Read more of the body and classify it again.
    Incomplete,
    /// The response was complete, but it named no error code that this crate
    /// recognizes.
    Unknown,
}

/// An Azure error code, mapped to a name that does not change.
///
/// Azure defines many error codes. This enum groups the codes that a read can
/// return. Match on this instead of on the code strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AzureErrorKind {
    /// The object does not exist.
    NotFound,
    /// The container does not exist.
    NoSuchContainer,
    /// The object or the container already exists.
    AlreadyExists,
    /// Azure rejected the credentials or the authorization.
    Unauthorized,
    /// A precondition on the request did not hold.
    Precondition,
    /// Azure cannot serve the requested byte range.
    RangeNotSatisfiable,
    /// Azure throttled the request.
    Throttled,
    /// Azure timed out while it processed the request.
    Timeout,
    /// Azure failed, or the service was unavailable.
    Service,
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth => f.write_str("Azure rejected the credentials or the authorization"),
            Self::Throttled => f.write_str("Azure throttled the request"),
            Self::Server => f.write_str("Azure failed, or the service was unavailable"),
            Self::Redirect => f.write_str("Azure answered with a redirect"),
            Self::Other => f.write_str("Azure refused the request"),
        }
    }
}

impl fmt::Display for AzureErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("the object does not exist"),
            Self::NoSuchContainer => f.write_str("the container does not exist"),
            Self::AlreadyExists => f.write_str("the object or the container already exists"),
            Self::Unauthorized => {
                f.write_str("Azure rejected the credentials or the authorization")
            }
            Self::Precondition => f.write_str("a precondition on the request did not hold"),
            Self::RangeNotSatisfiable => f.write_str("Azure cannot serve the requested byte range"),
            Self::Throttled => f.write_str("Azure throttled the request"),
            Self::Timeout => f.write_str("Azure timed out while it processed the request"),
            Self::Service => f.write_str("Azure failed, or the service was unavailable"),
        }
    }
}

impl fmt::Display for GetHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body { .. } => f.write_str("the object follows in the response body"),
            Self::Complete(_) => f.write_str("the response carries no body and is complete"),
            Self::NotModified { .. } => f.write_str("the object is not modified"),
            Self::PreconditionFailed => f.write_str("the If-Match condition did not hold"),
            Self::NotFound => f.write_str("the object does not exist"),
            Self::RangeNotSatisfiable { object_size } => {
                f.write_str("Azure cannot serve the requested range")?;
                match object_size {
                    Some(size) => write!(f, "; the object is {size} bytes"),
                    None => Ok(()),
                }
            }
            Self::ServiceFailure {
                status,
                class,
                request_id,
            } => {
                write!(f, "{class} (HTTP {status}")?;
                // Azure sends an ASCII identifier, but a header value carries
                // no such guarantee. Name it only when it is printable.
                if let Some(id) = request_id.and_then(|id| core::str::from_utf8(id).ok()) {
                    write!(f, ", request {id}")?;
                }
                f.write_str(")")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{AzureErrorKind, FailureClass, GetHeadOutcome};
    use std::string::ToString;

    #[test]
    fn describes_a_service_failure_with_its_status_and_request_id() {
        let failure = GetHeadOutcome::ServiceFailure {
            status: 429,
            class: FailureClass::Throttled,
            request_id: Some(b"request-123"),
        };
        assert_eq!(
            failure.to_string(),
            "Azure throttled the request (HTTP 429, request request-123)"
        );
    }

    #[test]
    fn omits_a_request_id_that_is_not_printable() {
        let failure = GetHeadOutcome::ServiceFailure {
            status: 500,
            class: FailureClass::Server,
            request_id: Some(b"\xff"),
        };
        assert_eq!(
            failure.to_string(),
            "Azure failed, or the service was unavailable (HTTP 500)"
        );
    }

    #[test]
    fn describes_the_remaining_outcomes() {
        assert_eq!(
            GetHeadOutcome::RangeNotSatisfiable {
                object_size: Some(10)
            }
            .to_string(),
            "Azure cannot serve the requested range; the object is 10 bytes"
        );
        assert_eq!(
            GetHeadOutcome::NotFound.to_string(),
            "the object does not exist"
        );
        assert_eq!(
            AzureErrorKind::NoSuchContainer.to_string(),
            "the container does not exist"
        );
    }
}
