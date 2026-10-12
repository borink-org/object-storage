// Azure block blobs: Put Block, Put Block List and Get Block List.
//
// A block blob is written in blocks: stage each block under an ID that you
// choose, then commit an ordered list of them. The commit takes the shared
// plan `PhysicalCommit`, and the three operations answer with the shared
// outcomes.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::azure::template::{
    Describe, Detail, Field, FieldList, Holes, Operation, Template, encode as encode_template,
    prepare as prepare_template,
};
use crate::azure::{
    AzureNamespace, Blobs, Write, body_kind, failed, named, names_failed_condition, push_metadata,
    push_stored, validate_key, validate_metadata, validate_options,
};
use crate::azure::{
    Container, ObjectText, azure_tag_char, encode_object, prepare_object, stored_fields,
    utf8_properties,
};
use crate::checksum::{BASE64_LEN, ChecksumKind};
use crate::common::{
    checksum_text, condition_header, validate_checksum_item, validate_properties, validate_tags,
    validate_template_options,
};
use crate::common::{
    decimal_header, encoded, encoded_with_body, finish_with_body, meta_of, push_checksum,
    push_condition, text_header, validate_condition,
};
use crate::request::Writer;
use crate::request::{ByteSink, HeadWriter, U64Decimal};
use crate::template::{Body, Fill, Hole, ParamValue, Values, WireRequest};
use crate::url;
use crate::url::QueryValue;
use crate::{ChecksumValue, ConditionValue, MetadataPair, ObjectItem, RevisionKind, StageOptions};
use crate::{
    CommitHeadOutcome, CommitShape, ConditionKind, Failure, HeaderSpan, InvalidPlan, LegacyRequest,
    ListPartsHeadOutcome, Listing, Method, ObjectMeta, Payload, PhysicalCommit, RequestedRange,
    ResponseFault, ResponseHead, Result, ServiceErrorKind, StageHeadOutcome, Timestamps,
    WriteOptions,
};

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

    // The element that names the source in a block list, opened and closed.
    const fn tags(self) -> (&'static str, &'static str) {
        match self {
            Self::Uncommitted => ("<Uncommitted>", "</Uncommitted>"),
            Self::Committed => ("<Committed>", "</Committed>"),
            Self::Latest => ("<Latest>", "</Latest>"),
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
    /// The options of the stage, such as a checksum of the block. A stage
    /// refuses [`WriteOptions::declared_md5`].
    pub options: WriteOptions<'a>,
}

impl<'a> PhysicalStageBlock<'a> {
    /// Creates a plan that stages `id` for `key` with no options.
    pub const fn new(key: &'a str, id: &'a str) -> Self {
        Self {
            key,
            id,
            options: WriteOptions::new(),
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
    /// [`Error::Capacity`] as [`encode_get`](crate::azure::encode_get) does.
    pub fn encode_list_blocks<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalListBlocks<'_>,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
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
                url::literal("comp", "blocklist"),
                url::literal("blocklisttype", kind),
                url::encoded("snapshot", plan.snapshot),
                url::encoded("versionid", plan.version),
            ],
            RequestedRange::Whole,
            now,
        )?;
        encoded(head, Method::Get, Payload::Slice(&[]))
    }

    /// Writes the request head for an Azure Put Block into `buf`.
    ///
    /// The head states the length of `content`, which stays where you put it,
    /// as for [`encode_put`](crate::azure::encode_put).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an Azure request,
    /// or if `content` is longer than one block may be. Returns
    /// [`Error::Capacity`] as [`encode_put`](crate::azure::encode_put) does, or call
    /// [`layered::stage_block_requirements`](crate::layered::stage_block_requirements)
    /// first.
    pub fn encode_stage_block<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalStageBlock<'_>,
        content: Payload<'r>,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
        validate_block_key(plan.key, self.namespace)?;
        validate_block_id(plan.id)?;
        if content.len() > MAX_STAGE_LEN {
            return Err(InvalidPlan::PayloadTooLarge.into());
        }
        validate_options(&plan.options, Write::Stage, content.bytes().is_some(), self)?;
        let mut head = HeadWriter::new(buf, headers);
        let query = [
            url::literal("comp", "block"),
            url::encoded("blockid", plan.id),
        ];
        self.build(
            &mut head,
            Some(plan.key),
            &query,
            RequestedRange::Whole,
            now,
        )?;
        head.header("content-length", U64Decimal::new(content.len()).as_bytes());
        push_checksum(&mut head, plan.options.checksum, &self.checksums, |sum| {
            sum.update(content.bytes().unwrap_or(&[]));
        });
        encoded(head, Method::Put, content)
    }

    /// Writes an Azure Put Block List into `buf`: the head, then the XML
    /// body after it.
    ///
    /// `blocks` become the object in this order, each looked up where its
    /// [`BlockSource`] says. The body is written into `buf` after the head,
    /// so [`LegacyRequest::body_span`] names it and [`LegacyRequest::payload`]
    /// borrows it; send both before reusing `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an Azure request,
    /// such as [`InvalidPlan::Option`] for a plan that sets
    /// [`PhysicalCommit::size`], if `blocks` holds more than 50,000 entries or
    /// an ID that fails the checks on [`BlockRef::id`]. Returns
    /// [`Error::Capacity`] with the bytes that the head and the body need
    /// together, or call
    /// [`layered::commit_blocks_requirements`](crate::layered::commit_blocks_requirements)
    /// first.
    pub fn encode_commit_blocks<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCommit<'_>,
        blocks: &[BlockRef<'_>],
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
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
        plan: &PhysicalCommit<'_>,
        blocks: impl Iterator<Item = (I, BlockSource)> + Clone,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>>
    where
        I: AsRef<str>,
    {
        validate_block_key(plan.key, self.namespace)?;
        validate_condition(plan.condition, plan.condition_value)?;
        validate_metadata(plan.metadata)?;
        validate_options(&plan.options, Write::Commit, true, self)?;
        // Azure does not check the length of the blocks it commits.
        if plan.size.is_some() {
            return Err(InvalidPlan::Option.into());
        }
        for (index, (id, _)) in blocks.clone().enumerate() {
            if index >= MAX_BLOCKS {
                return Err(InvalidPlan::Parts.into());
            }
            validate_block_id(id.as_ref())?;
        }
        let length = block_list_len(blocks.clone());
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &[url::literal("comp", "blocklist")],
            RequestedRange::Whole,
            now,
        )?;
        head.header("content-length", U64Decimal::new(length as u64).as_bytes());
        // The content of a commit is the block list, so a checksum of the
        // content is a checksum of that text. The object's own MD5 is a
        // property of the blob, `x-ms-blob-content-md5`.
        push_checksum(&mut head, plan.options.checksum, &self.checksums, |sum| {
            write_block_list(sum, blocks.clone());
        });
        if let Some(md5) = plan.options.declared_md5 {
            head.header("x-ms-blob-content-md5", md5.as_bytes());
        }
        push_stored(&mut head, &plan.options);
        push_metadata(&mut head, plan.metadata);
        push_condition(&mut head, plan.condition, plan.condition_value);
        encoded_with_body(head, Method::Put, |out| write_block_list(out, blocks))
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
        fill_blocks(body, into)
    }

    /// Reads the head that answers a stage.
    ///
    /// Every head that Azure sends becomes a [`StageHeadOutcome`],
    /// including the heads that report a failure. If the head names no error
    /// code, the outcome is [`StageHeadOutcome::NeedErrorBody`]: read the
    /// body and pass it to [`Self::accept_stage_block_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status that a stage never returns is [`ResponseFault::Status`].
    pub fn accept_stage_block_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<StageHeadOutcome<'h>> {
        accept_stage_block_head(head)
    }

    /// Finishes a [`StageHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// Pass the [`Failure`] of that outcome and the body
    /// that you read.
    pub fn accept_stage_block_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> StageHeadOutcome<'h> {
        accept_stage_block_error_body(failure, body)
    }

    /// Reads the head that answers a commit.
    ///
    /// Pass the `shape` that [`PhysicalCommit::shape`] gave you before
    /// the request. A failed condition is reported as
    /// [`CommitHeadOutcome::PreconditionFailed`] only if that plan
    /// carried a condition. Otherwise a 412 is a service failure that names
    /// its code.
    ///
    /// Every head that Azure sends becomes a [`CommitHeadOutcome`],
    /// including the heads that report a failure. If the head names no error
    /// code, the outcome is [`CommitHeadOutcome::NeedErrorBody`]: read the
    /// body and pass it to [`Self::accept_commit_blocks_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status that a commit never returns is [`ResponseFault::Status`].
    pub fn accept_commit_blocks_head<'h>(
        &self,
        shape: CommitShape,
        head: ResponseHead<'h>,
    ) -> Result<CommitHeadOutcome<'h>> {
        accept_commit_blocks_head(shape, head)
    }

    /// Finishes a [`CommitHeadOutcome::NeedErrorBody`] with the
    /// response body.
    ///
    /// Pass the `shape` of the commit, the [`Failure`] of
    /// that outcome and the body that you read.
    pub fn accept_commit_blocks_error_body<'h>(
        &self,
        shape: CommitShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> CommitHeadOutcome<'h> {
        accept_commit_blocks_error_body(shape, failure, body)
    }

    /// Reads the head that answers a block listing.
    ///
    /// Every head that Azure sends becomes a [`ListPartsHeadOutcome`],
    /// including the heads that report a failure. If the head names no error
    /// code, the outcome is [`ListPartsHeadOutcome::NeedErrorBody`]: read the
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
    ) -> Result<ListPartsHeadOutcome<'h>> {
        accept_list_blocks_head(head)
    }

    /// Finishes a [`ListPartsHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// Pass the [`Failure`] of that outcome and the body
    /// that you read.
    pub fn accept_list_blocks_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> ListPartsHeadOutcome<'h> {
        accept_list_blocks_error_body(failure, body)
    }
}

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
        last_modified: text_header(head.last_modified)?,
        ..meta_of(head)
    })
}

fn validate_block_key(key: &str, namespace: AzureNamespace) -> Result<()> {
    validate_key(key, namespace)
}

// The local half of the rules on `BlockRef::id`. Equal decoded lengths
// within one blob, and whether a block exists, are the service's to check.
pub(super) fn validate_block_id(id: &str) -> Result<()> {
    let id = id.as_bytes();
    let data = match id {
        [data @ .., b'=', b'='] | [data @ .., b'='] => data,
        data => data,
    };
    // The padding is the one or two `=` that end the ID.
    let padding = id.len() - data.len();
    // Every byte is tested, without stopping at the first bad one, so that
    // the test runs on many bytes at once.
    let base64 = data.iter().fold(true, |valid, &b| {
        valid & (b.is_ascii_alphanumeric() | (b == b'+') | (b == b'/'))
    });
    if id.is_empty()
        || id.len() > 88
        || !id.len().is_multiple_of(4)
        // The preceding checks establish 4 <= len <= 88.
        || id.len() / 4 * 3 - padding > 64
        || !base64
    {
        return Err(InvalidPlan::PartId.into());
    }
    Ok(())
}

// Writes the block list of a commit into `out`, one piece at a time. The
// block list is the content of a commit, so a commit that computes a
// checksum writes it twice: first into the checksum, then into the buffer
// as the body. Both writes go through this function, so they write the same
// bytes.
// The length of the block list that `write_block_list` writes. At most
// 50,000 IDs of 88 bytes and tags of 11 bytes: under 6 MiB with the
// wrapper, so it fits a 32-bit usize.
fn block_list_len<I: AsRef<str>>(blocks: impl Iterator<Item = (I, BlockSource)>) -> usize {
    COMMIT_OPEN.len()
        + COMMIT_CLOSE.len()
        + blocks
            .map(|(id, source)| {
                let (open, close) = source.tags();
                open.len() + id.as_ref().len() + close.len()
            })
            .sum::<usize>()
}

fn write_block_list<I: AsRef<str>>(
    out: &mut dyn ByteSink,
    blocks: impl Iterator<Item = (I, BlockSource)>,
) {
    out.push(COMMIT_OPEN);
    for (id, source) in blocks {
        let (open, close) = source.tags();
        out.push(open.as_bytes());
        out.push(id.as_ref().as_bytes());
        out.push(close.as_bytes());
    }
    out.push(COMMIT_CLOSE);
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

/// One Put Block, which stages one block of a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageBlockItem<'i> {
    /// The key of the blob, under the rules of
    /// [Object keys](crate#object-keys).
    pub key: &'i str,
    /// The block ID, under the rules of [`BlockRef::id`].
    pub id: &'i str,
    /// The block's bytes, or the length of those you send yourself. At most
    /// [`MAX_STAGE_LEN`].
    pub content: Payload<'i>,
    /// The checksum of the block, of the template's kind. Present exactly
    /// when the template names a kind.
    pub checksum: Option<ChecksumValue<'i>>,
}

impl<'i> StageBlockItem<'i> {
    /// Creates a Put Block of `content` as the block `id` of `key`.
    pub const fn new(key: &'i str, id: &'i str, content: Payload<'i>) -> Self {
        Self {
            key,
            id,
            content,
            checksum: None,
        }
    }
}

/// What every Put Block List of a template does alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitOptions<'o> {
    /// The condition that each commit carries, on the blob that the key
    /// holds before. [`ConditionKind::IfNoneMatch`] with `*` commits only
    /// if the key holds none.
    pub condition: ConditionKind,
    /// The metadata pairs to store with each blob, under the rules of
    /// [`PutOptions::metadata`](crate::PutOptions::metadata).
    pub metadata: &'o [MetadataPair<'o>],
    /// The content properties, the tags and the access tier to store with
    /// each blob. [`WriteOptions::checksum`] and
    /// [`WriteOptions::declared_md5`] are refused: a commit declares the
    /// blob's MD5 in [`CommitItem::content_md5`].
    pub options: WriteOptions<'o>,
    /// Whether each commit declares the MD5 of the blob it commits, which
    /// Azure stores and returns on a read but does not check.
    pub content_md5: bool,
}

/// One Put Block List, which commits a blob from its blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitItem<'i> {
    /// The key of the blob, under the rules of
    /// [Object keys](crate#object-keys).
    pub key: &'i str,
    /// What the template's condition compares against. Present exactly when
    /// the template carries one.
    pub condition_value: Option<ConditionValue<'i>>,
    /// The ordered blocks of the blob, at most 50,000.
    pub blocks: &'i [BlockRef<'i>],
    /// The base64 MD5 of the whole blob. Present exactly when the template's
    /// [`CommitOptions::content_md5`] is set.
    pub content_md5: Option<&'i str>,
}

impl<'i> CommitItem<'i> {
    /// Creates a commit of `blocks` as the blob `key`.
    pub const fn new(key: &'i str, blocks: &'i [BlockRef<'i>]) -> Self {
        Self {
            key,
            condition_value: None,
            blocks,
            content_md5: None,
        }
    }

    /// Returns the part of this commit that holds no borrows, for a
    /// template with `options`.
    pub const fn shape(&self, options: &CommitOptions<'_>) -> CommitShape {
        CommitShape {
            condition: options.condition,
        }
    }
}

/// Writes the template of Put Blocks with `options` into `buf`.
///
/// # Errors
///
/// Returns [`InvalidPlan::Option`] for a checksum other than an MD5 or a
/// CRC64.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_stage_block(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &StageOptions,
    buf: &mut [u8],
) -> Result<Template> {
    if options.checksum.is_some_and(|kind| !kind.azure_takes()) {
        return Err(InvalidPlan::Option.into());
    }
    prepare_template(
        container,
        token,
        namespace,
        &describe_stage(options.checksum),
        Operation::StageBlock,
        Detail::Stage(options.checksum),
        buf,
        |_| {},
    )
}

// The request of a Put Block of a template with `checksum`.
fn describe_stage(checksum: Option<ChecksumKind>) -> Describe<'static, 2> {
    Describe {
        method: Method::Put,
        key: true,
        query: [
            Some(("comp", ParamValue::Fixed(QueryValue::Literal("block")))),
            Some(("blockid", ParamValue::Hole(Hole::BlockId))),
        ],
        holes: Holes {
            content_length: Some(Hole::ContentLength),
            checksum: checksum.map(|kind| kind.header()),
            ..Holes::default()
        },
        fields: &[],
        metadata: &[],
        body: Body::None,
    }
}

/// Writes the Put Block `item` at `now` from `template` into `buf`, and
/// returns it.
///
/// The head states the length of the content, which stays where you put it:
/// the request borrows it, or leaves it to you to send.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), for a block ID that breaks the rules
/// of [`BlockRef::id`], for content longer than [`MAX_STAGE_LEN`], for a
/// checksum that is not the template's, and for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_stage_block<'r>(
    template: &Template,
    bytes: &[u8],
    item: &StageBlockItem<'r>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    let Detail::Stage(checksum) = template.detail else {
        return Err(InvalidPlan::Template.into());
    };
    let key = template.request_key(bytes, item.key)?;
    validate_block_id(item.id)?;
    if item.content.len() > MAX_STAGE_LEN {
        return Err(InvalidPlan::PayloadTooLarge.into());
    }
    validate_checksum_item(checksum, item.checksum)?;
    let mut text = [0; BASE64_LEN];
    let values = Values {
        now: Some(now),
        key,
        block_id: Some(item.id),
        content_length: Some(item.content.len()),
        checksum: checksum_text(item.checksum, &mut text, false),
        ..Values::default()
    };
    encode_template(
        template,
        bytes,
        &values,
        item.content,
        buf,
        &describe_stage(checksum),
    )
}

/// Reads the response head of a Put Block and reports what Azure did.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 201 is [`ResponseFault::Status`].
pub fn accept_stage_block_head<'h>(head: ResponseHead<'h>) -> Result<StageHeadOutcome<'h>> {
    match head.status {
        201 => Ok(StageHeadOutcome::Staged { e_tag: head.e_tag }),
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`StageHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_stage_block_error_body<'h>(
    failure: Failure<'h>,
    body: &[u8],
) -> StageHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Writes the template of Put Block Lists with `options` into `buf`.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a metadata pair, a content property,
/// a tag or a storage class that [`prepare_put`](crate::azure::prepare_put)
/// refuses, and [`InvalidPlan::Option`] for a checksum or a declared MD5 in
/// [`CommitOptions::options`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_commit_blocks(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &CommitOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    validate_template_options(&options.options)?;
    validate_metadata(options.metadata)?;
    validate_properties(&options.options, utf8_properties(namespace))?;
    validate_tags(options.options.tags, azure_tag_char, Some((10, 128, 256)))?;
    let mut fields = FieldList::new();
    stored_fields(&mut fields, &options.options);
    prepare_template(
        container,
        token,
        namespace,
        &describe_commit(
            options.condition,
            options.content_md5,
            fields.as_slice(),
            options.metadata,
        ),
        Operation::CommitBlocks,
        Detail::Commit {
            condition: options.condition,
            content_md5: options.content_md5,
        },
        buf,
        |_| {},
    )
}

// The request of a Put Block List of a template with `condition`, and an
// MD5 of the object when `content_md5` says so, which sends `fields` and
// `metadata`. Its block list is its body.
fn describe_commit<'p>(
    condition: ConditionKind,
    content_md5: bool,
    fields: &'p [Field<'p>],
    metadata: &'p [MetadataPair<'p>],
) -> Describe<'p, 1> {
    Describe {
        method: Method::Put,
        key: true,
        query: [Some((
            "comp",
            ParamValue::Fixed(QueryValue::Literal("blocklist")),
        ))],
        holes: Holes {
            content_length: Some(Hole::BodyLength),
            // The object's own MD5 is a property of the blob, which Azure
            // stores as it is declared.
            checksum: content_md5.then_some("x-ms-blob-content-md5"),
            condition: condition_header(condition),
            ..Holes::default()
        },
        fields,
        metadata,
        body: Body::Each,
    }
}

// Writes the block list of a commit as the body of its request.
struct CommitFill<'i> {
    values: Values<'i>,
    blocks: &'i [BlockRef<'i>],
}

impl Fill for CommitFill<'_> {
    const BODY: bool = true;

    fn values(&self) -> &Values<'_> {
        &self.values
    }

    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        if hole != Hole::Body {
            return false;
        }
        write_block_list(
            out,
            self.blocks.iter().map(|block| (block.id, block.source)),
        );
        true
    }

    fn body_len(&self) -> Option<usize> {
        Some(block_list_len(
            self.blocks.iter().map(|block| (block.id, block.source)),
        ))
    }
}

/// Writes the Put Block List `item` at `now` from `template` into `buf`,
/// with its block list as the body after the head, and returns it.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), for a condition value or an MD5 that
/// the template does not take, [`InvalidPlan::Parts`] for more than 50,000
/// blocks, a block ID that breaks the rules of [`BlockRef::id`], and a
/// template of another operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_commit_blocks<'r>(
    template: &Template,
    bytes: &[u8],
    item: &CommitItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    let Detail::Commit {
        condition,
        content_md5,
    } = template.detail
    else {
        return Err(InvalidPlan::Template.into());
    };
    let key = template.request_key(bytes, item.key)?;
    validate_condition(condition, item.condition_value)?;
    match (content_md5, item.content_md5) {
        (false, None) => {}
        (true, Some(text)) => ChecksumKind::Md5.check_base64(text)?,
        _ => return Err(InvalidPlan::Checksum.into()),
    }
    if item.blocks.len() > MAX_BLOCKS {
        return Err(InvalidPlan::Parts.into());
    }
    for block in item.blocks {
        validate_block_id(block.id)?;
    }
    let fill = CommitFill {
        values: Values {
            now: Some(now),
            key,
            condition: item.condition_value,
            checksum: item.content_md5,
            ..Values::default()
        },
        blocks: item.blocks,
    };
    encode_template(
        template,
        bytes,
        &fill,
        Payload::Slice(&[]),
        buf,
        &describe_commit(condition, content_md5, &[], &[]),
    )
}

/// Reads the response head of a Put Block List and reports what Azure did.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 201 is [`ResponseFault::Status`].
///
/// A 412 is [`CommitHeadOutcome::PreconditionFailed`] only if the commit
/// carried a condition and Azure names a failed condition.
pub fn accept_commit_blocks_head<'h>(
    shape: CommitShape,
    head: ResponseHead<'h>,
) -> Result<CommitHeadOutcome<'h>> {
    match head.status {
        201 => Ok(CommitHeadOutcome::Committed {
            meta: multipart_meta(head)?,
        }),
        412 if shape.condition != ConditionKind::None
            && named(&head) == Some(ServiceErrorKind::Precondition) =>
        {
            Ok(CommitHeadOutcome::PreconditionFailed)
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`CommitHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_commit_blocks_error_body<'h>(
    shape: CommitShape,
    failure: Failure<'h>,
    body: &[u8],
) -> CommitHeadOutcome<'h> {
    let kind = body_kind(body);
    if names_failed_condition(failure.status, shape.condition != ConditionKind::None, kind) {
        return CommitHeadOutcome::PreconditionFailed;
    }
    finish_with_body(failure, kind)
}

/// Writes the template of Get Block Lists of `kind` into `buf`. Each request
/// names a snapshot or a version of the kind `revision`, or the blob as it
/// is now.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_list_blocks(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    kind: BlockListKind,
    revision: RevisionKind,
    buf: &mut [u8],
) -> Result<Template> {
    let kind = match kind {
        BlockListKind::Committed => "committed",
        BlockListKind::Staged => "uncommitted",
        BlockListKind::All => "all",
    };
    prepare_object(
        container,
        token,
        namespace,
        Operation::ListBlocks,
        ConditionKind::None,
        revision,
        &ObjectText {
            param: Some((
                "blocklisttype",
                ParamValue::Fixed(QueryValue::Literal(kind)),
            )),
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the Get Block List of `item` at `now` from `template` into `buf`,
/// and returns it. Read the whole response body and pass it to
/// [`fill_blocks`].
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), for a revision that is not the
/// template's kind, for a condition value, and for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_list_blocks<'r>(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    encode_object(template, bytes, Operation::ListBlocks, item, now, buf)
}

/// Reads the response head of a Get Block List and reports what to do
/// next.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_list_blocks_head<'h>(head: ResponseHead<'h>) -> Result<ListPartsHeadOutcome<'h>> {
    match head.status {
        200 => Ok(ListPartsHeadOutcome::Parts {
            meta: multipart_meta(head)?,
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`ListPartsHeadOutcome::NeedErrorBody`] with the response
/// body.
pub fn accept_list_blocks_error_body<'h>(
    failure: Failure<'h>,
    body: &[u8],
) -> ListPartsHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Reads the blocks out of the response body of a Get Block List into
/// `into`, and returns how many it read.
///
/// The body is decoded in place, and the blocks borrow it.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if the body holds more blocks than the
/// array, with `required` set to the number it holds.
///
/// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is not
/// a block list.
pub fn fill_blocks<'b, E: From<Block<'b>>>(
    body: &'b mut [u8],
    into: &mut [E],
) -> Result<Listing<'b>> {
    crate::xml::azure::blocks::fill_blocks(body, into)
}
