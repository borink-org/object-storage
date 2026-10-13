//! Every type that crosses the boundary.
//!
//! `cbindgen` generates `include/borink/object_storage.h` from this module, so
//! a C program's declarations cannot differ from these. `layout` checks that
//! both compilers lay them out the same way.

#![forbid(unsafe_code)]

/// Bytes that your program owns and lends to a call.
///
/// A `len` of 0 is an empty value, and `ptr` may then be null.
///
/// # Lifetime
///
/// The bytes must stay valid for the whole call. A value returned by a reading
/// call points into the storage that the call read, and is valid until you
/// release or reuse that storage.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Bytes {
    /// The first byte.
    pub ptr: *const u8,
    /// The number of bytes.
    pub len: usize,
}

// The default of every struct that a call returns is each field absent or 0.
impl Default for Bytes {
    fn default() -> Self {
        Self {
            ptr: core::ptr::null(),
            len: 0,
        }
    }
}

/// Storage that a call writes into.
///
/// A `len` of 0 is an empty buffer, and `ptr` may then be null.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BytesMut {
    /// The first byte.
    pub ptr: *mut u8,
    /// The number of bytes.
    pub len: usize,
}

/// A range of bytes, as an offset from the start of your request buffer.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Span {
    /// The offset of the first byte.
    pub start: usize,
    /// The number of bytes.
    pub len: usize,
}

/// A range of your request buffer that a request may not have.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MaybeSpan {
    /// Whether the request has it.
    pub present: bool,
    /// Where it is.
    pub span: Span,
}

/// Bytes that a response head may not carry.
///
/// `present` and an empty `bytes` are different facts. A header that the
/// service sent empty is present, and one it did not send is not.
///
/// # Lifetime
///
/// `bytes` points into the storage that the `borink_header_ref`s of the call
/// pointed into, or into the error body that you passed. It is valid until you
/// release or reuse that storage.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MaybeBytes {
    /// Whether the head carried this value.
    pub present: bool,
    /// The bytes of it.
    pub bytes: Bytes,
}

/// A number that a response head may not carry.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MaybeU64 {
    /// Whether the head carried this number.
    pub present: bool,
    /// The number.
    pub value: u64,
}

/// A number that a plan may not carry.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MaybeU32 {
    /// Whether the plan carries this number.
    pub present: bool,
    /// The number.
    pub value: u32,
}

/// A failure, as the two numbers that describe every error of the protocol crate.
///
/// `code` is a `borink_error_code`, and `detail` is the discriminant of the
/// value inside it. A `code` of 0 means that nothing failed. Both numbers are
/// append-only: a value defined today keeps its meaning.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Status {
    /// The kind of failure, or 0 if there is none.
    pub code: u16,
    /// The discriminant of the value inside, or 0 if there is none.
    pub detail: u16,
}

/// The kind of storage account that the requests of a template go to.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum AzureNamespace {
    /// The account's namespace is not known. A template refuses only what
    /// both kinds of account refuse.
    Unknown = 0,
    /// A flat-namespace account.
    Flat = 1,
    /// A hierarchical-namespace account.
    Hierarchical = 2,
}

/// A corresponding Azure rejection, not a received response.
///
/// A zero status and empty code mean no mapping is known. The code points
/// at static memory and does not need to be freed.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AzureRejection {
    /// The corresponding HTTP status, or zero if unknown.
    pub status: u16,
    /// The service error code, or empty if unknown.
    pub code: Bytes,
}

/// Which kind of failure a `borink_status` carries.
///
/// These are the numbers that the protocol crate's error code uses.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum ErrorCode {
    /// Nothing failed.
    None = 0,
    /// The endpoint is not an ASCII HTTPS origin, or an HTTP one without
    /// `allow_http`.
    InvalidEndpoint = 1,
    /// The container name is not usable in a request.
    InvalidContainer = 2,
    /// The token is not usable as one HTTP header value.
    InvalidToken = 3,
    /// The plan cannot become a request. `detail` says why.
    InvalidPlan = 4,
    /// Output storage is too small. Grow it to `required` and call again.
    Capacity = 5,
    /// The response cannot be read. `detail` says which part was wrong.
    Response = 6,
}

/// The HTTP method of a request.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum Method {
    /// `GET`.
    Get = 1,
    /// `HEAD`.
    Head = 2,
    /// `PUT`.
    Put = 3,
    /// `DELETE`.
    Delete = 4,
}

/// What a read asks the service to return.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum GetKind {
    /// The bytes of the object.
    Bytes = 1,
    /// The properties and the metadata of the object, without its bytes.
    Head = 2,
}

/// Whether every read of a template sends a range, or none does.
///
/// A template fixes which fields its requests send, so a read with a range
/// and a read without one come from two templates.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum Ranging {
    /// No read sends a range: each one's `range` is `Whole`.
    Whole = 1,
    /// Every read sends a range.
    Ranged = 2,
}

/// Whether every read of a template names an earlier state of its object,
/// and which kind. Each read names its own in `revision`.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum RevisionKind {
    /// Every read names the object as it is now.
    Current = 1,
    /// Every read names a snapshot, by the timestamp Azure returned in
    /// `x-ms-snapshot`.
    Snapshot = 2,
    /// Every read names a version, by the identifier Azure returned in
    /// `x-ms-version-id`.
    Version = 3,
}

/// Which form of byte range a read requests.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum RangeForm {
    /// Every byte of the object. `start` and `end` are 0.
    Whole = 1,
    /// The half-open interval `start..end`.
    Bounded = 2,
    /// Every byte from `start` to the end of the object.
    Offset = 3,
    /// The last `start` bytes. Azure Blob Storage refuses this form.
    Suffix = 4,
}

/// The precondition that every request of a template carries.
///
/// Each request names what it compares against in its `condition_value`.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum Condition {
    /// The requests carry no precondition.
    None = 1,
    /// A request succeeds only if the current ETag matches.
    IfMatch = 2,
    /// A request succeeds only if the current ETag differs.
    IfNoneMatch = 3,
    /// A request succeeds only if the object changed after the instant it
    /// names.
    IfModifiedSince = 4,
    /// A request succeeds only if the object has not changed since the
    /// instant it names.
    IfUnmodifiedSince = 5,
}

/// What a `borink_condition_value` holds.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum ConditionForm {
    /// Nothing: the template carries no precondition.
    None = 0,
    /// An entity tag, or `*` for any, in `e_tag`.
    ETag = 1,
    /// An instant, in `seconds` since the Unix epoch. Read a date that the
    /// service sent with `borink_http_date_ms` and divide by 1000.
    Time = 2,
}

/// The kind of checksum that every request of a template sends.
///
/// These are the numbers that the protocol crate's checksum kind uses. Azure
/// takes an MD5 or a CRC64, and a template of any other is refused.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum ChecksumKind {
    /// No checksum.
    None = 0,
    /// MD5, sent as `content-md5`.
    Md5 = 1,
    /// CRC-64/NVME, sent as `x-ms-content-crc64`.
    Crc64 = 2,
}

/// Which stored version of a block a commit selects.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum BlockSource {
    /// The block staged and not yet committed.
    Uncommitted = 1,
    /// The block the blob holds now.
    Committed = 2,
    /// The block staged last, else the one the blob holds.
    Latest = 3,
}

/// The extra elements that a listing asks the service to write for each
/// object, as bits of `borink_list_options.include`.
#[repr(u32)]
#[derive(Clone, Copy)]
pub enum ListInclude {
    /// The metadata pairs of each object, as a `Metadata` element.
    Metadata = 1,
    /// An entry for each snapshot of an object, beside the object's own.
    Snapshots = 4,
    /// An entry for each version of an object.
    Versions = 8,
}

/// What one entry of a listing page names.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum EntryKind {
    /// One object.
    Object = 1,
    /// A group of keys that a delimited listing did not report one by one.
    Prefix = 2,
    /// A directory that the service keeps as its own entry. Only an Azure
    /// account with a hierarchical namespace reports one.
    Directory = 3,
    /// A delete marker in a listing of versions. Only S3 reports one, so
    /// these bindings, which speak to Azure, never do.
    DeleteMarker = 4,
}

/// The category of a service failure.
///
/// These are the numbers that the protocol crate's failure class uses. A number
/// that is not listed here comes from a later version of that crate. It
/// crosses unchanged, never as a substitute.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum FailureClass {
    /// The failure carries no category.
    None = 0,
    /// The service rejected the credentials or the authorization.
    Auth = 1,
    /// The service throttled the request. You can retry it later.
    Throttled = 2,
    /// The service failed, or it was unavailable.
    Server = 3,
    /// The service answered with a redirect.
    Redirect = 4,
    /// Any other failure, such as a malformed request.
    Other = 5,
}

/// The specific error that the service named.
///
/// These are the numbers that the protocol crate's service error kind uses, and
/// they cross unchanged in both directions.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum ServiceError {
    /// The service named no error.
    None = 0,
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
    /// The service refused the blocks of a commit: one is missing, or its ID
    /// is not one it takes.
    InvalidUpload = 10,
    /// The upload does not exist. Only S3 names one.
    NoSuchUpload = 11,
}

/// Which outcome a response head became.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutcomeKind {
    /// A body follows. Read it and put the bytes at `body`.
    Body = 1,
    /// No body follows and the read is complete.
    Complete = 2,
    /// The `If-None-Match` or `If-Modified-Since` condition held, so Azure
    /// sent no body.
    NotModified = 3,
    /// The condition did not hold, so Azure changed nothing.
    PreconditionFailed = 4,
    /// The object does not exist. A missing container is `ServiceFailure`
    /// with `failure.kind` `NoSuchContainer`.
    NotFound = 5,
    /// Azure cannot serve the requested range. Read `body.object_size`.
    RangeNotSatisfiable = 6,
    /// Azure stored the object. `meta` holds what the head said of it.
    Created = 7,
    /// Azure staged the block. `meta.e_tag` holds the entity tag, if the head
    /// carried one.
    Staged = 8,
    /// The head reports a failure but names no error.
    ///
    /// Read the response body, cap what you read, and pass it with `failure`
    /// to the operation's `borink_accept_*_error_body`.
    NeedErrorBody = 9,
    /// Azure refused the request, or it failed to carry it out.
    ServiceFailure = 10,
    /// The call was refused, or the response cannot be read. Read `error`.
    ///
    /// `error.detail` names the part that was wrong, not the value. You still
    /// hold the headers and the shape, so read those for the value itself.
    Invalid = 11,
    /// The protocol crate returned a variant that this crate does not know.
    Unsupported = 12,
    /// The page of a listing follows in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// `borink_fill_listing`. `body.expected_len` is the length of that body,
    /// and the other two values of `body` are absent.
    Page = 13,
    /// Azure committed the blob from its blocks. `meta` holds what the head
    /// said of it.
    Committed = 14,
}

/// The container that the requests of a template go to: what
/// `Container::new` and `Container::object` describe in the protocol crate.
///
/// Your program owns the values. They must stay valid for the call that
/// prepares a template, and no longer: a template holds no pointer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Container {
    /// The HTTPS origin of the storage account, or with `allow_http` an
    /// HTTP one.
    pub endpoint: Bytes,
    /// The container's name.
    pub name: Bytes,
    /// Whether `endpoint` may be an `http://` origin, such as an emulator's:
    /// `Container::new_allowing_http` rather than `Container::new`.
    ///
    /// Without TLS every request carries the token in clear. Leave this
    /// false, as a zeroed container has it, except for a local emulator or a
    /// network you trust.
    pub allow_http: bool,
    /// The key of the one object that every request of a template names, or
    /// empty for templates whose requests each name their own.
    ///
    /// A template of one object holds its key as checked and encoded text,
    /// so each request writes it without checking it. Pass that key, or an
    /// empty one, as each request's `key`. A listing names no object, and a
    /// listing template of such a container is refused.
    pub object: Bytes,
}

/// The length of a stored timestamp, in bytes.
pub const TIMESTAMPS_LEN: usize = 53;

/// An instant, formatted for the dates that a request carries: what
/// `Timestamps` is in the protocol crate, as it stores itself.
///
/// `borink_timestamps_from_unix` writes one. Write one each second, and pass
/// it to every request of that second, so that no request formats its dates
/// again. It holds no pointer. One whose dates are not text is refused with
/// `InvalidPlan`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Timestamps {
    /// The instant, as this library stores it.
    pub stored: [u8; TIMESTAMPS_LEN],
}

impl Default for Timestamps {
    fn default() -> Self {
        Self {
            stored: [0; TIMESTAMPS_LEN],
        }
    }
}

/// The length of a stored template, in bytes.
pub const TEMPLATE_LEN: usize = 80;

/// A template that a `borink_prepare_*` call wrote: the values that read the
/// bytes it wrote into your buffer.
///
/// Those bytes are the part of every request of one operation that is the
/// same for each object. The template holds no pointer. Keep it beside the bytes it was written into, and
/// pass both to each `borink_encode_*` call of its operation. You may copy
/// it, and store it with those bytes, such as in flash, to write requests
/// again without preparing again. It ends in a hash of the rest. A template
/// that was changed since it was written is refused with `InvalidPlan`, and
/// so is one that another version of the library wrote.
///
/// # Bytes
///
/// Encoding trusts that the bytes passed with a template are its own, and
/// checks nothing of them that would cost each request time. Bytes that are
/// not write a wrong request or are refused, but never make a call read
/// outside them. Where bytes could be another template's, such as bytes read
/// back from storage, check them once with `borink_template_check`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Template {
    /// The template, as this library stores it.
    pub stored: [u8; TEMPLATE_LEN],
}

impl Default for Template {
    fn default() -> Self {
        Self {
            stored: [0; TEMPLATE_LEN],
        }
    }
}

/// What a `borink_prepare_*` call wrote.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Prepared {
    /// Whether the template was written, and what stopped it.
    ///
    /// A `code` of 0 means that the template is in your buffer.
    pub status: Status,
    /// The number of bytes at the start of your buffer that the template
    /// takes, or with `Capacity` the number it needs.
    ///
    /// Those are its bytes: pass them to each request of the template.
    pub required: usize,
    /// The template, when `status.code` is 0.
    pub template: Template,
}

/// What a request's precondition compares against.
///
/// A template that carries a condition takes one in each request, and one
/// that carries none takes none.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ConditionValue {
    /// Which of the values below this holds, as a `borink_condition_form`.
    pub form: u16,
    /// The entity tag, or `*` for any, as `If-Match` and `If-None-Match`
    /// carry it.
    pub e_tag: Bytes,
    /// The instant, in seconds since the Unix epoch, which the request
    /// writes as an HTTP date.
    pub seconds: u64,
}

/// The byte range that a read requests.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Range {
    /// Which form of range this is, as a `borink_range_form`.
    pub form: u16,
    /// The first byte, or the length of a suffix.
    pub start: u64,
    /// The byte after the last byte, for a bounded range.
    pub end: u64,
}

/// What every read of a template does alike.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GetOptions {
    /// Whether the reads ask for bytes or for metadata, as a
    /// `borink_get_kind`.
    pub kind: u16,
    /// Whether every read sends a range, as a `borink_ranging`.
    pub ranging: u16,
    /// The precondition every read carries, as a `borink_condition`.
    pub condition: u16,
    /// Whether every read names a snapshot or a version, as a
    /// `borink_revision_kind`.
    pub revision: u16,
}

/// What one read of a template reads.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GetItem {
    /// The object key, before percent-encoding. Empty for a template of one
    /// object.
    pub key: Bytes,
    /// The bytes to read: `Whole` exactly when the template's `ranging` is.
    pub range: Range,
    /// What the template's precondition compares against.
    pub condition_value: ConditionValue,
    /// The snapshot or version to read, as Azure wrote it, when the
    /// template's `revision` names one; empty otherwise.
    pub revision: Bytes,
}

/// The part of a read that holds no pointer.
///
/// `borink_get_item_shape` returns it. Keep it while the request is in
/// flight, and pass it to `borink_accept_get_head` when the response
/// arrives. It is the whole per-request context: this crate keeps none of its
/// own.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GetShape {
    /// Whether the read asks for bytes or for metadata, as a
    /// `borink_get_kind`.
    pub kind: u16,
    /// The byte range that the read requests.
    pub range: Range,
    /// The precondition that the read carries, as a `borink_condition`.
    pub condition: u16,
}

/// The content properties that a write stores with each blob. An empty
/// value stores none.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ContentProperties {
    /// The media type, sent as `x-ms-blob-content-type`.
    pub content_type: Bytes,
    /// The content encoding, sent as `x-ms-blob-content-encoding`.
    pub content_encoding: Bytes,
    /// The content language, sent as `x-ms-blob-content-language`.
    pub content_language: Bytes,
    /// The content disposition, sent as `x-ms-blob-content-disposition`.
    pub content_disposition: Bytes,
    /// The cache control directives, sent as `x-ms-blob-cache-control`.
    pub cache_control: Bytes,
}

/// What every write of a template stores with each blob, beside its bytes.
///
/// Metadata and tags do not cross this boundary yet.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WriteOptions {
    /// The content properties.
    pub properties: ContentProperties,
    /// The access tier, such as `Hot` or `Cool`, or empty for the account's
    /// default.
    pub storage_class: Bytes,
}

/// What every whole-object write of a template does alike.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PutOptions {
    /// The precondition every write carries, as a `borink_condition`.
    pub condition: u16,
    /// What every write stores with each blob.
    pub options: WriteOptions,
    /// The checksum of the content that every write sends, as a
    /// `borink_checksum_kind`.
    pub checksum: u16,
}

/// What one whole-object write of a template writes.
///
/// The content stays in your program, and you send it after the head. Only
/// its length reaches the request.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PutItem {
    /// The object key. Empty for a template of one object.
    pub key: Bytes,
    /// The length of the content.
    pub content_len: u64,
    /// What the template's precondition compares against.
    pub condition_value: ConditionValue,
    /// The base64 checksum of the content, of the template's kind, or empty
    /// for a template that sends none.
    pub checksum: Bytes,
}

/// The part of a write that holds no pointer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PutShape {
    /// The precondition that the write carries, as a `borink_condition`.
    pub condition: u16,
}

/// What every Put Block of a template does alike.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct StageOptions {
    /// The checksum of each block that Azure checks, as a
    /// `borink_checksum_kind`.
    pub checksum: u16,
}

/// One Put Block, which stages one block of a blob.
///
/// The content stays in your program, as for a `borink_put_item`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct StageBlockItem {
    /// The key of the blob. Empty for a template of one object.
    pub key: Bytes,
    /// The block ID: base64 text of at most 64 bytes, of one length for
    /// every block of the blob.
    pub id: Bytes,
    /// The length of the block.
    pub content_len: u64,
    /// The base64 checksum of the block, of the template's kind, or empty
    /// for a template that sends none.
    pub checksum: Bytes,
}

/// What every Put Block List of a template does alike.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CommitOptions {
    /// The condition that each commit carries on the blob the key holds
    /// before, as a `borink_condition`.
    pub condition: u16,
    /// What every commit stores with each blob.
    pub options: WriteOptions,
    /// Whether each commit declares the MD5 of the blob it commits, which
    /// Azure stores and returns on a read but does not check.
    pub content_md5: bool,
}

/// One block of a commit.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BlockRef {
    /// The block ID, as it was staged.
    pub id: Bytes,
    /// Which stored version of the block to select, as a
    /// `borink_block_source`.
    pub source: u16,
}

/// One Put Block List, which commits a blob from its blocks.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CommitItem {
    /// The key of the blob. Empty for a template of one object.
    pub key: Bytes,
    /// What the template's precondition compares against.
    pub condition_value: ConditionValue,
    /// The blocks of the blob, in order: `block_count` of them, at most
    /// 50,000. The request's body is written from them, and they are not
    /// copied anywhere else.
    pub blocks: *const BlockRef,
    /// The number of blocks.
    pub block_count: usize,
    /// The base64 MD5 of the whole blob when the template's `content_md5`
    /// is set, and empty otherwise.
    pub content_md5: Bytes,
}

/// The part of a commit that holds no pointer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CommitShape {
    /// The precondition that the commit carries, as a `borink_condition`.
    pub condition: u16,
}

/// What every page of a listing template asks for alike.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ListOptions {
    /// The text that groups keys after the prefix, such as `/`, or empty to
    /// list every key. A delimited listing reports each group once, as an
    /// entry of kind `Prefix`.
    pub delimiter: Bytes,
    /// The most entries that one page reports.
    ///
    /// An absent number asks for the service's maximum. Azure applies that
    /// maximum to any larger number too: it answers 5,000 entries and a
    /// marker. The service may also report fewer entries than
    /// you asked for and still name a next page.
    pub max_results: MaybeU32,
    /// What each entry carries beside the object's own properties, as the
    /// bits of `borink_list_include` values.
    pub include: u32,
}

/// One page of a listing.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ListItem {
    /// The start of every key the listing names. Empty lists the whole
    /// container.
    pub prefix: Bytes,
    /// The `next_marker` of the page before, or empty for the first page.
    pub marker: Bytes,
}

/// One request, written into your buffer.
///
/// Every range is an offset into that buffer. Send `url` and each header
/// with an HTTP client that takes them, or write `head` to a connection
/// yourself. Then send the content: the body at `body` if the request has
/// one, or the content you named in the item.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WireRequest {
    /// Whether the request was written, and what stopped it.
    ///
    /// A `code` of 0 means that the request is in your buffer.
    pub status: Status,
    /// The number of bytes at the start of your buffer that the request
    /// takes, or with `Capacity` the number it needs. Size one buffer by
    /// the largest and reuse it.
    pub required: usize,
    /// The HTTP method, as a `borink_method`.
    pub method: u16,
    /// The URL.
    pub url: Span,
    /// The HTTP/1.1 head: the request line, `host`, every field line and the
    /// empty line that ends the head.
    pub head: Span,
    /// The field lines in the head, other than `host`, each written as
    /// `name: value` and a CRLF. Read them one by one with
    /// `borink_next_header`.
    pub fields: Span,
    /// The body that this library wrote, such as the block list of a
    /// commit, or absent for a request whose content you send.
    pub body: MaybeSpan,
    /// The length of the content that follows the head: the body's, or the
    /// content's that the item named.
    pub payload_len: u64,
}

/// One header field of a request.
///
/// # Lifetime
///
/// Both values point into your request buffer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Header {
    /// Whether the walk read one. A walk that has ended reports `false`.
    pub present: bool,
    /// The field name.
    pub name: Bytes,
    /// The field value.
    pub value: Bytes,
}

/// One response header, as the bytes that you already hold.
///
/// Build a small array of these from wherever your HTTP library keeps the
/// head, and reuse the array. This crate copies none of it.
///
/// # Lifetime
///
/// Both values must stay valid for as long as you use the outcome that the
/// reading call returns.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HeaderRef {
    /// The header name. A name that is not text is ignored.
    pub name: Bytes,
    /// The header value.
    pub value: Bytes,
}

/// Object metadata, borrowed from the response head.
///
/// # Lifetime
///
/// Every field points into the storage that the `borink_header_ref`s pointed
/// into, and is valid until you release or reuse it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ObjectMeta {
    /// The size of the whole object.
    pub size: MaybeU64,
    /// The entity tag.
    pub e_tag: MaybeBytes,
    /// The value of the `Last-Modified` header.
    pub last_modified: MaybeBytes,
    /// The version identifier.
    pub version: MaybeBytes,
    /// The value of the `Content-Encoding` header.
    pub content_encoding: MaybeBytes,
    /// Borrowed `Content-Type` header value; absent when not supplied.
    pub content_type: MaybeBytes,
}

/// Where the bytes of the response body belong in the object.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BodyWindow {
    /// The offset in the object of the first byte of the response body.
    pub object_offset: u64,
    /// The exact length of the response body.
    pub expected_len: MaybeU64,
    /// The size of the whole object.
    pub object_size: MaybeU64,
}

/// A response head that reports a failure.
///
/// Pass it back to the operation's `borink_accept_*_error_body` to finish a
/// `NeedErrorBody`.
///
/// A `NotFound` fills `kind` alone. A missing object is not a failure of the
/// head: it names an error and carries no status and no category, so both are
/// 0.
///
/// # Lifetime
///
/// `request_id` points into the storage that the `borink_header_ref`s pointed
/// into. Copy it if you keep this value past that storage.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Failure {
    /// The HTTP status code.
    pub status: u16,
    /// The category of the failure, as a `borink_failure_class`. Use it to
    /// decide whether to retry.
    ///
    /// This is `class` in the protocol crate, which C++ cannot spell.
    pub class: u16,
    /// The specific error that the head or the body named, as a
    /// `borink_service_error`.
    pub kind: u16,
    /// The value of the `x-ms-request-id` header.
    pub request_id: MaybeBytes,
}

/// The result of reading one response head.
///
/// One value describes the outcome of every operation. The fields that the
/// operation does not fill are absent.
///
/// # Lifetime
///
/// Everything that this value borrows is valid until you release or reuse the
/// storage that the `borink_header_ref`s pointed into.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Outcome {
    /// Which outcome this is, as a `borink_outcome_kind`.
    pub kind: u16,
    /// The metadata from the head.
    pub meta: ObjectMeta,
    /// Where the bytes of the body belong.
    pub body: BodyWindow,
    /// The failure, for `NeedErrorBody`, `ServiceFailure` and `NotFound`.
    pub failure: Failure,
    /// Why the call was refused, for `Invalid`.
    pub error: Status,
}

/// One entry of a listing page.
///
/// # Lifetime
///
/// `key`, `e_tag`, `last_modified` and `content_type` point into the body that
/// `borink_fill_listing` read. They are valid until you release or reuse that
/// buffer, or until the next call that reads it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ListEntry {
    /// What this entry names, as a `borink_entry_kind`.
    pub kind: u16,
    /// The object key, the shared start of the group, or the directory path.
    ///
    /// The bytes are UTF-8 text.
    pub key: Bytes,
    /// The size of the object. Absent for a group and for a directory.
    pub size: MaybeU64,
    /// The entity tag, as the listing wrote it.
    ///
    /// Azure lists an entity tag without the quotes that the `ETag` header
    /// carries. Both forms condition a request.
    pub e_tag: MaybeBytes,
    /// The value that the listing gave for the last modification, in the form
    /// that the `Last-Modified` header uses.
    pub last_modified: MaybeBytes,
    /// Borrowed media type, decoded from the listing when present.
    pub content_type: MaybeBytes,
    /// This entry as the service wrote it, from its opening tag to its closing
    /// one.
    ///
    /// Read a value that the fields above do not carry out of these bytes,
    /// with `borink_entry_property` or `borink_entry_properties`.
    pub raw: Bytes,
}

/// A walk over the values that one entry holds.
///
/// `borink_entry_properties` starts one and `borink_next_property` steps it.
/// The two values are this crate's own: keep the walk and pass it back.
///
/// # Lifetime
///
/// The walk points into the body that `borink_fill_listing` read, and is valid
/// for as long as the entry it came from.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Properties {
    /// The bytes of the entry that the walk has not read.
    pub remaining: Bytes,
    /// Whether the walk stands inside the properties element.
    pub within: bool,
}

/// One value that an entry holds.
///
/// # Lifetime
///
/// Both values point into the body that `borink_fill_listing` read, and are
/// valid until you release or reuse that buffer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Property {
    /// Whether the walk read one. A walk that has ended reports `false`.
    pub present: bool,
    /// The name of the element that held the value.
    pub name: Bytes,
    /// The value, as the service wrote it.
    pub value: Bytes,
}

/// An element that a listing writes for a blob, other than the four that
/// every `borink_list_entry` carries.
///
/// Name the ones you want in a `borink_property_set` and read the page with
/// `borink_fill_listing_with`, which keeps their values as it goes. Most are
/// written under the properties element; the ones marked otherwise stand
/// beside it. Read anything not listed here with `borink_entry_property`.
///
/// These are the numbers that the protocol crate's `BlobProperty` uses. A
/// `const` block in `layout.rs` fails the build if the two lists drift.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlobProperty {
    /// The access tier: `Hot`, `Cool`, `Cold` or `Archive`.
    AccessTier,
    /// Whether the tier was inferred rather than set.
    AccessTierInferred,
    /// When the tier was last changed.
    AccessTierChangeTime,
    /// The progress of a rehydration out of the archive tier.
    ArchiveStatus,
    /// The access control list, on a hierarchical account listing with permissions.
    Acl,
    /// `BlockBlob`, `PageBlob` or `AppendBlob`.
    BlobType,
    /// When the blob was created, in the form of the `Last-Modified` header.
    CreationTime,
    /// The media type, as stored with the blob.
    ContentType,
    /// The content encoding, as stored with the blob.
    ContentEncoding,
    /// The content language, as stored with the blob.
    ContentLanguage,
    /// The CRC64 of the content, if the service holds one.
    ContentCrc64,
    /// The MD5 of the content, base64, if the service holds one.
    ContentMd5,
    /// The cache control directives, as stored with the blob.
    CacheControl,
    /// The content disposition, as stored with the blob.
    ContentDisposition,
    /// The identifier of the last copy operation onto this blob.
    CopyId,
    /// The state of that copy: `pending`, `success`, `aborted` or `failed`.
    CopyStatus,
    /// The URL that copy read from.
    CopySource,
    /// The bytes copied so far and the total, as `copied/total`.
    CopyProgress,
    /// When that copy finished.
    CopyCompletionTime,
    /// Why that copy failed or was aborted.
    CopyStatusDescription,
    /// When a soft-deleted blob was deleted.
    DeletedTime,
    /// Whether the entry is a soft-deleted blob. Written beside the properties element.
    Deleted,
    /// The encryption scope the blob is stored under.
    EncryptionScope,
    /// When the blob expires, on a hierarchical account.
    ExpiryTime,
    /// The owning group, on a hierarchical account listing with permissions.
    Group,
    /// Whether this version is the current one. Written beside the properties element.
    IsCurrentVersion,
    /// Whether the blob is an incremental copy of a page blob snapshot.
    IncrementalCopy,
    /// Until when the immutability policy holds.
    ImmutabilityPolicyUntilDate,
    /// The immutability policy: `unlocked` or `locked`.
    ImmutabilityPolicyMode,
    /// Whether the blob is leased: `locked` or `unlocked`.
    LeaseStatus,
    /// The state of the lease: `available`, `leased`, `expired`, `breaking` or `broken`.
    LeaseState,
    /// Whether the lease is `infinite` or `fixed`.
    LeaseDuration,
    /// Whether a legal hold is set.
    LegalHold,
    /// The owner, on a hierarchical account listing with permissions.
    Owner,
    /// The POSIX permissions, on a hierarchical account listing with permissions.
    Permissions,
    /// How many days a soft-deleted blob is kept.
    RemainingRetentionDays,
    /// The priority of a rehydration out of the archive tier.
    RehydratePriority,
    /// Whether the blob is encrypted at rest.
    ServerEncrypted,
    /// The snapshot's timestamp, on an entry that names a snapshot. Written beside the properties element.
    Snapshot,
    /// How many tags the blob has.
    TagCount,
    /// The version's identifier, on an account that keeps versions. Written beside the properties element.
    VersionId,
    /// The sequence number of a page blob.
    BlobSequenceNumber,
    /// The metadata pairs, as the bytes of the element that holds them.
    Metadata,
}

/// The properties that one `borink_fill_listing_with` call is asked for.
///
/// One bit per property, at the bit that the property's number names. Start
/// from a zeroed value and add each property with `borink_property_set_with`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PropertySet {
    /// The bits. Bit `n` stands for the property numbered `n`.
    pub mask: u64,
}

/// What one call to `borink_fill_listing` read.
///
/// # Lifetime
///
/// `next_marker` points into the body that the call read, and is valid until
/// you release or reuse that buffer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Fill {
    /// Whether the page could be read.
    ///
    /// A `code` of 0 means that the entries are in your array. When it is
    /// `Capacity`, `required` is set; every other field is absent when the
    /// code is not 0.
    pub status: Status,
    /// The number of entries written into your array.
    ///
    /// The entries after these are untouched.
    pub filled: usize,
    /// The number of entries that the page holds, when the array had no room
    /// for all of them.
    ///
    /// The body has been decoded by then and cannot be read again. Ask the
    /// service for the page again, with an array of this many entries, or ask
    /// for a page no larger than your array.
    pub required: usize,
    /// The text that names the next page.
    ///
    /// Absent when the listing is complete. Copy the bytes into your own
    /// storage and pass them as the marker of the next request.
    pub next_marker: MaybeBytes,
}
