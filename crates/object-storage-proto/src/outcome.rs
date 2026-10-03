use core::fmt;

/// The result of reading the response head of a stage: an Azure Put Block,
/// or an S3 UploadPart.
///
/// A head that reports a failure is one of these too. The methods that read
/// it return an [`Err`] only for a head they cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StageHeadOutcome<'h> {
    /// The service holds the part.
    Staged {
        /// The entity tag of the part, if the head carries one.
        ///
        /// S3 names a part at the commit by its number and this tag, so keep
        /// it: see [`s3::PartRef`](crate::s3::PartRef). Azure names a block by
        /// the ID you chose, and sends no tag. The checksum and encryption
        /// headers that Azure does send are on
        /// [`BlockResponseHead`](crate::azure::BlockResponseHead).
        e_tag: Option<&'h [u8]>,
    },
    /// S3 took an UploadPartCopy, and says in the body whether it
    /// succeeded.
    ///
    /// This outcome is not final. Read the whole body and pass it with the
    /// head to
    /// [`s3::Objects::accept_stage_part_copy_body`](crate::s3::Objects::accept_stage_part_copy_body),
    /// which returns the final outcome. No other stage returns this.
    NeedResultBody {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// On S3, the upload does not exist. A missing container is a
    /// [`Self::ServiceFailure`] with [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The service's reason, if known.
        ///
        /// [`ServiceErrorKind::NoSuchUpload`] means the upload is gone: it
        /// was committed or aborted, or it never existed.
        kind: Option<ServiceErrorKind>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the request.
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for StageHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Staged { .. } => f.write_str("the service holds the part"),
            Self::NeedResultBody { .. } => f.write_str("the result follows in the response body"),
            Self::NotFound { kind } => not_found(f, *kind),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

/// The result of reading the response head of a copy: an Azure Copy Blob or
/// Copy Blob From URL, or an S3 CopyObject.
///
/// A head that reports a failure is one of these too. The methods that read
/// it return an [`Err`] only for a head they cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopyHeadOutcome<'h> {
    /// The service wrote the copy.
    Copied {
        /// The metadata of the copy: its entity tag, and on Azure its last
        /// modification, the copy's ID and its status, `success`.
        meta: ObjectMeta<'h>,
    },
    /// Azure started the copy and finishes it later.
    ///
    /// `meta.copy_id` names the copy. Read its progress with a HEAD of the
    /// target, in [`ObjectMeta::copy_status`], or stop it with
    /// [`Blobs::encode_abort_copy`](crate::Blobs::encode_abort_copy).
    Pending {
        /// The metadata of the target, with the copy's ID and its status,
        /// `pending`.
        meta: ObjectMeta<'h>,
    },
    /// S3 took the copy, and says in the body whether it succeeded.
    ///
    /// This outcome is not final. Read the whole body and pass it with the
    /// head to
    /// [`s3::Objects::accept_copy_body`](crate::s3::Objects::accept_copy_body),
    /// which returns the final outcome. Azure never returns this.
    NeedResultBody {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// The condition on the target or on the source did not hold, so the
    /// service copied nothing.
    PreconditionFailed,
    /// The source does not exist. A missing container is a
    /// [`Self::ServiceFailure`] with [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The service's reason, if known.
        kind: Option<ServiceErrorKind>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the copy, or failed to carry it out.
    ///
    /// A copy conditional on `If-None-Match: *` onto an object that exists
    /// is refused here on Azure, with [`ServiceErrorKind::AlreadyExists`].
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for CopyHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Copied { .. } => f.write_str("the service wrote the copy"),
            Self::Pending { .. } => f.write_str("the copy is pending"),
            Self::NeedResultBody { .. } => f.write_str("the result follows in the response body"),
            Self::PreconditionFailed => f.write_str("a precondition on the request did not hold"),
            Self::NotFound { kind } => f.write_str(kind.map_or(
                "the source or the container does not exist",
                ServiceErrorKind::as_str,
            )),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

/// The result of reading the response head of a restore: an S3
/// RestoreObject, or an Azure Set Blob Tier out of the archive.
///
/// A head that reports a failure is one of these too. The methods that read
/// it return an [`Err`] only for a head they cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RestoreHeadOutcome<'h> {
    /// The service started the restore, which takes hours. Follow it with a
    /// HEAD, in [`ObjectMeta::restore_status`].
    Started,
    /// The object is readable already: S3 holds a restored copy, whose time
    /// it extended, or Azure moved an object that was not archived at once.
    Readable,
    /// The object does not exist. A missing container is a
    /// [`Self::ServiceFailure`] with [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The service's reason, if known.
        kind: Option<ServiceErrorKind>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the restore.
    ///
    /// S3 refuses a restore that is running with 409
    /// `RestoreAlreadyInProgress`, and one of an object that is not archived
    /// with 403 `InvalidObjectState`. Azure refuses a tier change while it
    /// rehydrates an object with 409.
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for RestoreHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Started => f.write_str("the service started the restore"),
            Self::Readable => f.write_str("the object is readable"),
            Self::NotFound { kind } => not_found(f, *kind),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

/// The result of reading the response head of a request that changes what
/// the service stores about an object, and returns nothing: setting its tags
/// or, on Azure, its access tier.
///
/// A head that reports a failure is one of these too. The methods that read
/// it return an [`Err`] only for a head they cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdateHeadOutcome<'h> {
    /// The service made the change.
    Updated,
    /// The object does not exist. A missing container is a
    /// [`Self::ServiceFailure`] with [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The service's reason, if known.
        kind: Option<ServiceErrorKind>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the request.
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for UpdateHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Updated => f.write_str("the service made the change"),
            Self::NotFound { kind } => not_found(f, *kind),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

/// The result of reading the response head of a read of an object's tags:
/// an Azure Get Blob Tags, or an S3 GetObjectTagging.
///
/// A head that reports a failure is one of these too. The methods that read
/// it return an [`Err`] only for a head they cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TagsHeadOutcome<'h> {
    /// The tags follow in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`Blobs::fill_tags`](crate::Blobs::fill_tags) or
    /// [`s3::Objects::fill_tags`](crate::s3::Objects::fill_tags).
    Tags {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// The object does not exist. A missing container is a
    /// [`Self::ServiceFailure`] with [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The service's reason, if known.
        kind: Option<ServiceErrorKind>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the request.
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for TagsHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tags { .. } => f.write_str("the tags follow in the response body"),
            Self::NotFound { kind } => not_found(f, *kind),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

/// The result of reading the response head of a removal of several objects
/// in one request: an Azure Blob Batch, or an S3 DeleteObjects.
///
/// A head that reports a failure is one of these too. The methods that read
/// it return an [`Err`] only for a head they cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeleteManyHeadOutcome<'h> {
    /// The result of each removal follows in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`Blobs::fill_delete_results`](crate::Blobs::fill_delete_results) or
    /// [`s3::Objects::fill_delete_results`](crate::s3::Objects::fill_delete_results).
    Results {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the request as a whole, such as for a container
    /// that does not exist, with [`ServiceErrorKind::NoSuchContainer`].
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for DeleteManyHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Results { .. } => f.write_str("the results follow in the response body"),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

/// The result of reading the response head of a commit: an Azure Put Block
/// List, or an S3 CompleteMultipartUpload.
///
/// A head that reports a failure is one of these too. The methods that read
/// it return an [`Err`] only for a head they cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommitHeadOutcome<'h> {
    /// The object is committed.
    Committed {
        /// The committed object's metadata.
        meta: ObjectMeta<'h>,
    },
    /// The service took the commit, and says in the body whether it
    /// succeeded.
    ///
    /// S3 answers a commit with status 200 before it has finished, and then
    /// writes either the result or an error into the body. This outcome is
    /// not final. Read the whole body and pass it with the head to
    /// [`s3::Objects::accept_commit_parts_body`](crate::s3::Objects::accept_commit_parts_body),
    /// which returns the final outcome. Azure never returns this.
    NeedResultBody {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// The commit's condition failed.
    PreconditionFailed,
    /// On S3, the upload does not exist. A missing container is a
    /// [`Self::ServiceFailure`] with [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The service's reason, if known.
        kind: Option<ServiceErrorKind>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the request.
    ///
    /// A list of parts that the service cannot commit is refused here, with
    /// [`ServiceErrorKind::InvalidUpload`].
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for CommitHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Committed { .. } => f.write_str("the object is committed"),
            Self::NeedResultBody { .. } => f.write_str("the result follows in the response body"),
            Self::PreconditionFailed => f.write_str("a precondition on the request did not hold"),
            Self::NotFound { kind } => not_found(f, *kind),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

/// The result of reading the response head of a listing of parts: an Azure
/// Get Block List, or an S3 ListParts.
///
/// A head that reports a failure is one of these too. The methods that read
/// it return an [`Err`] only for a head they cannot read.
// The crate allocates nothing and its outcomes are `Copy`, so the variant
// that carries the metadata cannot be boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ListPartsHeadOutcome<'h> {
    /// The parts follow in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`Blobs::fill_blocks`](crate::Blobs::fill_blocks) or
    /// [`s3::Objects::fill_parts`](crate::s3::Objects::fill_parts).
    #[non_exhaustive]
    Parts {
        /// Metadata of the committed object, if any. Only Azure sends it.
        meta: ObjectMeta<'h>,
        /// The result body's byte length.
        expected_len: Option<u64>,
    },
    /// The object does not exist, or on S3 the upload does not. A missing
    /// container is a [`Self::ServiceFailure`] with
    /// [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The service's reason, if known.
        kind: Option<ServiceErrorKind>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the request.
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for ListPartsHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parts { .. } => f.write_str("the parts follow in the response body"),
            Self::NotFound { kind } => not_found(f, *kind),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

// What a `NotFound` of an operation on parts writes: what the service named
// as missing, or all that can be.
fn not_found(f: &mut fmt::Formatter<'_>, kind: Option<ServiceErrorKind>) -> fmt::Result {
    match kind {
        Some(
            kind @ (ServiceErrorKind::NotFound
            | ServiceErrorKind::NoSuchContainer
            | ServiceErrorKind::NoSuchUpload),
        ) => f.write_str(kind.as_str()),
        _ => f.write_str("the object, the upload or the container does not exist"),
    }
}

/// Object metadata borrowed from a response head.
///
/// Each field holds the bytes that the service sent. `last_modified` holds
/// them as text. To read it as an instant, use
/// [`layered::http_date_ms`](crate::layered::http_date_ms).
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
    pub last_modified: Option<&'h str>,
    /// The version identifier, if the service returned one.
    pub version: Option<&'h [u8]>,
    /// The value of the `Content-Encoding` header, if the service returned
    /// one.
    ///
    /// This crate does not decode the body. It returns this value so that you
    /// know how the bytes are encoded.
    pub content_encoding: Option<&'h [u8]>,
    /// The value of the `Content-Type` header, without an inferred default.
    pub content_type: Option<&'h [u8]>,
    /// The base64 of the MD5 that the service stores for the object, from
    /// the `Content-MD5` header.
    pub content_md5: Option<&'h [u8]>,
    /// The value of the `Content-Language` header.
    pub content_language: Option<&'h [u8]>,
    /// The value of the `Content-Disposition` header.
    pub content_disposition: Option<&'h [u8]>,
    /// The value of the `Cache-Control` header.
    pub cache_control: Option<&'h [u8]>,
    /// The storage class on S3, or the access tier on Azure.
    pub storage_class: Option<&'h [u8]>,
    /// The identifier of the last copy onto the object, from
    /// `x-ms-copy-id`. Azure only.
    pub copy_id: Option<&'h [u8]>,
    /// The state of the last copy onto the object, from
    /// `x-ms-copy-status`: `pending`, `success`, `aborted` or `failed`.
    /// Azure only.
    pub copy_status: Option<&'h [u8]>,
    /// The state of a restore from an archive, as the service writes it.
    ///
    /// S3 writes `x-amz-restore`: `ongoing-request="true"` while it
    /// restores, then `ongoing-request="false", expiry-date="…"`. Azure
    /// writes `x-ms-archive-status` while it rehydrates, such as
    /// `rehydrate-pending-to-hot`, and nothing once it is done.
    pub restore_status: Option<&'h [u8]>,
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
/// Use this to decide whether to retry a request, and how. This crate never
/// retries a request itself. For the specific error that the service named,
/// read [`ServiceErrorKind`].
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
/// failure. Pass it back to the `accept_*_error_body` method of the same
/// operation, such as
/// [`Blobs::accept_get_error_body`](crate::Blobs::accept_get_error_body).
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
    ///
    /// A bounded range that runs past the end of the object is served
    /// clipped to the end, and this outcome reports it as any other body:
    /// `body.expected_len` is then shorter than the range. Compare the two if
    /// a short read matters to you, such as when you read at offsets from an
    /// object size you recorded.
    Body {
        /// The metadata from the head.
        meta: ObjectMeta<'h>,
        /// Where the bytes of the body belong.
        body: BodyWindow,
    },
    /// No body follows and the request is complete.
    ///
    /// A [`GetKind::Head`](crate::GetKind::Head) plan ends here.
    Complete {
        /// The metadata from the head.
        meta: ObjectMeta<'h>,
    },
    /// The `If-None-Match` or `If-Modified-Since` condition held, so the
    /// service sent no body.
    NotModified {
        /// The entity tag, if the service repeated it.
        e_tag: Option<&'h [u8]>,
    },
    /// The `If-Match` or `If-Unmodified-Since` condition did not hold, so
    /// the service sent no body.
    PreconditionFailed,
    /// The object does not exist.
    ///
    /// A missing container is not this: it is a [`Self::ServiceFailure`] with
    /// [`ServiceErrorKind::NoSuchContainer`]. One exception is S3's answer
    /// to a HEAD, which carries no body: S3 answers a missing bucket with the
    /// same bare 404 as a missing key, so this outcome, with `kind` [`None`],
    /// can mean either.
    NotFound {
        /// The error, if the service named it.
        kind: Option<ServiceErrorKind>,
    },
    /// The service cannot serve the requested range.
    ///
    /// An empty object has no byte to serve, so the services answer any range
    /// of one with this. Read an object that may be empty whole.
    RangeNotSatisfiable {
        /// The size of the object, if `Content-Range: bytes */N` states it.
        object_size: Option<u64>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
    /// [`Blobs::accept_get_error_body`](crate::Blobs::accept_get_error_body). That
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
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
    /// [`Blobs::accept_put_error_body`](crate::Blobs::accept_put_error_body).
    /// That call returns the final outcome. If you cannot read the body, pass
    /// an empty one and the error stays unnamed.
    ///
    /// The `kind` of this failure is always [`None`].
    NeedErrorBody(Failure<'h>),
    /// The service refused the write, or it failed to store the object.
    ///
    /// A container that does not exist is refused here, with
    /// [`ServiceErrorKind::NoSuchContainer`].
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
    /// The condition did not hold, so the service removed nothing.
    PreconditionFailed,
    /// The object does not exist, so there was nothing to remove.
    ///
    /// A caller that removes an object it does not need can treat this as
    /// success. This crate does not decide that for you. A missing container
    /// is not this: it is a [`Self::ServiceFailure`] with
    /// [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The specific error, if the head names one.
        kind: Option<ServiceErrorKind>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
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
/// A head that reports a failure is one of these too;
/// [`Blobs::accept_list_head`](crate::Blobs::accept_list_head) and
/// [`s3::Objects::accept_list_head`](crate::s3::Objects::accept_list_head)
/// return an [`Err`] only for a head they cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ListHeadOutcome<'h> {
    /// The page follows in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`Blobs::fill_listing`](crate::Blobs::fill_listing) or
    /// [`s3::Objects::fill_listing`](crate::s3::Objects::fill_listing),
    /// which reads the entries out of it.
    Page {
        /// The exact length of the response body, if the head states it.
        ///
        /// Size the body buffer from this. S3 often leaves it out, so cap
        /// what you read when it is [`None`].
        expected_len: Option<u64>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
    /// [`Blobs::accept_list_error_body`](crate::Blobs::accept_list_error_body).
    /// That call returns the final outcome. If you cannot read the body, pass
    /// an empty one and the error stays unnamed.
    ///
    /// The `kind` of this failure is always [`None`].
    NeedErrorBody(Failure<'h>),
    /// The service refused the listing, or it failed to serve it.
    ///
    /// A container that does not exist is refused here, with
    /// [`ServiceErrorKind::NoSuchContainer`]: a listing has no object to be
    /// missing, so a missing container is never an empty page.
    ServiceFailure(Failure<'h>),
}

/// What one page of a listing held.
///
/// [`Blobs::fill_listing`](crate::Blobs::fill_listing) and
/// [`s3::Objects::fill_listing`](crate::s3::Objects::fill_listing) return
/// this once the page has been read to its end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Listing<'b> {
    /// The number of entries that this call wrote into your array.
    ///
    /// The entries after these are untouched.
    pub filled: usize,
    /// Where the next page starts, or [`None`] when the listing is complete.
    ///
    /// Copy this text into your own storage and pass it as
    /// [`PhysicalList::marker`](crate::PhysicalList::marker); the next page
    /// overwrites the body it borrows.
    ///
    /// A page names a next one whenever more keys follow, even if it reported
    /// fewer entries than it asked for.
    pub next_marker: Option<&'b str>,
    /// Where in the versions of the next marker's key the next page starts,
    /// in an S3 listing of versions. Pass it as
    /// [`PhysicalList::version_marker`](crate::PhysicalList::version_marker)
    /// beside the marker. [`None`] on every other listing.
    pub next_version_marker: Option<&'b str>,
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
    /// The named parts do not match what the service can commit.
    InvalidUpload = 10,
    /// The upload does not exist: it was committed or aborted, or it never
    /// existed. Only S3 keeps uploads.
    NoSuchUpload = 11,
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
            Self::InvalidUpload => "the service refused the upload's parts",
            Self::NoSuchUpload => "the upload does not exist",
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
            10 => Self::InvalidUpload,
            11 => Self::NoSuchUpload,
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
            Self::PreconditionFailed => f.write_str("the condition on the read did not hold"),
            Self::NotFound { .. } => f.write_str(ServiceErrorKind::NotFound.as_str()),
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
