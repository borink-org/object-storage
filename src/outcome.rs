use core::fmt;

/// Object metadata borrowed from a response head.
///
/// Each field holds the bytes that the service sent. To read `last_modified`
/// as an instant, use [`layered::http_date_ms`](crate::layered::http_date_ms).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObjectMeta<'h> {
    /// The size of the whole object, if the head states it.
    ///
    /// This is not the length of the returned range. For that length, read
    /// [`BodyWindow::expected_len`].
    pub size: Option<u64>,
    /// The entity tag, if the service returned one.
    pub e_tag: Option<&'h [u8]>,
    /// The value of the `Last-Modified` header, if the service returned one.
    pub last_modified: Option<&'h [u8]>,
    /// The version identifier, if the service returned one.
    pub version: Option<&'h [u8]>,
    /// The value of the `Content-Encoding` header, if the service returned
    /// one.
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
/// Use this to decide whether to retry a request, and how. For the specific
/// error that the service named, read [`ServiceErrorKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureClass {
    /// The service rejected the credentials or the authorization.
    Auth,
    /// The service throttled the request. You can retry it later.
    Throttled,
    /// The service failed, or it was unavailable.
    Server,
    /// The service answered with a redirect.
    ///
    /// This crate does not follow redirects. It reports them to you.
    Redirect,
    /// Any other failure, such as a malformed request.
    Other,
}

/// The result of reading a response head.
///
/// Every head that the service sends becomes one of these values, including
/// the heads that report a failure. Branch on this value to drive the request.
///
/// [`Blobs::accept_get_head`](crate::Blobs::accept_get_head) returns an
/// [`Err`] only if the head is invalid: see [`Error`](crate::Error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GetHeadOutcome<'h> {
    /// A body follows. Read it and put the bytes at `body`.
    #[non_exhaustive]
    Body {
        /// The metadata from the head.
        meta: ObjectMeta<'h>,
        /// Where the bytes of the body belong.
        body: BodyWindow,
    },
    /// No body follows and the request is complete.
    ///
    /// A metadata plan ends here.
    Complete {
        /// The metadata from the head.
        meta: ObjectMeta<'h>,
    },
    /// The `If-None-Match` condition held, so the service sent no body.
    NotModified {
        /// The entity tag, if the service repeated it.
        e_tag: Option<&'h [u8]>,
    },
    /// The `If-Match` condition did not hold, so the service sent no body.
    PreconditionFailed,
    /// The object does not exist, or the container that holds it does not.
    NotFound {
        /// Which of the two is missing, if the head names the error.
        ///
        /// [`ServiceErrorKind::NoSuchContainer`] means that the container is
        /// missing. If this is [`None`], read the response body with
        /// [`classify_error`](crate::classify_error).
        kind: Option<ServiceErrorKind>,
    },
    /// The service cannot serve the requested range.
    RangeNotSatisfiable {
        /// The size of the object, if `Content-Range: bytes */N` states it.
        object_size: Option<u64>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Read the response body and pass it to
    /// [`Blobs::accept_error_body`](crate::Blobs::accept_error_body), which
    /// returns the final outcome. If you cannot read the body, pass an empty
    /// one and the error stays unnamed.
    ///
    /// Cap what you read. An error body is a diagnostic, and the service
    /// decides how long it is.
    #[non_exhaustive]
    NeedErrorBody {
        /// The HTTP status code.
        status: u16,
        /// The category of the failure, from the status alone.
        class: FailureClass,
        /// The value of the `x-ms-request-id` header, if Azure sent one.
        request_id: Option<&'h [u8]>,
    },
    /// The service refused the request, or it failed to serve it.
    #[non_exhaustive]
    ServiceFailure {
        /// The HTTP status code.
        status: u16,
        /// The category of the failure. Use it to decide whether to retry.
        class: FailureClass,
        /// The specific error, if the head names one.
        ///
        /// If this is [`None`], read the response body with
        /// [`classify_error`](crate::classify_error).
        kind: Option<ServiceErrorKind>,
        /// The value of the `x-ms-request-id` header, if Azure sent one.
        request_id: Option<&'h [u8]>,
    },
}

/// The result of [`classify_error`](crate::classify_error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Classification {
    /// The body named an error that this crate recognizes.
    Classified(ServiceErrorKind),
    /// Your read limit cut the body short before the error code appeared.
    ///
    /// Read more of the body and classify it again.
    Incomplete,
    /// The response was complete, but it named no error code that this crate
    /// recognizes.
    Unknown,
}

/// A service error code, mapped to a name that does not change.
///
/// A storage service defines many error codes, and two services name the same
/// error differently. This enum groups the codes that a read can return. Match
/// on this instead of on the code strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServiceErrorKind {
    /// The object does not exist.
    NotFound,
    /// The container does not exist.
    NoSuchContainer,
    /// The object or the container already exists.
    AlreadyExists,
    /// The service rejected the credentials or the authorization.
    Unauthorized,
    /// A precondition on the request did not hold.
    Precondition,
    /// The service cannot serve the requested byte range.
    RangeNotSatisfiable,
    /// The service throttled the request.
    Throttled,
    /// The service timed out while it processed the request.
    Timeout,
    /// The service failed, or it was unavailable.
    Service,
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth => f.write_str("the service rejected the credentials or the authorization"),
            Self::Throttled => f.write_str("the service throttled the request"),
            Self::Server => f.write_str("the service failed, or it was unavailable"),
            Self::Redirect => f.write_str("the service answered with a redirect"),
            Self::Other => f.write_str("the service refused the request"),
        }
    }
}

impl fmt::Display for ServiceErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("the object does not exist"),
            Self::NoSuchContainer => f.write_str("the container does not exist"),
            Self::AlreadyExists => f.write_str("the object or the container already exists"),
            Self::Unauthorized => {
                f.write_str("the service rejected the credentials or the authorization")
            }
            Self::Precondition => f.write_str("a precondition on the request did not hold"),
            Self::RangeNotSatisfiable => {
                f.write_str("the service cannot serve the requested byte range")
            }
            Self::Throttled => f.write_str("the service throttled the request"),
            Self::Timeout => f.write_str("the service timed out while it processed the request"),
            Self::Service => f.write_str("the service failed, or it was unavailable"),
        }
    }
}

impl fmt::Display for GetHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body { .. } => f.write_str("the object follows in the response body"),
            Self::Complete { .. } => f.write_str("the response carries no body and is complete"),
            Self::NotModified { .. } => f.write_str("the object is not modified"),
            Self::PreconditionFailed => f.write_str("the If-Match condition did not hold"),
            Self::NotFound { kind } => match kind {
                Some(ServiceErrorKind::NoSuchContainer) => {
                    f.write_str("the container does not exist")
                }
                _ => f.write_str("the object does not exist"),
            },
            Self::RangeNotSatisfiable { object_size } => {
                f.write_str("the service cannot serve the requested range")?;
                match object_size {
                    Some(size) => write!(f, "; the object is {size} bytes"),
                    None => Ok(()),
                }
            }
            Self::NeedErrorBody {
                status,
                class,
                request_id,
            } => write_failure(f, class, *status, *request_id),
            Self::ServiceFailure {
                status,
                class,
                kind,
                request_id,
            } => match kind {
                // The kind is the finer parse, so it wins when one was made.
                // The class is the fallback and is always present.
                Some(kind) => write_failure(f, kind, *status, *request_id),
                None => write_failure(f, class, *status, *request_id),
            },
        }
    }
}

fn write_failure(
    f: &mut fmt::Formatter<'_>,
    reason: &dyn fmt::Display,
    status: u16,
    request_id: Option<&[u8]>,
) -> fmt::Result {
    write!(f, "{reason} (HTTP {status}")?;
    // Azure sends an ASCII identifier, but a header value carries no such
    // guarantee. Name it only when it is printable.
    if let Some(id) = request_id.and_then(|id| core::str::from_utf8(id).ok()) {
        write!(f, ", request {id}")?;
    }
    f.write_str(")")
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{FailureClass, GetHeadOutcome, ServiceErrorKind};
    use std::string::ToString;

    #[test]
    fn describes_a_service_failure_with_its_status_and_request_id() {
        let failure = GetHeadOutcome::ServiceFailure {
            status: 429,
            class: FailureClass::Throttled,
            kind: None,
            request_id: Some(b"request-123"),
        };
        assert_eq!(
            failure.to_string(),
            "the service throttled the request (HTTP 429, request request-123)"
        );
    }

    #[test]
    fn prefers_the_named_error_over_the_category() {
        let failure = GetHeadOutcome::ServiceFailure {
            status: 409,
            class: FailureClass::Other,
            kind: Some(ServiceErrorKind::AlreadyExists),
            request_id: None,
        };
        assert_eq!(
            failure.to_string(),
            "the object or the container already exists (HTTP 409)"
        );
    }

    #[test]
    fn omits_a_request_id_that_is_not_printable() {
        let failure = GetHeadOutcome::ServiceFailure {
            status: 500,
            class: FailureClass::Server,
            kind: None,
            request_id: Some(b"\xff"),
        };
        assert_eq!(
            failure.to_string(),
            "the service failed, or it was unavailable (HTTP 500)"
        );
    }

    #[test]
    fn separates_a_missing_object_from_a_missing_container() {
        assert_eq!(
            GetHeadOutcome::NotFound { kind: None }.to_string(),
            "the object does not exist"
        );
        assert_eq!(
            GetHeadOutcome::NotFound {
                kind: Some(ServiceErrorKind::NoSuchContainer)
            }
            .to_string(),
            "the container does not exist"
        );
    }

    #[test]
    fn describes_an_unsatisfiable_range_with_the_object_size() {
        assert_eq!(
            GetHeadOutcome::RangeNotSatisfiable {
                object_size: Some(10)
            }
            .to_string(),
            "the service cannot serve the requested range; the object is 10 bytes"
        );
    }
}
