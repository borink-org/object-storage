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
#[repr(u16)]
pub enum FailureClass {
    /// The service rejected the credentials or the authorization.
    Auth = 1,
    /// The service throttled the request. You can retry it later.
    Throttled = 2,
    /// The service failed, or it was unavailable.
    Server = 3,
    /// The service answered with a redirect.
    ///
    /// This crate does not follow redirects. It reports them to you.
    Redirect = 4,
    /// Any other failure, such as a malformed request.
    Other = 5,
}

impl FailureClass {
    /// Returns the sentence that [`Display`](fmt::Display) writes for this
    /// category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "the service rejected the credentials or the authorization",
            Self::Throttled => "the service throttled the request",
            Self::Server => "the service failed, or it was unavailable",
            Self::Redirect => "the service answered with a redirect",
            Self::Other => "the service refused the request",
        }
    }

    /// Returns the category with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Auth,
            2 => Self::Throttled,
            3 => Self::Server,
            4 => Self::Redirect,
            5 => Self::Other,
            _ => return None,
        })
    }
}

/// A response head that reports a failure.
///
/// The three head-reading methods return this in the two outcomes that carry a
/// failure. Its fields are public, so you can store one and hand the parts
/// back to
/// [`Blobs::accept_error_body`](crate::Blobs::accept_error_body) later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure<'h> {
    /// The HTTP status code.
    pub status: u16,
    /// The category of the failure. Use it to decide whether to retry.
    pub class: FailureClass,
    /// The specific error, if the head or the body named one.
    pub kind: Option<ServiceErrorKind>,
    /// The value of the `x-ms-request-id` header, if Azure sent one.
    pub request_id: Option<&'h [u8]>,
}

impl fmt::Display for Failure<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The kind is the finer parse, so it wins when one was made. The class
        // is the fallback and is always present.
        let reason = match self.kind {
            Some(kind) => kind.as_str(),
            None => self.class.as_str(),
        };
        write!(f, "{reason} (HTTP {}", self.status)?;
        // Azure sends an ASCII identifier, but a header value carries no such
        // guarantee. Name it only when it is printable.
        if let Some(id) = self.request_id.and_then(|id| core::str::from_utf8(id).ok()) {
            write!(f, ", request {id}")?;
        }
        f.write_str(")")
    }
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
    /// This outcome is not final. Read the response body and pass it, with the
    /// status and the request identifier of this failure, to
    /// [`Blobs::accept_error_body`](crate::Blobs::accept_error_body). That
    /// call returns the final outcome. If you cannot read the body, pass an
    /// empty one and the error stays unnamed.
    ///
    /// Cap what you read. An error body is a diagnostic, and the service
    /// decides how long it is.
    ///
    /// The `kind` of this failure is always [`None`].
    NeedErrorBody(Failure<'h>),
    /// The service refused the request, or it failed to serve it.
    ServiceFailure(Failure<'h>),
}

/// The result of reading the response head of a write.
///
/// Every head that Azure sends becomes one of these values, including the
/// heads that report a failure.
///
/// [`Blobs::accept_put_head`](crate::Blobs::accept_put_head) returns an
/// [`Err`] only if the head is invalid: see [`Error`](crate::Error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PutHeadOutcome<'h> {
    /// Azure stored the object.
    Created {
        /// The metadata of the object that Azure stored.
        ///
        /// A write reports no size, because the size is the length of the
        /// content that you sent.
        meta: ObjectMeta<'h>,
    },
    /// The entity tag in the condition did not match, so Azure stored nothing.
    ///
    /// A write conditional on `If-None-Match: *` does not report a lost race
    /// here. Azure answers that with status 409, which reaches you as
    /// [`Self::ServiceFailure`] whose kind is
    /// [`ServiceErrorKind::AlreadyExists`]. The Azure documentation states 412
    /// for that case; this crate follows the service.
    PreconditionFailed,
    /// The container does not exist, so Azure stored nothing.
    NotFound {
        /// The specific error, if the head names one.
        kind: Option<ServiceErrorKind>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Read the response body and pass it, with the
    /// status and the request identifier of this failure, to
    /// [`Blobs::accept_put_error_body`](crate::Blobs::accept_put_error_body).
    /// That call returns the final outcome. If you cannot read the body, pass
    /// an empty one and the error stays unnamed.
    ///
    /// The `kind` of this failure is always [`None`].
    NeedErrorBody(Failure<'h>),
    /// The service refused the write, or it failed to store the object.
    ServiceFailure(Failure<'h>),
}

/// The result of reading the response head of a removal.
///
/// Every head that Azure sends becomes one of these values, including the
/// heads that report a failure.
///
/// [`Blobs::accept_delete_head`](crate::Blobs::accept_delete_head) returns an
/// [`Err`] only if the head is invalid: see [`Error`](crate::Error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeleteHeadOutcome<'h> {
    /// Azure accepted the removal.
    ///
    /// The object is gone unless the plan asked only for its snapshots: see
    /// [`DeleteKind::SnapshotsOnly`](crate::DeleteKind::SnapshotsOnly).
    Accepted,
    /// The entity tag in the condition did not match, so Azure removed
    /// nothing.
    PreconditionFailed,
    /// The object does not exist, so there was nothing to remove.
    ///
    /// A caller that removes an object it does not need can treat this as
    /// success. This crate does not decide that for you.
    NotFound {
        /// The specific error, if the head names one.
        kind: Option<ServiceErrorKind>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Read the response body and pass it, with the
    /// status and the request identifier of this failure, to
    /// [`Blobs::accept_delete_error_body`](crate::Blobs::accept_delete_error_body).
    /// That call returns the final outcome. If you cannot read the body, pass
    /// an empty one and the error stays unnamed.
    ///
    /// The `kind` of this failure is always [`None`].
    NeedErrorBody(Failure<'h>),
    /// The service refused the removal, or it failed to carry it out.
    ///
    /// An object that has snapshots is refused here, unless the plan asked to
    /// remove them too: see [`DeleteKind`](crate::DeleteKind).
    ServiceFailure(Failure<'h>),
}

/// The result of reading the response head of a listing.
///
/// Every head that Azure sends becomes one of these values, including the
/// heads that report a failure.
///
/// [`Blobs::accept_list_head`](crate::Blobs::accept_list_head) returns an
/// [`Err`] only if the head is invalid: see [`Error`](crate::Error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ListHeadOutcome<'h> {
    /// The page follows in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`Blobs::fill_listing`](crate::Blobs::fill_listing), which reads the
    /// entries out of it.
    Page {
        /// The exact length of the response body, if the head states it.
        ///
        /// Size the body buffer from this. The body is a document, not object
        /// bytes, so there is no offset into an object to report with it.
        expected_len: Option<u64>,
    },
    /// The container does not exist, so there was nothing to list.
    NotFound {
        /// The specific error, if the head names one.
        kind: Option<ServiceErrorKind>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Read the response body and pass it, with the
    /// status and the request identifier of this failure, to
    /// [`Blobs::accept_list_error_body`](crate::Blobs::accept_list_error_body).
    /// That call returns the final outcome. If you cannot read the body, pass
    /// an empty one and the error stays unnamed.
    ///
    /// The `kind` of this failure is always [`None`].
    NeedErrorBody(Failure<'h>),
    /// The service refused the listing, or it failed to serve it.
    ServiceFailure(Failure<'h>),
}

/// What one call to [`Blobs::fill_listing`](crate::Blobs::fill_listing) read.
///
/// Your array is the budget. A page that does not fit in it is not an error
/// and loses nothing: the call stops at the entry that would not fit, leaving
/// the rest of the body as it found it, and hands back the token that reads
/// the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill<'b> {
    /// The page was read to its end.
    Page(Listing<'b>),
    /// The array filled before the page ended.
    ///
    /// Use the entries that were written, and then read the rest of the same
    /// body with
    /// [`Blobs::resume_listing`](crate::Blobs::resume_listing). The entries
    /// borrow the body, so the compiler will not let you do it the other way
    /// round.
    ///
    /// An array with no room at all reports this with `filled` of zero and
    /// makes no progress.
    Partial {
        /// The number of entries written into your array.
        filled: usize,
        /// Where the rest of the page starts.
        ///
        /// This describes the body it came from and no other.
        resume: Resume,
    },
}

/// Where a page was left off.
///
/// [`Fill::Partial`] hands this out and
/// [`Blobs::resume_listing`](crate::Blobs::resume_listing) takes it back. It
/// holds no borrow, so it can sit beside the buffer it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resume {
    // Where the walk stopped: the next entry, or the tag that closes them.
    pub(crate) at: usize,
    // Whether that point is still inside the entries.
    pub(crate) within: bool,
    // The text of the next marker, as a range of the body. It lies past the
    // entries, so it is still untouched whenever the walk stops.
    pub(crate) marker: Option<(usize, usize)>,
}

/// What one page of a listing held.
///
/// [`Fill::Page`] carries this once the page has been read to its end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Listing<'b> {
    /// The number of entries that this call wrote into your array.
    ///
    /// The entries after these are untouched.
    pub filled: usize,
    /// Where the next page starts, or [`None`] when the listing is complete.
    ///
    /// Copy these bytes into your own storage and pass them as
    /// [`PhysicalList::marker`](crate::PhysicalList::marker). They borrow the
    /// body, which the next page overwrites.
    ///
    /// This is how a listing of any size is read: one page at a time, with the
    /// service naming where to continue. A page reports a marker whenever more
    /// keys follow, even if this page reported fewer entries than it asked
    /// for.
    pub next_marker: Option<&'b [u8]>,
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
#[repr(u16)]
pub enum ServiceErrorKind {
    /// The object does not exist.
    NotFound = 1,
    /// The container does not exist.
    NoSuchContainer = 2,
    /// The object or the container already exists.
    AlreadyExists = 3,
    /// The service rejected the credentials or the authorization.
    Unauthorized = 4,
    /// A precondition on the request did not hold.
    Precondition = 5,
    /// The service cannot serve the requested byte range.
    RangeNotSatisfiable = 6,
    /// The service throttled the request.
    Throttled = 7,
    /// The service timed out while it processed the request.
    Timeout = 8,
    /// The service failed, or it was unavailable.
    Service = 9,
}

impl ServiceErrorKind {
    /// Returns the sentence that [`Display`](fmt::Display) writes for this
    /// error.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "the object does not exist",
            Self::NoSuchContainer => "the container does not exist",
            Self::AlreadyExists => "the object or the container already exists",
            Self::Unauthorized => "the service rejected the credentials or the authorization",
            Self::Precondition => "a precondition on the request did not hold",
            Self::RangeNotSatisfiable => "the service cannot serve the requested byte range",
            Self::Throttled => "the service throttled the request",
            Self::Timeout => "the service timed out while it processed the request",
            Self::Service => "the service failed, or it was unavailable",
        }
    }

    /// Returns the error with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::NotFound,
            2 => Self::NoSuchContainer,
            3 => Self::AlreadyExists,
            4 => Self::Unauthorized,
            5 => Self::Precondition,
            6 => Self::RangeNotSatisfiable,
            7 => Self::Throttled,
            8 => Self::Timeout,
            9 => Self::Service,
            _ => return None,
        })
    }
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for ServiceErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
                    f.write_str(ServiceErrorKind::NoSuchContainer.as_str())
                }
                _ => f.write_str(ServiceErrorKind::NotFound.as_str()),
            },
            Self::RangeNotSatisfiable { object_size } => {
                f.write_str("the service cannot serve the requested range")?;
                match object_size {
                    Some(size) => write!(f, "; the object is {size} bytes"),
                    None => Ok(()),
                }
            }
            Self::NeedErrorBody(failure) | Self::ServiceFailure(failure) => {
                fmt::Display::fmt(failure, f)
            }
        }
    }
}

impl fmt::Display for PutHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created { .. } => f.write_str("the service stored the object"),
            Self::PreconditionFailed => f.write_str("the condition on the write did not hold"),
            Self::NotFound { .. } => f.write_str(ServiceErrorKind::NoSuchContainer.as_str()),
            Self::NeedErrorBody(failure) | Self::ServiceFailure(failure) => {
                fmt::Display::fmt(failure, f)
            }
        }
    }
}

impl fmt::Display for DeleteHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accepted => f.write_str("the service accepted the removal"),
            Self::PreconditionFailed => f.write_str("the condition on the removal did not hold"),
            Self::NotFound { .. } => f.write_str(ServiceErrorKind::NotFound.as_str()),
            Self::NeedErrorBody(failure) | Self::ServiceFailure(failure) => {
                fmt::Display::fmt(failure, f)
            }
        }
    }
}

impl fmt::Display for ListHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Page { .. } => f.write_str("the page follows in the response body"),
            Self::NotFound { .. } => f.write_str(ServiceErrorKind::NoSuchContainer.as_str()),
            Self::NeedErrorBody(failure) | Self::ServiceFailure(failure) => {
                fmt::Display::fmt(failure, f)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{Failure, FailureClass, GetHeadOutcome, ServiceErrorKind};
    use std::string::ToString;

    #[test]
    fn describes_a_service_failure_with_its_status_and_request_id() {
        let failure = GetHeadOutcome::ServiceFailure(Failure {
            status: 429,
            class: FailureClass::Throttled,
            kind: None,
            request_id: Some(b"request-123"),
        });
        assert_eq!(
            failure.to_string(),
            "the service throttled the request (HTTP 429, request request-123)"
        );
    }

    #[test]
    fn prefers_the_named_error_over_the_category() {
        let failure = GetHeadOutcome::ServiceFailure(Failure {
            status: 409,
            class: FailureClass::Other,
            kind: Some(ServiceErrorKind::AlreadyExists),
            request_id: None,
        });
        assert_eq!(
            failure.to_string(),
            "the object or the container already exists (HTTP 409)"
        );
    }

    #[test]
    fn omits_a_request_id_that_is_not_printable() {
        let failure = GetHeadOutcome::ServiceFailure(Failure {
            status: 500,
            class: FailureClass::Server,
            kind: None,
            request_id: Some(b"\xff"),
        });
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
