// Azure block blobs: Put Block, Put Block List and Get Block List.
//
// A block blob is written in blocks: stage each block under an ID that you
// choose, then commit an ordered list of them. The three operations answer
// with the shared outcomes.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::azure::template::{
    Describe, Detail, Field, FieldList, Holes, Operation, Template, encode as encode_template,
    prepare as prepare_template,
};
use crate::azure::{
    AzureNamespace, body_kind, failed, named, names_existing_object, names_failed_condition,
    validate_metadata,
};
use crate::azure::{
    Container, ObjectText, azure_tag_char, encode_object, prepare_object, stored_fields,
    utf8_properties,
};
use crate::checksum::{BASE64_LEN, ChecksumKind};
use crate::common::{
    checksum_text, condition_header, validate_checksum_item, validate_properties, validate_tags,
};
use crate::common::{decimal_header, finish_with_body, meta_of, text_header, validate_condition};
use crate::request::ByteSink;
use crate::request::Writer;
use crate::template::{Body, Fill, Hole, ParamValue, Values, WireRequest};
use crate::url::QueryValue;
use crate::{ChecksumValue, ConditionValue, MetadataPair, ObjectItem, RevisionKind, StageOptions};
use crate::{
    CommitHeadOutcome, CommitShape, ConditionKind, Failure, InvalidPlan, ListPartsHeadOutcome,
    Listing, Method, ObjectMeta, Payload, ResponseFault, ResponseHead, Result, ServiceErrorKind,
    StageHeadOutcome, Timestamps, WriteOptions,
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
    /// [`block_id`](crate::azure::block_id), or pass a listed
    /// [`Block::id`] unchanged. The encoder checks the text: it is not empty,
    /// and it is at most 88 characters of standard base64 with at most two `=`
    /// of padding. That is at most 64 decoded bytes. The encoder percent-encodes
    /// it into the query itself. Only the service checks that every block of one
    /// blob decodes to the same length, and that a referenced block exists.
    pub id: &'a str,
    /// The stored version to select.
    pub source: BlockSource,
}

impl<'a> From<&BlockRef<'a>> for BlockRef<'a> {
    fn from(block: &BlockRef<'a>) -> Self {
        *block
    }
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

/// The most bytes that Azure stages in one `Put Block` request.
///
/// [`encode_stage_block`] refuses a longer payload with
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
    /// each blob.
    pub options: WriteOptions<'o>,
    /// Whether each commit declares the MD5 of the blob it commits, which
    /// Azure stores and returns on a read but does not check.
    pub content_md5: bool,
}

/// One Put Block List, which commits a blob from its blocks.
///
/// The blocks are a slice of [`BlockRef`], or any iterator of them that can
/// be cloned. Such an iterator can name each block from its number as it
/// goes. A commit reads the blocks more than once, and keeps none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitItem<'i, B = &'i [BlockRef<'i>]> {
    /// The key of the blob, under the rules of
    /// [Object keys](crate#object-keys).
    pub key: &'i str,
    /// What the template's condition compares against. Present exactly when
    /// the template carries one.
    pub condition_value: Option<ConditionValue<'i>>,
    /// The ordered blocks of the blob, at most 50,000.
    pub blocks: B,
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
}

impl<B> CommitItem<'_, B> {
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
struct CommitFill<'i, B> {
    values: Values<'i>,
    blocks: B,
}

// Each block of `blocks` as its ID and its source.
fn block_pairs<'b, B>(blocks: &B) -> impl Iterator<Item = (&'b str, BlockSource)> + use<'b, B>
where
    B: IntoIterator<Item: Into<BlockRef<'b>>> + Clone,
{
    blocks.clone().into_iter().map(|block| {
        let block = block.into();
        (block.id, block.source)
    })
}

impl<'b, B> Fill for CommitFill<'_, B>
where
    B: IntoIterator<Item: Into<BlockRef<'b>>> + Clone,
{
    const BODY: bool = true;

    fn values(&self) -> &Values<'_> {
        &self.values
    }

    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        if hole != Hole::Body {
            return false;
        }
        write_block_list(out, block_pairs(&self.blocks));
        true
    }

    fn body_len(&self) -> Option<usize> {
        Some(block_list_len(block_pairs(&self.blocks)))
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
pub fn encode_commit_blocks<'r, 'b, B>(
    template: &Template,
    bytes: &[u8],
    item: &CommitItem<'_, B>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>>
where
    B: IntoIterator<Item: Into<BlockRef<'b>>> + Clone,
{
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
    if item.blocks.clone().into_iter().count() > MAX_BLOCKS {
        return Err(InvalidPlan::Parts.into());
    }
    for (id, _) in block_pairs(&item.blocks) {
        validate_block_id(id)?;
    }
    let fill = CommitFill {
        values: Values {
            now: Some(now),
            key,
            condition: item.condition_value,
            checksum: item.content_md5,
            ..Values::default()
        },
        blocks: item.blocks.clone(),
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
        409 if names_existing_object(409, shape.condition, named(&head)) => {
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
    if names_failed_condition(failure.status, shape.condition != ConditionKind::None, kind)
        || names_existing_object(failure.status, shape.condition, kind)
    {
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

/// The most blocks that a Get Block List body of `len` bytes can hold.
///
/// Use it to size the array for [`fill_blocks`] from the
/// `expected_len` of
/// [`ListPartsHeadOutcome::Parts`](crate::ListPartsHeadOutcome::Parts).
/// The smallest element that the reader accepts is
/// `<Block><Name>x</Name><Size>0</Size></Block>`, 43 bytes. The reader
/// requires a name and a size, and does not check that the name is base64.
pub const fn max_blocks_in(len: usize) -> usize {
    len / 43
}

/// Writes the block ID for `bytes` in the base64 form that Azure stores.
///
/// A block ID is base64 text on the wire, and decodes to at most 64 bytes.
/// Every block of one blob must decode to the same length. Choose the bytes
/// however you number blocks, and pass what this returns as [`BlockRef::id`]
/// or [`StageBlockItem::id`].
///
/// Copies the encoding into `into` and returns it. The encoding is four
/// characters for every three bytes, rounded up, so 88 bytes of `into` fit
/// every ID. Returns [`None`] if `bytes` is empty or longer than 64 bytes,
/// or if `into` is shorter than the encoding.
pub fn block_id<'a>(bytes: &[u8], into: &'a mut [u8]) -> Option<&'a str> {
    if bytes.is_empty() || bytes.len() > 64 {
        return None;
    }
    let into = into.get_mut(..bytes.len().div_ceil(3) * 4)?;
    Some(crate::encoding::base64_into(bytes, into))
}

#[cfg(test)]
mod tests {
    use super::block_id;

    #[test]
    fn writes_the_base64_of_chosen_bytes() {
        let mut into = [0; 12];
        assert_eq!(block_id(&[0, 0, 0, 0], &mut into), Some("AAAAAA=="));
        assert_eq!(block_id(&[0, 0, 0, 1], &mut into), Some("AAAAAQ=="));
        assert_eq!(block_id(b"ab", &mut into), Some("YWI="));
        assert_eq!(block_id(b"abc", &mut into), Some("YWJj"));
        assert_eq!(block_id(&[0xfb, 0xff], &mut into), Some("+/8="));
        assert_eq!(block_id(b"", &mut into), None);
        assert_eq!(block_id(&[0; 65], &mut [0; 88]), None);
        assert_eq!(block_id(b"abc", &mut [0; 3]), None);
    }
}
