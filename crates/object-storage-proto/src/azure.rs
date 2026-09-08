//! Azure Blob Storage requests and responses.
//!
//! Put Block From URL and structured-body framing are not implemented.

use crate::request::{HeadWriter, U64Decimal, Writer};
use crate::{
    BodyWindow, Classification, CommitBlocksHeadOutcome, CommitBlocksShape, ConditionKind,
    DeleteHeadOutcome, DeleteKind, DeleteShape, Error, Failure, FailureClass, GetHeadOutcome,
    GetKind, GetShape, HeaderSpan, InvalidPlan, ListBlocksHeadOutcome, ListEntry, ListHeadOutcome,
    Listing, Method, ObjectMeta, Payload, PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut,
    PropertySet, PropertyValues, PutHeadOutcome, PutShape, RequestedRange, ResponseFault,
    ResponseHead, Result, ServiceErrorKind, StageBlockHeadOutcome, Timestamps, WireRequest,
};

/// The most recent Azure Storage version that every region supports.
///
/// See the [Azure Storage service version lifecycle](https://learn.microsoft.com/en-us/rest/api/storageservices/versioning-for-the-azure-storage-services).
pub const VERSION: &str = "2026-04-06";

// A flat-namespace account limits blob names to 1,024 characters.
// Azure counts a blob name in UTF-16 code units, so a character outside the
// basic plane counts twice. Measured: a name of 1024 two-byte characters is
// taken and one of 541 four-byte characters, which is 1041 code units, is
// refused with 400. See the live suite.
//
// A hierarchical-namespace account has no such limit. Measured: it stored a
// key of 32,689 units and read it back, and refused a longer one with 414, a
// request line too long, at 32,759 bytes of encoded URL. That
// bound is the URL's, which the service checks. This crate applies the flat
// limit only when told the account is flat; a client that does not know sends
// the key, and the service answers for the account it is.
const MAX_BLOB_NAME_UNITS: usize = 1024;

// The most `/`-delimited segments Azure takes in a name. Its documentation
// gives 254; measurement gives this.
const MAX_BLOB_NAME_SEGMENTS: usize = 255;

/// An Azure Blob endpoint and container name, both borrowed.
#[derive(Debug, Clone, Copy)]
pub struct Container<'a> {
    endpoint: &'a str,
    name: &'a str,
}

impl<'a> Container<'a> {
    /// Creates a container reference from an origin and a container name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidEndpoint`] if `endpoint` is not an ASCII HTTP
    /// or HTTPS origin.
    ///
    /// Returns [`Error::InvalidContainer`] if `name` is empty, or if it
    /// contains bytes that would change the structure of the request.
    pub fn new(endpoint: &'a str, name: &'a str) -> Result<Self> {
        if !crate::http::valid_http_origin(endpoint) {
            return Err(Error::InvalidEndpoint);
        }
        if name.is_empty()
            || name
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_control())
        {
            return Err(Error::InvalidContainer);
        }
        Ok(Self { endpoint, name })
    }
}

/// The Azure Blob operations that one bearer token authorizes.
///
/// This is a small borrowed value, and it is [`Copy`]. Create it once per
/// token and keep it while the token is valid. It borrows the endpoint, the
/// container name and the token, so it cannot live in the value that owns
/// those strings. Keep the strings in one value and this in a second value
/// that borrows the first.
///
/// You can also create it again for every request. Each creation checks the
/// token as a header value, which is a scan of its bytes.
///
/// Every method that encodes a request takes the current time in `now`,
/// because this crate never reads the clock.
#[derive(Clone, Copy)]
pub struct Blobs<'a> {
    container: Container<'a>,
    token: &'a str,
    namespace: AzureNamespace,
}

impl core::fmt::Debug for Blobs<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Blobs")
            .field("container", &self.container)
            .field("token", &"<redacted>")
            .field("namespace", &self.namespace)
            .finish()
    }
}

/// The kind of storage account that a client talks to.
///
/// [`Blobs::with_namespace`] sets it and [`InvalidPlan::azure_rejection`]
/// reads it. This crate never discovers it: no request or response says
/// which kind of account answered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u16)]
pub enum AzureNamespace {
    /// The account's namespace is not known. A client refuses only what
    /// both kinds of account refuse, and sends the rest.
    #[default]
    Unknown = 0,
    /// A flat-namespace account.
    Flat = 1,
    /// A hierarchical-namespace account.
    Hierarchical = 2,
}

/// Azure's corresponding rejection for one local validation failure.
///
/// No request was sent. Other failures, such as authentication, may take
/// precedence if the request is sent. This is not a received response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AzureRejection {
    /// The corresponding HTTP status.
    pub status: u16,
    /// The service error code, stored in static memory.
    pub code: &'static str,
}

impl InvalidPlan {
    /// Returns the known Azure rejection corresponding to this reason.
    ///
    /// Returns `None` when no mapping is established for the namespace,
    /// including names or ranges Azure may accept with unwanted behavior.
    /// The local reason remains authoritative; this predicts neither error
    /// precedence nor the outcome of a request containing other faults.
    ///
    /// ```
    /// use borink_object_storage_proto::{AzureNamespace, InvalidPlan};
    /// let reason = InvalidPlan::KeyTooLong;
    /// let azure = reason.azure_rejection(AzureNamespace::Flat).unwrap();
    /// assert_eq!((azure.status, azure.code), (400, "OutOfRangeInput"));
    /// assert_eq!(reason.azure_rejection(AzureNamespace::Hierarchical), None);
    /// ```
    pub const fn azure_rejection(self, namespace: AzureNamespace) -> Option<AzureRejection> {
        let code = match (self, namespace) {
            (Self::MaxResults, _) => "OutOfRangeQueryParameterValue",
            (Self::KeyTooLong, AzureNamespace::Flat) => "OutOfRangeInput",
            _ => return None,
        };
        Some(AzureRejection { status: 400, code })
    }
}

/// Which stored version of a block Put Block List must use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u16)]
pub enum BlockSource {
    /// Require a staged block.
    Uncommitted = 1,
    /// Reuse a block from the current committed object.
    Committed = 2,
    /// Prefer staged data, otherwise use committed data.
    #[default]
    Latest = 3,
}

impl BlockSource {
    /// Decodes a native selector from a binding.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::Uncommitted),
            2 => Some(Self::Committed),
            3 => Some(Self::Latest),
            _ => None,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Uncommitted => "Uncommitted",
            Self::Committed => "Committed",
            Self::Latest => "Latest",
        }
    }
}

/// One entry of the ordered block list that a commit writes.
///
/// The same ID may appear more than once, and entries with different
/// [`BlockSource`]s may be mixed in one list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockRef<'a> {
    /// The block ID, as the base64 text that the service stores.
    ///
    /// Choose the bytes yourself and write them with
    /// [`layered::block_id`](crate::layered::block_id), or pass a listed
    /// [`Block::id`] unchanged. The encoder checks the text: it is not empty,
    /// and it is at most 88 characters of standard base64 with at most two `=`
    /// of padding. That is at most 64 decoded bytes. The encoder percent-encodes
    /// it into the query itself. Only the service checks that every block of one
    /// blob decodes to the same length, and that a referenced block exists.
    pub id: &'a str,
    /// The stored version to select.
    pub source: BlockSource,
}

/// Which blocks to enumerate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum BlockListKind {
    /// Blocks staged but not yet committed.
    #[default]
    Staged = 1,
    /// Blocks of the committed object; empty before the first commit.
    Committed = 2,
    /// Both lists; succeeds even when only staged blocks exist.
    All = 3,
}

impl BlockListKind {
    /// Returns the kind with this discriminant, or `None` for an unknown one.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Staged,
            2 => Self::Committed,
            3 => Self::All,
            _ => return None,
        })
    }
}

/// Whether a listed block belongs to the committed object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum BlockState {
    /// Staged and not yet committed.
    #[default]
    Staged = 1,
    /// Block of the committed object.
    Committed = 2,
}

impl BlockState {
    /// Returns the state with this discriminant, or `None` for an unknown one.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Staged,
            2 => Self::Committed,
            _ => return None,
        })
    }
}

/// One block, borrowing the response body after in-place decoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Block<'b> {
    /// The identifier to pass unchanged to [`BlockRef::id`].
    pub id: &'b str,
    /// The block's byte length.
    pub size: u64,
    /// The list that held this block.
    pub state: BlockState,
}

/// A native Put Block plan: stage one block of an object.
#[derive(Debug, Clone, Copy)]
pub struct PhysicalStageBlock<'a> {
    /// Object key.
    pub key: &'a str,
    /// The block ID, as base64 text. See [`BlockRef::id`] for the rules.
    pub id: &'a str,
}

/// A native Put Block List plan: publish an ordered list of blocks as the
/// object.
#[derive(Debug, Clone, Copy)]
pub struct PhysicalCommitBlocks<'a> {
    /// Object key.
    pub key: &'a str,
    /// Object precondition.
    pub condition: ConditionKind,
    /// ETag or wildcard for the precondition.
    pub condition_value: Option<&'a [u8]>,
}

impl<'a> PhysicalCommitBlocks<'a> {
    /// An unconditional block-list write.
    pub const fn new(key: &'a str) -> Self {
        Self {
            key,
            condition: ConditionKind::None,
            condition_value: None,
        }
    }

    /// The part of the plan that reading the response needs.
    pub const fn shape(&self) -> CommitBlocksShape {
        CommitBlocksShape {
            condition: self.condition,
        }
    }
}

/// A native Get Block List plan: read which blocks an object holds.
#[derive(Debug, Clone, Copy)]
pub struct PhysicalListBlocks<'a> {
    /// Object key.
    pub key: &'a str,
    /// Committed, staged, or both lists.
    pub kind: BlockListKind,
    /// Optional snapshot target; mutually exclusive with version.
    pub snapshot: Option<&'a str>,
    /// Optional version target.
    pub version: Option<&'a str>,
}

impl<'a> PhysicalListBlocks<'a> {
    /// A read of the current object's blocks of `kind`.
    pub const fn new(key: &'a str, kind: BlockListKind) -> Self {
        Self {
            key,
            kind,
            snapshot: None,
            version: None,
        }
    }
}

impl<'a> Blobs<'a> {
    /// Writes the request head for an Azure Get Block List into `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an Azure request,
    /// or if it names both a snapshot and a version. Returns
    /// [`Error::Capacity`] as [`Blobs::encode_get`] does.
    pub fn encode_list_blocks<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalListBlocks<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_block_key(plan.key, self.namespace)?;
        if (plan.snapshot.is_some() && plan.version.is_some())
            || plan.snapshot.is_some_and(str::is_empty)
            || plan.version.is_some_and(str::is_empty)
        {
            return Err(InvalidPlan::Option.into());
        }
        let kind = match plan.kind {
            BlockListKind::Committed => "committed",
            BlockListKind::Staged => "uncommitted",
            BlockListKind::All => "all",
        };
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &[
                Some(("comp", QueryValue::Literal("blocklist"))),
                Some(("blocklisttype", QueryValue::Literal(kind))),
                plan.snapshot
                    .map(|value| ("snapshot", QueryValue::Encoded(value.as_bytes()))),
                plan.version
                    .map(|value| ("versionid", QueryValue::Encoded(value.as_bytes()))),
            ],
            RequestedRange::Whole,
            now,
        );
        encoded(head, Method::Get, Payload::Slice(&[]))
    }

    /// Writes the request head for an Azure Put Block into `buf`.
    ///
    /// The head states the length of `content`, which stays where you put it,
    /// as for [`Blobs::encode_put`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an Azure request,
    /// or if `content` is longer than one block may be. Returns
    /// [`Error::Capacity`] as [`Blobs::encode_put`] does, or call
    /// [`layered::stage_block_requirements`](crate::layered::stage_block_requirements)
    /// first.
    pub fn encode_stage_block<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalStageBlock<'_>,
        content: Payload<'r>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_block_key(plan.key, self.namespace)?;
        validate_block_id(plan.id)?;
        if content.len() > MAX_STAGE_LEN {
            return Err(InvalidPlan::PayloadTooLarge.into());
        }
        let mut head = HeadWriter::new(buf, headers);
        let query = [
            Some(("comp", QueryValue::Literal("block"))),
            Some(("blockid", QueryValue::Encoded(plan.id.as_bytes()))),
        ];
        self.build(
            &mut head,
            Some(plan.key),
            &query,
            RequestedRange::Whole,
            now,
        );
        head.header("content-length", |out| {
            out.push(U64Decimal::new(content.len()).as_bytes())
        });
        encoded(head, Method::Put, content)
    }

    /// Writes an Azure Put Block List into `buf`: the head, then the XML
    /// body after it.
    ///
    /// `blocks` become the object in this order, each looked up where its
    /// [`BlockSource`] says. The body is written into `buf` after the head,
    /// so [`WireRequest::body_span`] names it and [`WireRequest::payload`]
    /// borrows it; send both before reusing `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an Azure request,
    /// if `blocks` holds more than 50,000 entries or an ID that fails the
    /// checks on [`BlockRef::id`]. Returns [`Error::Capacity`] with the bytes
    /// that the head and the body need together, or call
    /// [`layered::commit_blocks_requirements`](crate::layered::commit_blocks_requirements)
    /// first.
    pub fn encode_commit_blocks<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCommitBlocks<'_>,
        blocks: &[BlockRef<'_>],
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        self.encode_commit_blocks_from_iter(
            buf,
            headers,
            plan,
            blocks.iter().map(|block| (block.id, block.source)),
            now,
        )
    }

    /// [`Blobs::encode_commit_blocks`] over block references that are not in
    /// one array.
    ///
    /// Use this when the references are produced rather than stored: from an
    /// array in another language, or from an ID derived per index. No second
    /// array of references is built. This method still copies each ID into
    /// the body.
    ///
    /// `blocks` is traversed twice: once to validate the IDs and size the
    /// body, once to write it. Every traversal must yield the same items in
    /// the same order, because the head states the body length that the first
    /// traversal measured.
    ///
    /// # Errors
    ///
    /// As [`Blobs::encode_commit_blocks`].
    pub fn encode_commit_blocks_from_iter<'r, I>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCommitBlocks<'_>,
        blocks: impl Iterator<Item = (I, BlockSource)> + Clone,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>>
    where
        I: AsRef<str>,
    {
        validate_block_key(plan.key, self.namespace)?;
        validate_condition(plan.condition, plan.condition_value)?;
        let mut length = COMMIT_OPEN.len() + COMMIT_CLOSE.len();
        for (index, (id, source)) in blocks.clone().enumerate() {
            if index >= MAX_BLOCKS {
                return Err(InvalidPlan::Blocks.into());
            }
            validate_block_id(id.as_ref())?;
            // At most 50,000 IDs of 88 bytes and tags of 11 bytes: < 6 MiB,
            // including the wrapper, so this fits usize on our 32-bit targets.
            length += 5 + 2 * source.tag().len() + id.as_ref().len();
        }
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &[Some(("comp", QueryValue::Literal("blocklist")))],
            RequestedRange::Whole,
            now,
        );
        head.header("content-length", |out| {
            out.push(U64Decimal::new(length as u64).as_bytes())
        });
        push_condition(&mut head, plan.condition, plan.condition_value);
        let body = head.body(|out| {
            out.push(COMMIT_OPEN);
            for (id, source) in blocks {
                out.push(b"<");
                out.push(source.tag().as_bytes());
                out.push(b">");
                out.push(id.as_ref().as_bytes());
                out.push(b"</");
                out.push(source.tag().as_bytes());
                out.push(b">");
            }
            out.push(COMMIT_CLOSE);
        });
        let capacity = head.capacity();
        head.finish_with_body(Method::Put, Payload::Slice(&[]), Some(body))
            .ok_or_else(|| capacity_error(capacity))
    }

    /// Reads a Get Block List body whole into `into`.
    ///
    /// Both sections are read in the order the service wrote them, and each
    /// entry names its section in [`Block::state`]. IDs are decoded in place,
    /// so the entries borrow `body`, and the body cannot be read again. On an
    /// error nothing in `into` is meaningful; fetch the body again before
    /// retrying.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`] with the number of blocks the body holds if
    /// `into` is shorter than that;
    /// [`layered::max_blocks_in`](crate::layered::max_blocks_in) bounds it
    /// from the body's length. Returns [`Error::Response`] if the body is not
    /// a block list.
    pub fn fill_blocks<'b, E: From<Block<'b>>>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
    ) -> Result<Listing<'b>> {
        crate::xml::azure_blocks::fill_blocks(body, into)
    }

    /// Reads the head that answers a stage.
    ///
    /// Every head that Azure sends becomes a [`StageBlockHeadOutcome`],
    /// including the heads that report a failure. If the head names no error
    /// code, the outcome is [`StageBlockHeadOutcome::NeedErrorBody`]: read the
    /// body and pass it to [`Self::accept_stage_block_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status that a stage never returns is [`ResponseFault::Status`].
    pub fn accept_stage_block_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<StageBlockHeadOutcome<'h>> {
        match head.status {
            201 => Ok(StageBlockHeadOutcome::Staged),
            404 if head.error_code.is_none() => Ok(StageBlockHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(StageBlockHeadOutcome::NotFound { kind: named(&head) }),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(StageBlockHeadOutcome::NeedErrorBody(
                failure(status, None, head.request_id),
            )),
            status => Ok(StageBlockHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a missing error code with the response body.
    pub fn accept_stage_block_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> StageBlockHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => StageBlockHeadOutcome::NotFound { kind },
            status => StageBlockHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Reads the head that answers a commit.
    ///
    /// Pass the `shape` that [`PhysicalCommitBlocks::shape`] gave you before
    /// the request. A failed condition is reported as
    /// [`CommitBlocksHeadOutcome::PreconditionFailed`] only if that plan
    /// carried a condition. Otherwise a 412 is a service failure that names
    /// its code.
    ///
    /// Every head that Azure sends becomes a [`CommitBlocksHeadOutcome`],
    /// including the heads that report a failure. If the head names no error
    /// code, the outcome is [`CommitBlocksHeadOutcome::NeedErrorBody`]: read the
    /// body and pass it to [`Self::accept_commit_blocks_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status that a commit never returns is [`ResponseFault::Status`].
    pub fn accept_commit_blocks_head<'h>(
        &self,
        shape: CommitBlocksShape,
        head: ResponseHead<'h>,
    ) -> Result<CommitBlocksHeadOutcome<'h>> {
        match head.status {
            201 => Ok(CommitBlocksHeadOutcome::Committed {
                meta: multipart_meta(head)?,
            }),
            412 if shape.condition != ConditionKind::None
                && named(&head) == Some(ServiceErrorKind::Precondition) =>
            {
                Ok(CommitBlocksHeadOutcome::PreconditionFailed)
            }
            404 if head.error_code.is_none() => Ok(CommitBlocksHeadOutcome::NeedErrorBody(
                failure(404, None, head.request_id),
            )),
            404 => Ok(CommitBlocksHeadOutcome::NotFound { kind: named(&head) }),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(CommitBlocksHeadOutcome::NeedErrorBody(
                failure(status, None, head.request_id),
            )),
            status => Ok(CommitBlocksHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a missing error code with the response body.
    pub fn accept_commit_blocks_error_body<'h>(
        &self,
        shape: CommitBlocksShape,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> CommitBlocksHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            412 if shape.condition != ConditionKind::None
                && kind == Some(ServiceErrorKind::Precondition) =>
            {
                CommitBlocksHeadOutcome::PreconditionFailed
            }
            404 => CommitBlocksHeadOutcome::NotFound { kind },
            status => CommitBlocksHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Reads the head that answers a block listing.
    ///
    /// Every head that Azure sends becomes a [`ListBlocksHeadOutcome`],
    /// including the heads that report a failure. If the head names no error
    /// code, the outcome is [`ListBlocksHeadOutcome::NeedErrorBody`]: read the
    /// body and pass it to [`Self::accept_list_blocks_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`], and a
    /// `Content-Length` that is not a number is [`ResponseFault::Head`].
    pub fn accept_list_blocks_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<ListBlocksHeadOutcome<'h>> {
        match head.status {
            200 => Ok(ListBlocksHeadOutcome::Blocks {
                meta: multipart_meta(head)?,
                expected_len: decimal_header(head.content_length)?,
            }),

            404 if head.error_code.is_none() => Ok(ListBlocksHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(ListBlocksHeadOutcome::NotFound { kind: named(&head) }),
            201..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(ListBlocksHeadOutcome::NeedErrorBody(
                failure(status, None, head.request_id),
            )),
            status => Ok(ListBlocksHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a missing error code with the response body.
    pub fn accept_list_blocks_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> ListBlocksHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => ListBlocksHeadOutcome::NotFound { kind },
            status => ListBlocksHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Creates a client from a container and a bearer token.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidToken`] if `token` is not usable as one HTTP
    /// header value.
    pub fn new(container: Container<'a>, token: &'a str) -> Result<Self> {
        if !valid_header(token.as_bytes()) {
            return Err(Error::InvalidToken);
        }
        Ok(Self {
            container,
            token,
            namespace: AzureNamespace::Unknown,
        })
    }

    /// Returns this client with the account's namespace set.
    ///
    /// A client told it is on a flat account refuses a key of more than 1,024
    /// UTF-16 code units as [`InvalidPlan::KeyTooLong`]. Any other client
    /// sends it and reports what the service answers.
    pub const fn with_namespace(mut self, namespace: AzureNamespace) -> Self {
        self.namespace = namespace;
        self
    }

    /// Writes the request head for `get` into `buf`.
    ///
    /// This method allocates nothing. It writes the URL and the header values
    /// into `buf`, records their spans in `headers`, and returns a
    /// [`WireRequest`] that borrows both buffers.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `get` cannot become an Azure
    /// request. This method validates the plan before it writes any byte, so
    /// it never reports an invalid plan as a capacity error.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or call
    /// [`layered::get_requirements`](crate::layered::get_requirements) first.
    pub fn encode_get<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        get: &PhysicalGet<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_get(get, self.namespace)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, Some(get.key), &[], get.range, now);
        push_condition(&mut head, get.condition, get.condition_value);
        let method = match get.kind {
            GetKind::Bytes => Method::Get,
            GetKind::Metadata => Method::Head,
        };
        encoded(head, method, Payload::Slice(&[]))
    }

    /// Writes the request head for `put` into `buf`.
    ///
    /// The head states the length of `content`. If you pass
    /// [`Payload::Slice`], the returned request borrows those bytes and copies
    /// none of them. If you pass [`Payload::Streamed`], the request carries no
    /// content and you send the stated number of bytes yourself.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `put` cannot become an Azure request,
    /// or if `content` is longer than Azure writes in one request. This method
    /// validates the plan before it writes any byte, so it never reports an
    /// invalid plan as a capacity error.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or call
    /// [`layered::put_requirements`](crate::layered::put_requirements) first.
    pub fn encode_put<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        put: &PhysicalPut<'_>,
        content: Payload<'r>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_put(put, content.len(), self.namespace)?;
        let length = content.len();
        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, Some(put.key), &[], RequestedRange::Whole, now);
        head.header("x-ms-blob-type", |out| out.push(b"BlockBlob"));
        // The content length is head bytes like any other, so it is written
        // into the caller's buffer rather than formatted at send time.
        head.header("content-length", |out| {
            out.push(U64Decimal::new(length).as_bytes());
        });
        push_condition(&mut head, put.condition, put.condition_value);
        encoded(head, Method::Put, content)
    }

    // The parts that every request head carries, in the order that they are
    // written into the caller's buffer. Each part is one range of that buffer.
    // `key` is `None` for a request that names the container alone, and the
    // query is written in the order it is given.
    fn build(
        &self,
        head: &mut HeadWriter<'_>,
        key: Option<&str>,
        query: &[Option<(&str, QueryValue<'_>)>],
        range: RequestedRange,
        now: &Timestamps,
    ) {
        head.url(|out| {
            out.push(self.container.endpoint.as_bytes());
            out.push(b"/");
            out.push(self.container.name.as_bytes());
            if let Some(key) = key {
                out.push(b"/");
                for part in crate::path::encode_object_key(key) {
                    out.push(part);
                }
            }
            for (index, (name, value)) in query.iter().flatten().enumerate() {
                out.push(if index == 0 { b"?" } else { b"&" });
                out.push(name.as_bytes());
                out.push(b"=");
                value.write(out);
            }
        });
        head.header("authorization", |out| {
            out.push(b"Bearer ");
            out.push(self.token.as_bytes());
        });
        head.header("x-ms-date", |out| out.push(now.rfc1123().as_bytes()));
        head.header("x-ms-version", |out| out.push(VERSION.as_bytes()));
        if range != RequestedRange::Whole {
            head.header("range", |out| write_range(out, range));
        }
    }

    /// Reads a response head and reports what to do next.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_get`]. This
    /// method checks the head against that plan, so you never restate what the
    /// plan already holds.
    ///
    /// Every head that Azure sends becomes a [`GetHeadOutcome`], including the
    /// heads that report a failure. Azure names its errors in the
    /// `x-ms-error-code` header, so this method needs no part of the response
    /// body and returns the named error with the outcome. If Azure sent no
    /// such header, the outcome names no error: call [`classify_error`] with
    /// the response body to read the error code from there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against `shape`.
    /// A `Content-Range` whose end is before its start is
    /// [`ResponseFault::Head`]. A ranged plan that Azure answers with status
    /// 200 is [`ResponseFault::Range`].
    pub fn accept_get_head<'h>(
        &self,
        shape: GetShape,
        head: ResponseHead<'h>,
    ) -> Result<GetHeadOutcome<'h>> {
        let ranged = shape.range != RequestedRange::Whole;
        match head.status {
            206 if !ranged => Err(ResponseFault::Range.into()),
            200 if ranged => Err(ResponseFault::Range.into()),
            200 | 206 => accept_success(shape, head),
            // A conditional status the plan did not ask for is a contradiction,
            // not an outcome: nothing in the plan explains it.
            304 if shape.condition != ConditionKind::IfNoneMatch => {
                Err(ResponseFault::Status.into())
            }
            304 => Ok(GetHeadOutcome::NotModified { e_tag: head.e_tag }),
            412 if shape.condition != ConditionKind::IfMatch => Err(ResponseFault::Status.into()),
            412 => Ok(GetHeadOutcome::PreconditionFailed),
            // Azure repeats the header's code in the body, so only a
            // missing header is worth a body read. A header naming a code
            // this crate does not know is already decisive.
            404 if head.error_code.is_none() => Ok(GetHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(GetHeadOutcome::NotFound { kind: named(&head) }),
            416 => Ok(GetHeadOutcome::RangeNotSatisfiable {
                object_size: match head.content_range.map(parse_content_range) {
                    None => None,
                    // `bytes */N` is the only form 416 may carry.
                    Some(Some(ContentRange::Unsatisfied { total })) => total,
                    Some(_) => return Err(ResponseFault::Head.into()),
                },
            }),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(GetHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(GetHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`GetHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// Pass the `status` and the `request_id` of that
    /// [`Failure`], and the body that you read. The body names
    /// the error, exactly as the `x-ms-error-code` header would have. Pass an
    /// empty body if you could not read one: the outcome is then final with
    /// the error unnamed.
    ///
    /// To tell a body that your read limit cut short from a body that names an
    /// error this crate does not recognize, call [`classify_error`] instead.
    pub fn accept_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> GetHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => GetHeadOutcome::NotFound { kind },
            // The body's code refines the category too, exactly as the
            // header's would have.
            status => GetHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Writes the request head for `delete` into `buf`.
    ///
    /// The request has no content. Azure removes the object it names and
    /// nothing else: see [`PhysicalDelete`] for what that excludes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `delete` cannot become an Azure
    /// request. This method validates the plan before it writes any byte, so
    /// it never reports an invalid plan as a capacity error.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or call
    /// [`layered::delete_requirements`](crate::layered::delete_requirements)
    /// first.
    pub fn encode_delete<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        delete: &PhysicalDelete<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_delete(delete, self.namespace)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, Some(delete.key), &[], RequestedRange::Whole, now);
        if let Some(value) = delete_snapshots(delete.kind) {
            head.header("x-ms-delete-snapshots", |out| out.push(value.as_bytes()));
        }
        push_condition(&mut head, delete.condition, delete.condition_value);
        encoded(head, Method::Delete, Payload::Slice(&[]))
    }

    /// Reads the response head of a removal and reports what Azure did.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_delete`]. This
    /// method checks the head against that plan, so you never restate what the
    /// plan already holds.
    ///
    /// Every head that Azure sends becomes a [`DeleteHeadOutcome`], including
    /// the heads that report a failure.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against `shape`.
    /// A success status that a removal never returns, and a failed condition
    /// on a removal that carried none, are both [`ResponseFault::Status`].
    pub fn accept_delete_head<'h>(
        &self,
        shape: DeleteShape,
        head: ResponseHead<'h>,
    ) -> Result<DeleteHeadOutcome<'h>> {
        match head.status {
            202 => Ok(DeleteHeadOutcome::Accepted),
            412 if shape.condition == ConditionKind::None => Err(ResponseFault::Status.into()),
            412 => Ok(DeleteHeadOutcome::PreconditionFailed),
            404 if head.error_code.is_none() => Ok(DeleteHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(DeleteHeadOutcome::NotFound { kind: named(&head) }),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(DeleteHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(DeleteHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`DeleteHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_error_body`] for a removal, and reads the body
    /// the same way.
    pub fn accept_delete_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> DeleteHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => DeleteHeadOutcome::NotFound { kind },
            status => DeleteHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Reads the response head of a write and reports what Azure did.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_put`]. This
    /// method checks the head against that plan, so you never restate what the
    /// plan already holds.
    ///
    /// Every head that Azure sends becomes a [`PutHeadOutcome`], including the
    /// heads that report a failure.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against `shape`.
    /// A success status that a write never returns, and a failed condition on
    /// a write that carried none, are both [`ResponseFault::Status`].
    pub fn accept_put_head<'h>(
        &self,
        shape: PutShape,
        head: ResponseHead<'h>,
    ) -> Result<PutHeadOutcome<'h>> {
        match head.status {
            201 => Ok(PutHeadOutcome::Created {
                meta: ObjectMeta {
                    size: None,
                    e_tag: head.e_tag,
                    last_modified: text_header(head.last_modified)?,
                    version: head.version,
                    content_encoding: head.content_encoding,
                    content_type: head.content_type,
                },
            }),
            // Nothing in an unconditional write explains a failed condition.
            412 if shape.condition == ConditionKind::None => Err(ResponseFault::Status.into()),
            412 => Ok(PutHeadOutcome::PreconditionFailed),
            404 if head.error_code.is_none() => Ok(PutHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(PutHeadOutcome::NotFound { kind: named(&head) }),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(PutHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(PutHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`PutHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_error_body`] for a write, and reads the body the
    /// same way.
    pub fn accept_put_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> PutHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => PutHeadOutcome::NotFound { kind },
            status => PutHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Writes the request head for one page of `list` into `buf`.
    ///
    /// The response carries the page as a document in its body: read it whole
    /// and pass it to [`Self::fill_listing`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `list` cannot become an Azure
    /// request. This method validates the plan before it writes any byte, so
    /// it never reports an invalid plan as a capacity error.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or call
    /// [`layered::list_requirements`](crate::layered::list_requirements)
    /// first.
    pub fn encode_list<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        list: &PhysicalList<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_list(list, self.namespace)?;
        // The query is written in this order every time, so a caller can
        // compare the URL byte for byte. Azure signs none of it.
        let query = [
            Some(("restype", QueryValue::Literal("container"))),
            Some(("comp", QueryValue::Literal("list"))),
            (!list.prefix.is_empty())
                .then_some(("prefix", QueryValue::Encoded(list.prefix.as_bytes()))),
            list.delimited
                .then_some(("delimiter", QueryValue::Encoded(DELIMITER))),
            list.marker
                .map(|marker| ("marker", QueryValue::Encoded(marker.as_bytes()))),
            list.max_results
                .map(|max_results| ("maxresults", QueryValue::Number(max_results))),
        ];

        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, None, &query, RequestedRange::Whole, now);
        encoded(head, Method::Get, Payload::Slice(&[]))
    }

    /// Reads the response head of a listing and reports what Azure did.
    ///
    /// A head that reports a failure is an outcome too, so it returns [`Ok`].
    /// Only a head that cannot be read is an [`Err`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status that a listing never returns is [`ResponseFault::Status`].
    pub fn accept_list_head<'h>(&self, head: ResponseHead<'h>) -> Result<ListHeadOutcome<'h>> {
        match head.status {
            200 => Ok(ListHeadOutcome::Page {
                expected_len: decimal_header(head.content_length)?,
            }),
            404 if head.error_code.is_none() => Ok(ListHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(ListHeadOutcome::NotFound { kind: named(&head) }),
            201..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(ListHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(ListHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`ListHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_error_body`] for a listing, and reads the body
    /// the same way.
    pub fn accept_list_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> ListHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => ListHeadOutcome::NotFound { kind },
            status => ListHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Reads a page out of the response body of a listing.
    ///
    /// Pass the whole body that [`ListHeadOutcome::Page`] announced and an
    /// array to write the entries into. Reading is destructive: a body that
    /// has been read is no longer a document.
    ///
    /// Your array must hold the whole page. An array of `max_results` entries
    /// always does, because the service never writes more than it asked for.
    /// The entries are written as [`ListEntry`], or as any type built from
    /// one, so a binding fills its own array directly.
    ///
    /// For fields not carried by `ListEntry`, use [`ListEntry::property`] or
    /// [`Self::fill_listing_with`], selecting [`BlobProperty`](crate::BlobProperty)
    /// values such as `VersionId`, `IsCurrentVersion` and `Snapshot`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`] if the page holds more entries than the
    /// array, with `required` set to the number it holds. The body has been
    /// decoded by then and cannot be read again: ask the service for the page
    /// again, with a larger array.
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a listing page. This reads the grammar that Azure writes, not XML
    /// at large: a namespace prefix, a reference to an entity no listing
    /// declares, an entry tag spelled with an attribute, and anything else
    /// Azure does not write are refused rather than guessed at.
    pub fn fill_listing<'b, E: From<ListEntry<'b>>>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
    ) -> Result<Listing<'b>> {
        crate::xml::fill_listing(body, into, PropertySet::default(), |entry, _| entry.into())
    }

    /// Reads a page the way [`Self::fill_listing`] does, and hands you the
    /// values of the properties in `wanted` as it goes.
    ///
    /// `build` is called once per entry, with the entry and its values, and
    /// what it returns is written into your array. So your entry type can
    /// hold the two or three properties you care about, read in the same
    /// pass as everything else. The values point into `body`, like the
    /// entry. A group of keys gives no values.
    ///
    /// Reading the page costs the same whatever the set holds, and the same
    /// as reading it without one.
    ///
    /// ```
    /// # use borink_object_storage_proto::{
    /// #     Blobs, ListEntry, PropertySet, BlobProperty, Result,
    /// # };
    /// # fn read(blobs: &Blobs<'_>, body: &mut [u8]) -> Result<()> {
    /// let wanted = PropertySet::of(&[
    ///     BlobProperty::VersionId, BlobProperty::IsCurrentVersion,
    /// ]);
    /// let mut entries = [(ListEntry::default(), None, None); 100];
    /// blobs.fill_listing_with(body, &mut entries, wanted, |entry, values| {
    ///     (entry, values.get(BlobProperty::VersionId),
    ///         values.get(BlobProperty::IsCurrentVersion))
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn fill_listing_with<'b, E>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
        wanted: PropertySet,
        build: impl FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) -> E,
    ) -> Result<Listing<'b>> {
        crate::xml::fill_listing(body, into, wanted, build)
    }
}

// Both providers group keys at `/` and at nothing else, so the delimiter is
// what a plan turns on rather than a byte that it carries.
const DELIMITER: &[u8] = b"/";

/// The most bytes that Azure stages in one `Put Block` request.
///
/// [`Blobs::encode_stage_block`] refuses a longer payload with
/// [`InvalidPlan::PayloadTooLarge`].
pub const MAX_STAGE_LEN: u64 = 4000 * 1024 * 1024;
const MAX_BLOCKS: usize = 50_000;
const COMMIT_OPEN: &[u8] = b"<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>";
const COMMIT_CLOSE: &[u8] = b"</BlockList>";

fn multipart_meta(head: ResponseHead<'_>) -> Result<ObjectMeta<'_>> {
    Ok(ObjectMeta {
        size: None,
        e_tag: head.e_tag,
        last_modified: text_header(head.last_modified)?,
        version: head.version,
        content_encoding: head.content_encoding,
        content_type: head.content_type,
    })
}

fn validate_block_key(key: &str, namespace: AzureNamespace) -> Result<()> {
    validate_key(key, namespace)
}

// The local half of the rules on `BlockRef::id`. Equal decoded lengths
// within one blob, and whether a block exists, are the service's to check.
pub(crate) fn validate_block_id(id: &str) -> Result<()> {
    let data = id.trim_end_matches('=');
    // Trimming returns a subslice, so its length cannot exceed id.len().
    let padding = id.len() - data.len();
    if id.is_empty()
        || id.len() > 88
        || !id.len().is_multiple_of(4)
        || padding > 2
        // The preceding checks establish 4 <= len <= 88 and padding <= 2.
        || id.len() / 4 * 3 - padding > 64
        || !data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    {
        return Err(InvalidPlan::BlockId.into());
    }
    Ok(())
}

// One query value, in the form that the URL writer needs it.
#[derive(Clone, Copy)]
enum QueryValue<'q> {
    // Text of this crate's own, which is already usable in a URL.
    Literal(&'q str),
    // Bytes of the caller's or the service's, which are not.
    Encoded(&'q [u8]),
    Number(u32),
}

impl QueryValue<'_> {
    fn write(self, out: &mut Writer<'_>) {
        match self {
            Self::Literal(value) => out.push(value.as_bytes()),
            Self::Encoded(value) => {
                for part in crate::path::encode_query_value(value) {
                    out.push(part);
                }
            }
            Self::Number(value) => out.push(U64Decimal::new(value as u64).as_bytes()),
        }
    }
}

// The one record that every failing head becomes, whichever operation asked.
fn failure<'h>(
    status: u16,
    kind: Option<ServiceErrorKind>,
    request_id: Option<&'h [u8]>,
) -> Failure<'h> {
    Failure {
        status,
        class: failure_class(status, kind),
        kind,
        request_id,
    }
}

fn named<'h>(head: &ResponseHead<'h>) -> Option<ServiceErrorKind> {
    kind_for_code(trim_ascii(head.error_code.unwrap_or_default()))
}

/// Borrows the native error code, preferring the header over the XML body.
/// Unknown codes are retained unchanged.
pub fn error_code<'a>(head: &ResponseHead<'a>, body: &'a [u8]) -> Option<&'a [u8]> {
    head.error_code
        .map(trim_ascii)
        .or_else(|| crate::xml::error_code(body).map(str::as_bytes))
}

/// Classifies the Azure error that `head` names, or that `body` names if the
/// head carries no error code. Set `truncated` if a read limit stopped the
/// body early.
pub fn classify_error(head: &ResponseHead<'_>, body: &[u8], truncated: bool) -> Classification {
    let code = head
        .error_code
        .map(trim_ascii)
        .or_else(|| crate::xml::error_code(body).map(|code| code.as_bytes()));
    match code.and_then(kind_for_code) {
        Some(kind) => Classification::Classified(kind),
        None if truncated => Classification::Incomplete,
        None => Classification::Unknown,
    }
}

fn accept_success<'h>(shape: GetShape, head: ResponseHead<'h>) -> Result<GetHeadOutcome<'h>> {
    let content_length = decimal_header(head.content_length)?;
    let last_modified = text_header(head.last_modified)?;
    let meta = |size| ObjectMeta {
        size,
        e_tag: head.e_tag,
        last_modified,
        version: head.version,
        content_encoding: head.content_encoding,
        content_type: head.content_type,
    };
    if head.status == 200 {
        // An unranged plan reads from byte zero, and Azure states the whole
        // object length, so `Content-Length` is both the window and the size.
        return Ok(match shape.kind {
            GetKind::Metadata => GetHeadOutcome::Complete {
                meta: meta(content_length),
            },
            GetKind::Bytes => GetHeadOutcome::Body {
                meta: meta(content_length),
                body: BodyWindow {
                    object_offset: 0,
                    expected_len: content_length,
                    object_size: content_length,
                },
            },
        });
    }
    let value = head
        .content_range
        .ok_or(Error::Response(ResponseFault::Head))?;
    let ContentRange::Satisfied { start, end, total } =
        parse_content_range(value).ok_or(Error::Response(ResponseFault::Head))?
    else {
        return Err(ResponseFault::Head.into());
    };
    // parse_content_range establishes start <= end < u64::MAX.
    let served = end - start + 1;
    if content_length.is_some_and(|length| length != served) {
        return Err(ResponseFault::Head.into());
    }
    // Azure serves the whole satisfiable range, so a short serve is a
    // mismatch: silently accepting it would hand consumers a partial read.
    let requested_start = match shape.range {
        RequestedRange::Bounded { start, .. } | RequestedRange::Offset(start) => start,
        RequestedRange::Whole | RequestedRange::Suffix(_) => {
            // Public shapes need not have passed through encode_get.
            return Err(ResponseFault::Range.into());
        }
    };
    if start != requested_start {
        return Err(ResponseFault::Range.into());
    }
    if let Some(total) = total {
        let satisfiable = match shape.range {
            RequestedRange::Bounded { end, .. } => end.min(total),
            _ => total,
        };
        // The parser's end < total bound also makes the exclusive end fit.
        if end + 1 != satisfiable {
            return Err(ResponseFault::Range.into());
        }
    }
    Ok(GetHeadOutcome::Body {
        meta: meta(total),
        body: BodyWindow {
            object_offset: start,
            expected_len: Some(served),
            object_size: total,
        },
    })
}

enum ContentRange {
    Satisfied {
        start: u64,
        end: u64,
        total: Option<u64>,
    },
    Unsatisfied {
        total: Option<u64>,
    },
}

// Satisfied ranges establish S <= E < u64::MAX, and E < T when T is known.
// This keeps inclusive lengths and exclusive ends representable even for /*.
fn parse_content_range(value: &[u8]) -> Option<ContentRange> {
    let rest = trim_ascii(value).strip_prefix(b"bytes ")?;
    let slash = rest.iter().rposition(|byte| *byte == b'/')?;
    let (spec, total) = (trim_ascii(&rest[..slash]), trim_ascii(&rest[slash + 1..]));
    let total = match total {
        b"*" => None,
        digits => Some(decimal(digits)?),
    };
    if spec == b"*" {
        return Some(ContentRange::Unsatisfied { total });
    }
    let dash = spec.iter().position(|byte| *byte == b'-')?;
    let start = decimal(&spec[..dash])?;
    let end = decimal(&spec[dash + 1..])?;
    if start > end || end >= total.unwrap_or(u64::MAX) {
        return None;
    }
    Some(ContentRange::Satisfied { start, end, total })
}

// Reads a header value that carries text. Azure writes `Last-Modified` in
// ASCII, so a value that is not UTF-8 is a fault in the head.
fn text_header(value: Option<&[u8]>) -> Result<Option<&str>> {
    value
        .map(|value| core::str::from_utf8(value).map_err(|_| Error::Response(ResponseFault::Head)))
        .transpose()
}

fn decimal_header(value: Option<&[u8]>) -> Result<Option<u64>> {
    match value {
        None => Ok(None),
        Some(value) => decimal(trim_ascii(value))
            .map(Some)
            .ok_or(Error::Response(ResponseFault::Head)),
    }
}

pub(crate) fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u64, |value, byte| {
        let digit = byte.checked_sub(b'0').filter(|digit| *digit <= 9)?;
        value.checked_mul(10)?.checked_add(digit as u64)
    })
}

fn trim_ascii(value: &[u8]) -> &[u8] {
    value.trim_ascii()
}

fn kind_for_code(code: &[u8]) -> Option<ServiceErrorKind> {
    Some(match code {
        b"BlobNotFound" | b"ResourceNotFound" => ServiceErrorKind::NotFound,
        b"ContainerNotFound" => ServiceErrorKind::NoSuchContainer,
        b"BlobAlreadyExists" | b"ContainerAlreadyExists" => ServiceErrorKind::AlreadyExists,
        b"ConditionNotMet" | b"TargetConditionNotMet" => ServiceErrorKind::Precondition,
        b"InvalidRange" => ServiceErrorKind::RangeNotSatisfiable,
        b"ServerBusy" => ServiceErrorKind::Throttled,
        b"OperationTimedOut" => ServiceErrorKind::Timeout,
        b"AuthenticationFailed"
        | b"AuthorizationFailure"
        | b"InvalidAuthenticationInfo"
        | b"AuthorizationPermissionMismatch"
        | b"InsufficientAccountPermissions" => ServiceErrorKind::Unauthorized,
        b"InternalError" | b"ServiceUnavailable" => ServiceErrorKind::Service,
        b"InvalidBlockList"
        | b"InvalidBlockId"
        | b"InvalidBlobOrBlock"
        | b"BlockCountExceedsLimit"
        | b"BlockListTooLong" => ServiceErrorKind::InvalidUpload,
        _ => return None,
    })
}

// The error that a failed response body names, if it names one this crate
// recognizes.
fn body_kind(body: &[u8]) -> Option<ServiceErrorKind> {
    crate::xml::error_code(body).and_then(|code| kind_for_code(code.as_bytes()))
}

fn failure_class(status: u16, kind: Option<ServiceErrorKind>) -> FailureClass {
    match kind {
        Some(ServiceErrorKind::Unauthorized) => FailureClass::Auth,
        Some(ServiceErrorKind::Throttled) => FailureClass::Throttled,
        Some(ServiceErrorKind::Service | ServiceErrorKind::Timeout) => FailureClass::Server,
        _ => match status {
            300..=399 => FailureClass::Redirect,
            401 | 403 => FailureClass::Auth,
            408 | 429 => FailureClass::Throttled,
            500..=599 => FailureClass::Server,
            _ => FailureClass::Other,
        },
    }
}

// The condition is the last header of every request that carries one.
fn push_condition(head: &mut HeadWriter<'_>, condition: ConditionKind, value: Option<&[u8]>) {
    if let Some(name) = condition_header(condition) {
        let value = value.expect("the plan was validated");
        head.header(name, |out| out.push(value));
    }
}

fn write_range(out: &mut Writer<'_>, range: RequestedRange) {
    out.push(b"bytes=");
    match range {
        RequestedRange::Bounded { start, end } => {
            out.push(U64Decimal::new(start).as_bytes());
            out.push(b"-");
            // validate_get requires start < end, so end is nonzero.
            out.push(U64Decimal::new(end - 1).as_bytes());
        }
        RequestedRange::Offset(first) => {
            out.push(U64Decimal::new(first).as_bytes());
            out.push(b"-");
        }
        RequestedRange::Whole | RequestedRange::Suffix(_) => {
            unreachable!("the plan was validated")
        }
    }
}

// The written head, or the exact number of bytes that it needed.
fn capacity_error(capacity: crate::CapacityError) -> Error {
    // A slice's byte size must fit isize, including descriptor arrays on 32-bit.
    // HeaderSpan is nonzero-sized; divide before comparing to avoid overflow.
    let max_headers = isize::MAX as usize / core::mem::size_of::<HeaderSpan>();
    if capacity.required > isize::MAX as usize || capacity.required_headers > max_headers {
        InvalidPlan::RequestTooLarge.into()
    } else {
        Error::Capacity(capacity)
    }
}

#[cfg(test)]
#[test]
fn a_request_larger_than_a_slice_is_not_a_recoverable_capacity_error() {
    let capacity = crate::CapacityError {
        required: isize::MAX as usize + 1,
        ..crate::CapacityError::default()
    };
    assert_eq!(
        capacity_error(capacity),
        InvalidPlan::RequestTooLarge.into()
    );
    let max_headers = isize::MAX as usize / core::mem::size_of::<HeaderSpan>();
    let capacity = crate::CapacityError {
        required_headers: max_headers,
        ..crate::CapacityError::default()
    };
    assert_eq!(capacity_error(capacity), Error::Capacity(capacity));
    let capacity = crate::CapacityError {
        required_headers: max_headers + 1,
        ..capacity
    };
    assert_eq!(
        capacity_error(capacity),
        InvalidPlan::RequestTooLarge.into()
    );
}

fn encoded<'r>(
    head: HeadWriter<'r>,
    method: Method,
    payload: Payload<'r>,
) -> Result<WireRequest<'r>> {
    let capacity = head.capacity();
    head.finish(method, payload)
        .ok_or_else(|| capacity_error(capacity))
}

fn delete_snapshots(kind: DeleteKind) -> Option<&'static str> {
    match kind {
        // Azure refuses an object with snapshots when the header is absent,
        // which is the outcome a plan that names the object alone asks for.
        DeleteKind::Object => None,
        DeleteKind::ObjectAndSnapshots => Some("include"),
        DeleteKind::SnapshotsOnly => Some("only"),
    }
}

fn condition_header(kind: ConditionKind) -> Option<&'static str> {
    match kind {
        ConditionKind::None => None,
        ConditionKind::IfMatch => Some("if-match"),
        ConditionKind::IfNoneMatch => Some("if-none-match"),
    }
}

// The length of a name as Azure counts it.
fn name_units(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

// Reject unsupported names before encoding, without losing the reason.
fn validate_key(key: &str, namespace: AzureNamespace) -> Result<()> {
    if key.is_empty() {
        return Err(InvalidPlan::EmptyKey.into());
    }
    if namespace == AzureNamespace::Flat && name_units(key) > MAX_BLOB_NAME_UNITS {
        return Err(InvalidPlan::KeyTooLong.into());
    }
    // Azure refuses an ASCII control character in a name, with 400. Measured
    // for U+0001, U+000B, U+000C, U+000E and U+007F. Testing the bytes is
    // testing the characters: every byte of a character outside ASCII is 0x80
    // or above, and none of those is an ASCII control.
    if key.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(InvalidPlan::KeyControlCharacter.into());
    }
    // Azure takes a name of 255 `/`-delimited segments and refuses 256,
    // whatever the 254 in its documentation says. Measured by bisection; see
    // the live suite.
    // At most one segment per byte plus one; str lengths fit isize.
    if key.matches('/').count() + 1 > MAX_BLOB_NAME_SEGMENTS {
        return Err(InvalidPlan::KeyTooManySegments.into());
    }
    // Azure drops a dot from the end of every segment of a name: `dot.` is
    // stored as `dot`, and `dotseg./x` as `dotseg/x`. Measured; see the live
    // suite.
    //
    // The same test covers a segment that is only dots. A host resolves `.`
    // and `..` out of the URL before it sends it, as the standard for URLs
    // requires, so those would name another object entirely; measured, as
    // `dots/../up` wrote `up`.
    if key.split('/').any(|segment| segment.ends_with('.')) {
        return Err(InvalidPlan::KeyWouldBeNormalized.into());
    }
    Ok(())
}

fn validate_get(get: &PhysicalGet<'_>, namespace: AzureNamespace) -> Result<()> {
    validate_key(get.key, namespace)?;
    match get.range {
        RequestedRange::Bounded { start, end } if start >= end => {
            return Err(InvalidPlan::Range.into());
        }
        RequestedRange::Suffix(_) => return Err(InvalidPlan::UnsupportedRange.into()),
        RequestedRange::Whole => {}
        _ if get.kind == GetKind::Metadata => {
            return Err(InvalidPlan::RangedMetadata.into());
        }
        _ => {}
    }
    validate_condition(get.condition, get.condition_value)
}

// The kind and the value must agree in both directions: a kind without a value
// cannot be encoded, and a value without a kind would be dropped.
fn validate_condition(condition: ConditionKind, value: Option<&[u8]>) -> Result<()> {
    match (condition, value) {
        (ConditionKind::None, None) => Ok(()),
        (ConditionKind::IfMatch | ConditionKind::IfNoneMatch, Some(value))
            if valid_header(value) =>
        {
            Ok(())
        }
        _ => Err(InvalidPlan::Condition.into()),
    }
}

/// The most bytes that Azure writes in one `Put Blob` request.
///
/// [`Blobs::encode_put`] refuses a longer payload with
/// [`InvalidPlan::PayloadTooLarge`]. Write a longer object in blocks. This
/// is a `u64` because it does not fit a 32-bit `usize`.
pub const MAX_PUT_LEN: u64 = 5000 * 1024 * 1024;

fn validate_put(put: &PhysicalPut<'_>, len: u64, namespace: AzureNamespace) -> Result<()> {
    validate_key(put.key, namespace)?;
    if len > MAX_PUT_LEN {
        return Err(InvalidPlan::PayloadTooLarge.into());
    }
    validate_condition(put.condition, put.condition_value)
}

fn validate_list(list: &PhysicalList<'_>, namespace: AzureNamespace) -> Result<()> {
    // A prefix is the start of a key, so it is bounded like one. An empty
    // prefix lists the whole container and is valid.
    // A prefix is the start of a key, so it is bounded like one. The rest of
    // `validate_key` does not apply: a prefix is written into the query, where
    // nothing resolves a `..` and nothing drops a trailing dot, and `dir.` is
    // an honest prefix of `dir.txt`.
    if namespace == AzureNamespace::Flat && name_units(list.prefix) > MAX_BLOB_NAME_UNITS {
        return Err(InvalidPlan::Prefix.into());
    }
    if list.marker.is_some_and(str::is_empty) {
        return Err(InvalidPlan::Marker.into());
    }
    if list.max_results == Some(0) {
        return Err(InvalidPlan::MaxResults.into());
    }
    Ok(())
}

fn validate_delete(delete: &PhysicalDelete<'_>, namespace: AzureNamespace) -> Result<()> {
    validate_key(delete.key, namespace)?;
    validate_condition(delete.condition, delete.condition_value)
}

fn valid_header(value: &[u8]) -> bool {
    !value.is_empty() && value.is_ascii() && !value.iter().any(u8::is_ascii_control)
}

/// Azure block-operation response headers, borrowing transport-owned values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockResponseHead<'a> {
    /// Shared headers, including raw native error code and request ID.
    pub common: ResponseHead<'a>,
    /// The `content-md5` value.
    pub content_md5: Option<&'a [u8]>,
    /// The `x-ms-content-crc64` value.
    pub content_crc64: Option<&'a [u8]>,
    /// The `x-ms-request-server-encrypted` value.
    pub server_encrypted: Option<&'a [u8]>,
    /// The `x-ms-encryption-key-sha256` value.
    pub encryption_key_sha256: Option<&'a [u8]>,
    /// The `x-ms-encryption-scope` value.
    pub encryption_scope: Option<&'a [u8]>,
    /// The `x-ms-client-request-id` value.
    pub client_request_id: Option<&'a [u8]>,
    /// The `date` value.
    pub date: Option<&'a [u8]>,
    /// The `x-ms-blob-content-length` value.
    pub blob_content_length: Option<&'a [u8]>,
}

impl<'a> BlockResponseHead<'a> {
    /// Reads shared and native fields in one pass.
    pub fn from_headers(
        status: u16,
        headers: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> Self {
        let mut result = Self {
            common: ResponseHead::new(status),
            ..Self::default()
        };
        for (name, value) in headers {
            result.insert(name, value);
        }
        result
    }

    /// Consumes an incremental parser's header without copying its value.
    pub fn insert(&mut self, name: &str, value: &'a [u8]) {
        let slot = if name.eq_ignore_ascii_case("content-md5") {
            &mut self.content_md5
        } else if name.eq_ignore_ascii_case("x-ms-content-crc64") {
            &mut self.content_crc64
        } else if name.eq_ignore_ascii_case("x-ms-request-server-encrypted") {
            &mut self.server_encrypted
        } else if name.eq_ignore_ascii_case("x-ms-encryption-key-sha256") {
            &mut self.encryption_key_sha256
        } else if name.eq_ignore_ascii_case("x-ms-encryption-scope") {
            &mut self.encryption_scope
        } else if name.eq_ignore_ascii_case("x-ms-client-request-id") {
            &mut self.client_request_id
        } else if name.eq_ignore_ascii_case("date") {
            &mut self.date
        } else if name.eq_ignore_ascii_case("x-ms-blob-content-length") {
            &mut self.blob_content_length
        } else {
            self.common.insert(name, value);
            return;
        };
        if slot.is_none() {
            *slot = Some(value);
        }
    }
}
