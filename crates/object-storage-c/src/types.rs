//! Every type that crosses the boundary.
//!
//! `cbindgen` generates `include/borink/object_storage.h` from this module, so
//! a C program's declarations cannot differ from these. `layout` checks that
//! both compilers lay them out the same way.

#![forbid(unsafe_code)]

/// The most headers that one request head carries.
///
/// This is the core crate's own bound, and the array in `borink_request_head`
/// has exactly this many slots. A compile-time assertion stops the build if
/// the core crate raises it.
pub const BORINK_MAX_HEADERS: usize = 6;

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
#[derive(Clone, Copy, Default)]
pub struct Span {
    /// The offset of the first byte.
    pub start: usize,
    /// The number of bytes.
    pub len: usize,
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

/// A failure, as the two numbers that describe every error of the core crate.
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

/// Which kind of failure a `borink_status` carries.
///
/// These are the numbers that the core crate's error code uses.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum ErrorCode {
    /// Nothing failed.
    None = 0,
    /// The endpoint is not an ASCII HTTP or HTTPS origin.
    InvalidEndpoint = 1,
    /// The container name is not usable in a request.
    InvalidContainer = 2,
    /// The token is not usable as one HTTP header value.
    InvalidToken = 3,
    /// The plan cannot become a request. `detail` says why.
    InvalidPlan = 4,
    /// The buffer is too small. Grow it to `required` and call again.
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
    /// The metadata of the object, without its bytes.
    Metadata = 2,
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

/// The ETag precondition that a request carries.
///
/// A request that carries one passes the entity tag as `condition_value`. A
/// request that carries none passes an empty `condition_value`.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum Condition {
    /// The request carries no precondition.
    None = 1,
    /// The request succeeds only if the current ETag matches.
    IfMatch = 2,
    /// The request succeeds only if the current ETag differs.
    IfNoneMatch = 3,
}

/// What a removal takes with it.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum DeleteKind {
    /// Remove the object alone. Azure refuses this if it has snapshots.
    Object = 1,
    /// Remove the object and its snapshots.
    ObjectAndSnapshots = 2,
    /// Remove the snapshots and keep the object.
    SnapshotsOnly = 3,
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
}

/// The category of a service failure.
///
/// These are the numbers that the core crate's failure class uses. A number
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
/// These are the numbers that the core crate's service error kind uses, and
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
}

/// Which outcome a response head became.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutcomeKind {
    /// A body follows. Read it and put the bytes at `body`.
    Body = 1,
    /// No body follows and the read is complete.
    Complete = 2,
    /// The `If-None-Match` condition held, so Azure sent no body.
    NotModified = 3,
    /// The condition did not hold, so Azure changed nothing.
    PreconditionFailed = 4,
    /// The object or its container does not exist. Read `failure.kind`.
    NotFound = 5,
    /// Azure cannot serve the requested range. Read `body.object_size`.
    RangeNotSatisfiable = 6,
    /// Azure stored the object.
    Done = 7,
    /// Azure accepted the removal.
    Accepted = 8,
    /// The head reports a failure but names no error.
    ///
    /// Read the response body, cap what you read, and pass it with `failure`
    /// to `borink_finish_get_error_body` or one of its two siblings.
    NeedErrorBody = 9,
    /// Azure refused the request, or it failed to carry it out.
    ServiceFailure = 10,
    /// The call was refused, or the response cannot be read. Read `error`.
    ///
    /// `error.detail` names the part that was wrong, not the value. You still
    /// hold the headers and the shape, so read those for the value itself.
    Invalid = 11,
    /// The core crate returned a variant that this crate does not know.
    Unsupported = 12,
    /// The page of a listing follows in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// `borink_fill_listing`. `body.expected_len` is the length of that body,
    /// and the other two values of `body` are absent.
    Page = 13,
}

/// One container, and the token that opens it.
///
/// Fill one in per client and keep it. Your program owns the three values, and
/// refreshing the token is assigning `token` again.
///
/// # Lifetime
///
/// The three values must stay valid for every call that takes this session.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Session {
    /// The HTTP or HTTPS origin of the storage account.
    pub endpoint: Bytes,
    /// The container name.
    pub container: Bytes,
    /// The Entra ID bearer token, without the `Bearer ` prefix.
    pub token: Bytes,
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

/// The part of a read plan that holds no borrows.
///
/// Store one of these while the request is in flight, and pass it to
/// `borink_accept_get_head` when the response arrives. It is the whole
/// per-request context: this crate keeps none of its own.
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

/// The part of a write plan that holds no borrows.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PutShape {
    /// The precondition that the write carries, as a `borink_condition`.
    pub condition: u16,
}

/// The part of a removal plan that holds no borrows.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DeleteShape {
    /// What the removal takes with it, as a `borink_delete_kind`.
    pub kind: u16,
    /// The precondition that the removal carries, as a `borink_condition`.
    pub condition: u16,
}

/// The part of a listing plan that holds no borrows.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ListShape {
    /// Whether the listing groups the keys at each `/` after the prefix.
    ///
    /// A delimited listing reports each group once, as an entry of kind
    /// `Prefix`, instead of reporting every key in it.
    pub delimited: bool,
    /// The most entries that one page reports.
    ///
    /// An absent number asks for the service's maximum, which Azure also
    /// applies to any larger number: it answers 5,000 entries and a marker
    /// rather than refusing. The service may also report fewer entries than
    /// you asked for and still name a next page.
    pub max_results: MaybeU32,
}

/// One request header, as two ranges of the request buffer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RequestHeader {
    /// The range that holds the header name.
    pub name: Span,
    /// The range that holds the header value.
    pub value: Span,
}

/// A request head, as ranges of the buffer that holds it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RequestHead {
    /// Whether the head was written, and what stopped it.
    ///
    /// A `code` of 0 means that the head is in your buffer.
    pub status: Status,
    /// The number of bytes that this request head needs.
    ///
    /// This is the exact size whenever the plan is valid, whether or not the
    /// head was written. Size one buffer by it and reuse that buffer.
    pub required: usize,
    /// The HTTP method, as a `borink_method`.
    pub method: u16,
    /// The range that holds the complete object URL.
    pub url: Span,
    /// How many of `headers` this request uses.
    pub header_count: usize,
    /// The headers, in the order that the core crate wrote them.
    pub headers: [RequestHeader; BORINK_MAX_HEADERS],
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
/// Store one of these and pass it back to `borink_finish_get_error_body` to
/// finish a `NeedErrorBody`.
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
    /// This is `class` in the core crate, which C++ cannot spell.
    pub class: u16,
    /// The specific error that the head or the body named, as a
    /// `borink_service_error`.
    pub kind: u16,
    /// The value of the `x-ms-request-id` header.
    pub request_id: MaybeBytes,
}

/// The result of reading one response head.
///
/// One value describes a read, a write and a removal. The fields that the
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
/// `key`, `e_tag` and `last_modified` point into the body that
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
/// These are the numbers that the core crate's `BlobProperty` uses. A
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
