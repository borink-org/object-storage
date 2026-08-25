/// Metadata borrowed from an Azure response head.
///
/// Values are the bytes Azure sent. Turning `last_modified` into an instant is
/// arithmetic over a public value, so it lives in
/// [`layered::http_date_ms`](crate::layered::http_date_ms) rather than here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObjectMeta<'h> {
    /// Total object size when the head stated one, not the returned length.
    pub size: Option<u64>,
    /// Entity tag when Azure returned one.
    pub e_tag: Option<&'h [u8]>,
    /// `Last-Modified` as Azure spelled it.
    pub last_modified: Option<&'h [u8]>,
    /// Azure blob version identifier when returned.
    pub version: Option<&'h [u8]>,
    /// `Content-Encoding` when present: passthrough metadata, not a transform.
    pub content_encoding: Option<&'h [u8]>,
}

/// Where the incoming body bytes belong.
///
/// Offsets are defined over the *stored* bytes of the object, which is also
/// HTTP's selected representation for Azure Blob Storage. The transport must
/// therefore deliver the body transfer-decoded but not content-decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyWindow {
    /// Stored-byte offset of the first wire-body byte.
    pub object_offset: u64,
    /// Exact wire length when the head states one.
    pub expected_len: Option<u64>,
    /// Total object size when known.
    pub object_size: Option<u64>,
}

/// The retry-relevant taxonomy of a service failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureClass {
    /// Credentials or authorization were rejected.
    Auth,
    /// The request was throttled and may be retried later.
    Throttled,
    /// Azure failed or was unavailable.
    Server,
    /// Azure answered with a redirect, which is surfaced, not followed.
    Redirect,
    /// Anything else, including malformed requests.
    Other,
}

/// Every response Azure actually sends maps to one of these.
///
/// A scheduler branches on this; `Err` is reserved for heads that are
/// unparseable, self-contradictory, or disagree with the plan they answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GetHeadOutcome<'h> {
    /// A body follows and belongs at this window.
    Body {
        /// Metadata from the head.
        meta: ObjectMeta<'h>,
        /// Where the body bytes belong.
        body: BodyWindow,
    },
    /// The exchange is complete without a body, as for a metadata plan.
    Complete(ObjectMeta<'h>),
    /// The `If-None-Match` condition held.
    NotModified {
        /// The entity tag, when Azure repeated it.
        etag: Option<&'h [u8]>,
    },
    /// The `If-Match` condition did not hold.
    PreconditionFailed,
    /// The object does not exist.
    NotFound,
    /// Azure could not satisfy the requested range.
    RangeNotSatisfiable {
        /// The object size, when `Content-Range: bytes */N` carried it.
        object_size: Option<u64>,
    },
    /// Azure refused or failed to serve the request.
    ServiceFailure {
        /// The HTTP status code.
        status: u16,
        /// What a scheduler needs to decide about retrying.
        class: FailureClass,
        /// Azure's request identifier, for support and correlation.
        request_id: Option<&'h [u8]>,
    },
}
