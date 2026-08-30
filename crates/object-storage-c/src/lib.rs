//! Azure Blob Storage for a C or C++ program that owns its memory and its I/O.
//!
//! This crate builds HTTP request heads and reads HTTP response heads. It
//! never opens a socket, never reads the clock, and never allocates. Your
//! program supplies the buffer, the current time and the HTTP client.
//!
//! Include `borink/object_storage.h` and link the archive. A C++ program can
//! include `borink/object_storage.hpp` instead, which adds inline helpers over
//! the same declarations and needs no C++ runtime library.
//!
//! # How a read works
//!
//! A read has four steps.
//!
//! 1. Fill in a `borink_session` with the endpoint, the container and the
//!    token. Your program owns those bytes and keeps them.
//! 2. Describe the read as a `borink_get_shape`, and keep it while the request
//!    is in flight. It holds no pointer, so it outlives the key and ETag
//!    bytes.
//! 3. Call `borink_encode_get` to write the request head into your buffer. It
//!    returns a `borink_request_head`, which names the URL and each header by
//!    offset and length into that buffer. Send them with your HTTP client.
//! 4. Name each response header with a `borink_header_ref` and call
//!    `borink_accept_get_head` with the same `borink_get_shape`. It returns a
//!    `borink_outcome` whose `disposition` tells you what to do with the body.
//!
//! Pass the same shape to steps 3 and 4. The second call checks the response
//! against the plan, so you never restate what the shape already holds.
//!
//! A write has the same four steps, with `borink_put_shape`,
//! `borink_encode_put` and `borink_accept_put_head`. A removal has them with
//! `borink_delete_shape`, `borink_encode_delete` and
//! `borink_accept_delete_head`.
//!
//! An outcome whose disposition is `NeedErrorBody` is not final. Azure named
//! no error in the head, so read a bounded error body and pass it, with the
//! `failure` of that outcome, to `borink_finish_get_error_body`.
//!
//! # Example
//!
//! ```c
//! borink_session session = {as_bytes(endpoint), as_bytes(container), as_bytes(token)};
//! borink_status opened = borink_validate(&session);
//! if (opened.code != 0) { /* ... */ }
//!
//! borink_get_shape shape = {BORINK_GET_KIND_BYTES, {BORINK_RANGE_FORM_WHOLE, 0, 0},
//!                           BORINK_CONDITION_NONE};
//! borink_request_head head =
//!     borink_encode_get(&session, &shape, key, no_bytes, buffer, now);
//! if (head.status.code == BORINK_ERROR_CODE_CAPACITY) { /* grow to head.required */ }
//!
//! // ... send head.url and head.headers with your HTTP client ...
//!
//! borink_outcome outcome =
//!     borink_accept_get_head(&session, &shape, status, headers, header_count);
//! if (outcome.disposition == BORINK_DISPOSITION_BODY) { /* read the body */ }
//! ```
//!
//! # Sizing the buffer
//!
//! `borink_encode_get` refuses a buffer that is too small. It reports
//! `Capacity` in `status`, and the number of bytes it needs in `required`.
//! Call it with an empty buffer to learn that number, then size one buffer per
//! session and reuse it.
//!
//! # Where each value lives
//!
//! The request head is in your buffer, so `borink_request_head` names its
//! parts by offset rather than by pointer. Resizing the buffer moves the
//! bytes; the offsets still address them.
//!
//! The response head stays wherever your HTTP library put it. A
//! `borink_header_ref` points at those bytes, and every borrowed field of the
//! outcome points into the same bytes. This crate copies no part of a head,
//! and requires no particular layout of one.
//!
//! Each borrowed field states under its own `# Lifetime` when it stops being
//! valid.
//!
//! # Reading a failure
//!
//! No call returns a Rust `Result`, and nothing here throws. `borink_validate`
//! returns a `borink_status`, and `borink_request_head` and `borink_outcome`
//! each carry one.
//!
//! A status is two numbers. `code` is a `borink_error_code`, and `detail` is
//! the discriminant of the value inside it. Both are the core crate's own, and
//! `borink_describe_status` writes the sentence for a pair.
//!
//! A status names the part of the exchange that was wrong, not the value that
//! was wrong. You passed the headers and the shape in, so read those to find
//! the value.
//!
//! A response that Azure sends in normal operation is not a failure. A missing
//! object, a failed condition and a throttle each arrive as a disposition on
//! the outcome.
//!
//! Every other enum crosses as the number the core crate gives it, in both
//! directions. A number that this crate does not define is refused as
//! `InvalidPlan`, not read as another value.
//!
//! # What it costs
//!
//! Nothing here allocates, and nothing here reads a clock. Every call is total
//! over its inputs, so no call panics and none unwinds into your program.
//!
//! # Passing pointers
//!
//! Every entry point takes raw pointers, so each is an `unsafe extern "C"`
//! function with a `# Safety` section naming what it requires. A null
//! `session` or `shape` is refused as `InvalidPlan`, never dereferenced. A
//! `borink_bytes` whose `len` is 0 may have a null `ptr`.

#![no_std]

mod panic;

/// Links the standard library, for the panic handler and the unwinder that a
/// static archive carries on a hosted target, and for the test harness.
///
/// Nothing in this crate calls into it.
#[cfg(any(feature = "std", test))]
extern crate std;

use core::fmt::{self, Write as _};

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{
    Blobs, Container, DeleteHeadOutcome, Error, GetHeadOutcome, InvalidPlan, PhysicalDelete,
    PhysicalGet, PhysicalPut, PutHeadOutcome, ResponseHead, Timestamps, WireRequest,
};

/// The most headers that one request head carries.
///
/// This is the core crate's own bound, and the array in `borink_request_head`
/// has exactly this many slots. The assertion below stops the build if the
/// core crate raises it.
pub const BORINK_MAX_HEADERS: usize = 6;

// ---------------------------------------------------------------- primitives

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
#[derive(Clone, Copy)]
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
#[derive(Clone, Copy)]
pub struct MaybeBytes {
    /// Whether the head carried this value.
    pub present: bool,
    /// The bytes of it.
    pub bytes: Bytes,
}

/// A number that a response head may not carry.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MaybeU64 {
    /// Whether the head carried this number.
    pub present: bool,
    /// The number.
    pub value: u64,
}

/// A failure, as the two numbers that describe every error of the core crate.
///
/// `code` is a `borink_error_code`, and `detail` is the discriminant of the
/// value inside it. A `code` of 0 means that nothing failed. Both numbers are
/// append-only: a value defined today keeps its meaning.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
    /// The kind of failure, or 0 if there is none.
    pub code: u16,
    /// The discriminant of the value inside, or 0 if there is none.
    pub detail: u16,
}

// --------------------------------------------------------------------- enums

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

/// What a response tells you to do.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Disposition {
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
}

// ------------------------------------------------------------------- session

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

// --------------------------------------------------------------------- plans

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

// -------------------------------------------------------------- request head

/// One request header, as two ranges of the request buffer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RequestHeader {
    /// The range that holds the header name.
    pub name: Span,
    /// The range that holds the header value.
    pub value: Span,
}

/// A request head, as ranges of the buffer that holds it.
#[repr(C)]
#[derive(Clone, Copy)]
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

// ------------------------------------------------------- response and outcome

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
#[derive(Clone, Copy)]
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
#[derive(Clone, Copy)]
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
#[derive(Clone, Copy)]
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
#[derive(Clone, Copy)]
pub struct Outcome {
    /// What to do with the response, as a `borink_disposition`.
    pub disposition: u16,
    /// The metadata from the head.
    pub meta: ObjectMeta,
    /// Where the bytes of the body belong.
    pub body: BodyWindow,
    /// The failure, for `NeedErrorBody`, `ServiceFailure` and `NotFound`.
    pub failure: Failure,
    /// Why the call was refused, for `Invalid`.
    pub error: Status,
}

// ------------------------------------------------------------------- layout
//
// The header is generated from the declarations above, so the two spellings of
// a struct cannot drift. `borink_layout_disagrees` checks the remaining
// assumption: that a C compiler lays each of them out as `#[repr(C)]`
// promises. `tests/abi.c` fills a `borink_layout` with its own `sizeof`,
// `alignof` and `offsetof`, and this crate compares it field by field.

/// What a C compiler computes for the structs that cross this boundary.
///
/// Fill every field with the `sizeof`, `alignof` or `offsetof` that its name
/// gives, and pass it to `borink_layout_disagrees`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(missing_docs, reason = "each field is named by what it measures")]
pub struct Layout {
    pub sizeof_bytes: usize,
    pub alignof_bytes: usize,
    pub offsetof_bytes_len: usize,
    pub sizeof_bytes_mut: usize,
    pub alignof_bytes_mut: usize,
    pub sizeof_span: usize,
    pub offsetof_span_len: usize,
    pub sizeof_maybe_bytes: usize,
    pub alignof_maybe_bytes: usize,
    pub offsetof_maybe_bytes_bytes: usize,
    pub sizeof_maybe_u64: usize,
    pub alignof_maybe_u64: usize,
    pub offsetof_maybe_u64_value: usize,
    pub sizeof_status: usize,
    pub offsetof_status_detail: usize,
    pub sizeof_session: usize,
    pub offsetof_session_container: usize,
    pub offsetof_session_token: usize,
    pub sizeof_range: usize,
    pub alignof_range: usize,
    pub offsetof_range_start: usize,
    pub offsetof_range_end: usize,
    pub sizeof_get_shape: usize,
    pub offsetof_get_shape_range: usize,
    pub offsetof_get_shape_condition: usize,
    pub sizeof_put_shape: usize,
    pub sizeof_delete_shape: usize,
    pub offsetof_delete_shape_condition: usize,
    pub sizeof_request_header: usize,
    pub offsetof_request_header_value: usize,
    pub sizeof_request_head: usize,
    pub alignof_request_head: usize,
    pub offsetof_request_head_required: usize,
    pub offsetof_request_head_method: usize,
    pub offsetof_request_head_url: usize,
    pub offsetof_request_head_header_count: usize,
    pub offsetof_request_head_headers: usize,
    pub sizeof_header_ref: usize,
    pub offsetof_header_ref_value: usize,
    pub sizeof_object_meta: usize,
    pub offsetof_object_meta_e_tag: usize,
    pub offsetof_object_meta_last_modified: usize,
    pub offsetof_object_meta_version: usize,
    pub offsetof_object_meta_content_encoding: usize,
    pub sizeof_body_window: usize,
    pub offsetof_body_window_expected_len: usize,
    pub offsetof_body_window_object_size: usize,
    pub sizeof_failure: usize,
    pub offsetof_failure_class: usize,
    pub offsetof_failure_kind: usize,
    pub offsetof_failure_request_id: usize,
    pub sizeof_outcome: usize,
    pub alignof_outcome: usize,
    pub offsetof_outcome_meta: usize,
    pub offsetof_outcome_body: usize,
    pub offsetof_outcome_failure: usize,
    pub offsetof_outcome_error: usize,
}

/// The layout that this crate compiled to.
fn layout() -> Layout {
    use core::mem::offset_of;
    Layout {
        sizeof_bytes: size_of::<Bytes>(),
        alignof_bytes: align_of::<Bytes>(),
        offsetof_bytes_len: offset_of!(Bytes, len),
        sizeof_bytes_mut: size_of::<BytesMut>(),
        alignof_bytes_mut: align_of::<BytesMut>(),
        sizeof_span: size_of::<Span>(),
        offsetof_span_len: offset_of!(Span, len),
        sizeof_maybe_bytes: size_of::<MaybeBytes>(),
        alignof_maybe_bytes: align_of::<MaybeBytes>(),
        offsetof_maybe_bytes_bytes: offset_of!(MaybeBytes, bytes),
        sizeof_maybe_u64: size_of::<MaybeU64>(),
        alignof_maybe_u64: align_of::<MaybeU64>(),
        offsetof_maybe_u64_value: offset_of!(MaybeU64, value),
        sizeof_status: size_of::<Status>(),
        offsetof_status_detail: offset_of!(Status, detail),
        sizeof_session: size_of::<Session>(),
        offsetof_session_container: offset_of!(Session, container),
        offsetof_session_token: offset_of!(Session, token),
        sizeof_range: size_of::<Range>(),
        alignof_range: align_of::<Range>(),
        offsetof_range_start: offset_of!(Range, start),
        offsetof_range_end: offset_of!(Range, end),
        sizeof_get_shape: size_of::<GetShape>(),
        offsetof_get_shape_range: offset_of!(GetShape, range),
        offsetof_get_shape_condition: offset_of!(GetShape, condition),
        sizeof_put_shape: size_of::<PutShape>(),
        sizeof_delete_shape: size_of::<DeleteShape>(),
        offsetof_delete_shape_condition: offset_of!(DeleteShape, condition),
        sizeof_request_header: size_of::<RequestHeader>(),
        offsetof_request_header_value: offset_of!(RequestHeader, value),
        sizeof_request_head: size_of::<RequestHead>(),
        alignof_request_head: align_of::<RequestHead>(),
        offsetof_request_head_required: offset_of!(RequestHead, required),
        offsetof_request_head_method: offset_of!(RequestHead, method),
        offsetof_request_head_url: offset_of!(RequestHead, url),
        offsetof_request_head_header_count: offset_of!(RequestHead, header_count),
        offsetof_request_head_headers: offset_of!(RequestHead, headers),
        sizeof_header_ref: size_of::<HeaderRef>(),
        offsetof_header_ref_value: offset_of!(HeaderRef, value),
        sizeof_object_meta: size_of::<ObjectMeta>(),
        offsetof_object_meta_e_tag: offset_of!(ObjectMeta, e_tag),
        offsetof_object_meta_last_modified: offset_of!(ObjectMeta, last_modified),
        offsetof_object_meta_version: offset_of!(ObjectMeta, version),
        offsetof_object_meta_content_encoding: offset_of!(ObjectMeta, content_encoding),
        sizeof_body_window: size_of::<BodyWindow>(),
        offsetof_body_window_expected_len: offset_of!(BodyWindow, expected_len),
        offsetof_body_window_object_size: offset_of!(BodyWindow, object_size),
        sizeof_failure: size_of::<Failure>(),
        offsetof_failure_class: offset_of!(Failure, class),
        offsetof_failure_kind: offset_of!(Failure, kind),
        offsetof_failure_request_id: offset_of!(Failure, request_id),
        sizeof_outcome: size_of::<Outcome>(),
        alignof_outcome: align_of::<Outcome>(),
        offsetof_outcome_meta: offset_of!(Outcome, meta),
        offsetof_outcome_body: offset_of!(Outcome, body),
        offsetof_outcome_failure: offset_of!(Outcome, failure),
        offsetof_outcome_error: offset_of!(Outcome, error),
    }
}

/// Compares the layout a C compiler computed with the one this crate uses.
///
/// Returns the number of fields of `probe` that differ, and 0 when every one
/// agrees. Call it once at startup, or from a static assertion in your own
/// test, before you read a field of any struct here.
///
/// # Safety
///
/// `probe` must be null or point at one readable `borink_layout`. A null
/// `probe` counts every field as different.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_layout_disagrees(probe: *const Layout) -> usize {
    let ours = layout();
    let fields = size_of::<Layout>() / size_of::<usize>();
    if probe.is_null() {
        return fields;
    }
    // SAFETY: the caller states that `probe` points at one readable value, and
    // `Layout` is `usize` fields alone, so reading it as those is its layout.
    let (theirs, ours) = unsafe {
        (
            parts(probe.cast::<usize>(), fields),
            parts(core::ptr::from_ref(&ours).cast::<usize>(), fields),
        )
    };
    theirs
        .iter()
        .zip(ours)
        .filter(|(theirs, ours)| theirs != ours)
        .count()
}

// Every enum above crosses as a number. These pin the two lists to each other:
// a value renumbered on either side stops this build.
const _: () = {
    assert!(BORINK_MAX_HEADERS == proto::MAX_HEADERS);

    assert!(ErrorCode::InvalidEndpoint as u16 == proto::ErrorCode::InvalidEndpoint as u16);
    assert!(ErrorCode::InvalidContainer as u16 == proto::ErrorCode::InvalidContainer as u16);
    assert!(ErrorCode::InvalidToken as u16 == proto::ErrorCode::InvalidToken as u16);
    assert!(ErrorCode::InvalidPlan as u16 == proto::ErrorCode::InvalidPlan as u16);
    assert!(ErrorCode::Capacity as u16 == proto::ErrorCode::Capacity as u16);
    assert!(ErrorCode::Response as u16 == proto::ErrorCode::Response as u16);

    assert!(Method::Get as u16 == proto::Method::Get as u16);
    assert!(Method::Head as u16 == proto::Method::Head as u16);
    assert!(Method::Put as u16 == proto::Method::Put as u16);
    assert!(Method::Delete as u16 == proto::Method::Delete as u16);

    assert!(GetKind::Bytes as u16 == proto::GetKind::Bytes as u16);
    assert!(GetKind::Metadata as u16 == proto::GetKind::Metadata as u16);

    assert!(RangeForm::Whole as u16 == proto::RangeForm::Whole as u16);
    assert!(RangeForm::Bounded as u16 == proto::RangeForm::Bounded as u16);
    assert!(RangeForm::Offset as u16 == proto::RangeForm::Offset as u16);
    assert!(RangeForm::Suffix as u16 == proto::RangeForm::Suffix as u16);

    assert!(Condition::None as u16 == proto::ConditionKind::None as u16);
    assert!(Condition::IfMatch as u16 == proto::ConditionKind::IfMatch as u16);
    assert!(Condition::IfNoneMatch as u16 == proto::ConditionKind::IfNoneMatch as u16);

    assert!(DeleteKind::Object as u16 == proto::DeleteKind::Object as u16);
    assert!(DeleteKind::ObjectAndSnapshots as u16 == proto::DeleteKind::ObjectAndSnapshots as u16);
    assert!(DeleteKind::SnapshotsOnly as u16 == proto::DeleteKind::SnapshotsOnly as u16);

    assert!(FailureClass::Auth as u16 == proto::FailureClass::Auth as u16);
    assert!(FailureClass::Throttled as u16 == proto::FailureClass::Throttled as u16);
    assert!(FailureClass::Server as u16 == proto::FailureClass::Server as u16);
    assert!(FailureClass::Redirect as u16 == proto::FailureClass::Redirect as u16);
    assert!(FailureClass::Other as u16 == proto::FailureClass::Other as u16);

    assert!(ServiceError::NotFound as u16 == proto::ServiceErrorKind::NotFound as u16);
    assert!(
        ServiceError::NoSuchContainer as u16 == proto::ServiceErrorKind::NoSuchContainer as u16
    );
    assert!(ServiceError::AlreadyExists as u16 == proto::ServiceErrorKind::AlreadyExists as u16);
    assert!(ServiceError::Unauthorized as u16 == proto::ServiceErrorKind::Unauthorized as u16);
    assert!(ServiceError::Precondition as u16 == proto::ServiceErrorKind::Precondition as u16);
    assert!(
        ServiceError::RangeNotSatisfiable as u16
            == proto::ServiceErrorKind::RangeNotSatisfiable as u16
    );
    assert!(ServiceError::Throttled as u16 == proto::ServiceErrorKind::Throttled as u16);
    assert!(ServiceError::Timeout as u16 == proto::ServiceErrorKind::Timeout as u16);
    assert!(ServiceError::Service as u16 == proto::ServiceErrorKind::Service as u16);
};

// ------------------------------------------------------------------ pointers

/// Reads `len` items at `ptr` as a slice.
///
/// # Safety
///
/// `ptr` must be valid for `len` reads of `T`, aligned, and unwritten for the
/// lifetime `'a`. Any `ptr` is accepted when `len` is 0.
unsafe fn parts<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        return &[];
    }
    // SAFETY: the caller states that `ptr` addresses `len` items.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

/// Reads a writable buffer as a slice.
///
/// # Safety
///
/// `buf.ptr` must be valid for `buf.len` reads and writes, aligned, and
/// reached through no other reference for the lifetime `'a`. Any `ptr` is
/// accepted when `len` is 0.
unsafe fn parts_mut<'a>(buf: BytesMut) -> &'a mut [u8] {
    if buf.len == 0 {
        return &mut [];
    }
    // SAFETY: the caller states that `buf.ptr` addresses `buf.len` bytes and
    // that nothing else reaches them.
    unsafe { core::slice::from_raw_parts_mut(buf.ptr, buf.len) }
}

/// Reads borrowed bytes as a slice.
///
/// # Safety
///
/// As `parts`.
unsafe fn slice<'a>(bytes: Bytes) -> &'a [u8] {
    // SAFETY: the caller states the contract of `parts`.
    unsafe { parts(bytes.ptr, bytes.len) }
}

/// Reads a value the head may not have carried.
///
/// # Safety
///
/// As `parts`, when `value.present`.
unsafe fn maybe_slice<'a>(value: MaybeBytes) -> Option<&'a [u8]> {
    // SAFETY: the caller states the contract of `parts`.
    value.present.then(|| unsafe { slice(value.bytes) })
}

// ------------------------------------------------------------- entry points

/// Reports what is wrong with `session`, if anything.
///
/// A `code` of 0 means that the session can build requests. Every other call
/// makes the same check, so this exists to fail early.
///
/// # Safety
///
/// `session` must be null or point at one readable `borink_session` whose
/// three values are each readable for their length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_validate(session: *const Session) -> Status {
    // SAFETY: the caller states the contract of this function.
    match unsafe { usable(session) } {
        Ok(_) => Status { code: 0, detail: 0 },
        Err(status) => status,
    }
}

/// Writes the request head of a read into `buf`.
///
/// Pass an empty `condition_value` if `shape` carries no condition. Pass an
/// empty `buf` to learn the size that this request needs.
///
/// # Safety
///
/// `session` and `shape` must each be null or point at one readable value.
/// `key`, `condition_value` and `buf` must each address their stated length,
/// and `buf` must be reached through nothing else during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_get(
    session: *const Session,
    shape: *const GetShape,
    key: Bytes,
    condition_value: Bytes,
    buf: BytesMut,
    unix_seconds: u64,
) -> RequestHead {
    // SAFETY: the caller states the contract of this function.
    let planned = unsafe { planning(session, shape, get_shape, key) };
    let (blobs, shape, key) = match planned {
        Ok(planned) => planned,
        Err(status) => return refused(status, 0),
    };
    let now = Timestamps::from_unix(unix_seconds);
    // SAFETY: as above.
    let get = PhysicalGet::from_shape(shape, key, unsafe { condition(condition_value) });
    // SAFETY: as above.
    written(blobs.encode_get(unsafe { parts_mut(buf) }, &get, &now))
}

/// Writes the request head of a write into `buf`.
///
/// The head states `content_len`. You send those bytes yourself.
///
/// # Safety
///
/// As `borink_encode_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_put(
    session: *const Session,
    shape: *const PutShape,
    key: Bytes,
    condition_value: Bytes,
    buf: BytesMut,
    content_len: u64,
    unix_seconds: u64,
) -> RequestHead {
    // SAFETY: the caller states the contract of this function.
    let planned = unsafe { planning(session, shape, put_shape, key) };
    let (blobs, shape, key) = match planned {
        Ok(planned) => planned,
        Err(status) => return refused(status, 0),
    };
    let now = Timestamps::from_unix(unix_seconds);
    // SAFETY: as above.
    let put = PhysicalPut::from_shape(shape, key, unsafe { condition(condition_value) });
    // The content stays in your program. Only its length reaches the head, so
    // the request borrows no content and you send the bytes yourself.
    let content = proto::Payload::Streamed { len: content_len };
    // SAFETY: as above.
    written(blobs.encode_put(unsafe { parts_mut(buf) }, &put, content, &now))
}

/// Writes the request head of a removal into `buf`.
///
/// # Safety
///
/// As `borink_encode_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_delete(
    session: *const Session,
    shape: *const DeleteShape,
    key: Bytes,
    condition_value: Bytes,
    buf: BytesMut,
    unix_seconds: u64,
) -> RequestHead {
    // SAFETY: the caller states the contract of this function.
    let planned = unsafe { planning(session, shape, delete_shape, key) };
    let (blobs, shape, key) = match planned {
        Ok(planned) => planned,
        Err(status) => return refused(status, 0),
    };
    let now = Timestamps::from_unix(unix_seconds);
    // SAFETY: as above.
    let delete = PhysicalDelete::from_shape(shape, key, unsafe { condition(condition_value) });
    // SAFETY: as above.
    written(blobs.encode_delete(unsafe { parts_mut(buf) }, &delete, &now))
}

/// Reads the response head of a read.
///
/// Pass the same `shape` that you passed to `borink_encode_get`, and one
/// `borink_header_ref` per response header. The outcome points into the same
/// bytes as those headers.
///
/// # Safety
///
/// `session` and `shape` must each be null or point at one readable value.
/// `headers` must address `header_count` readable values.
///
/// # Lifetime
///
/// The bytes that `headers` points at must stay valid, and must not move, for
/// as long as you use the returned outcome.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_get_head(
    session: *const Session,
    shape: *const GetShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    match unsafe { reading(session, shape, get_shape, status, headers, header_count) } {
        Ok((blobs, shape, head)) => match blobs.accept_get_head(shape, head) {
            Ok(outcome) => get_outcome(&outcome),
            Err(error) => invalid(status_of(&error)),
        },
        Err(status) => invalid(status),
    }
}

/// Reads the response head of a write.
///
/// # Safety
///
/// As `borink_accept_get_head`.
///
/// # Lifetime
///
/// As `borink_accept_get_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_put_head(
    session: *const Session,
    shape: *const PutShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    match unsafe { reading(session, shape, put_shape, status, headers, header_count) } {
        Ok((blobs, shape, head)) => match blobs.accept_put_head(shape, head) {
            Ok(outcome) => put_outcome(&outcome),
            Err(error) => invalid(status_of(&error)),
        },
        Err(status) => invalid(status),
    }
}

/// Reads the response head of a removal.
///
/// # Safety
///
/// As `borink_accept_get_head`.
///
/// # Lifetime
///
/// As `borink_accept_get_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_delete_head(
    session: *const Session,
    shape: *const DeleteShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    match unsafe { reading(session, shape, delete_shape, status, headers, header_count) } {
        Ok((blobs, shape, head)) => match blobs.accept_delete_head(shape, head) {
            Ok(outcome) => delete_outcome(&outcome),
            Err(error) => invalid(status_of(&error)),
        },
        Err(status) => invalid(status),
    }
}

/// Finishes a read whose head asked for the error body.
///
/// Pass the `failure` of that outcome and the body that you read. Pass an
/// empty body if you read none: the outcome is then final with the error
/// unnamed.
///
/// # Safety
///
/// `session` and `failure` must each be null or point at one readable value.
/// `body` must address its stated length.
///
/// # Lifetime
///
/// `failure->request_id` must still point at valid bytes, and they must stay
/// valid for as long as you use the returned outcome. So must `body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_finish_get_error_body(
    session: *const Session,
    failure: *const Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    match unsafe { finishing(session, failure) } {
        // SAFETY: as above.
        Ok((blobs, status, id)) => {
            get_outcome(&blobs.accept_error_body(status, id, unsafe { slice(body) }))
        }
        Err(status) => invalid(status),
    }
}

/// Finishes a write whose head asked for the error body.
///
/// # Safety
///
/// As `borink_finish_get_error_body`.
///
/// # Lifetime
///
/// As `borink_finish_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_finish_put_error_body(
    session: *const Session,
    failure: *const Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    match unsafe { finishing(session, failure) } {
        // SAFETY: as above.
        Ok((blobs, status, id)) => {
            put_outcome(&blobs.accept_put_error_body(status, id, unsafe { slice(body) }))
        }
        Err(status) => invalid(status),
    }
}

/// Finishes a removal whose head asked for the error body.
///
/// # Safety
///
/// As `borink_finish_get_error_body`.
///
/// # Lifetime
///
/// As `borink_finish_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_finish_delete_error_body(
    session: *const Session,
    failure: *const Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    match unsafe { finishing(session, failure) } {
        // SAFETY: as above.
        Ok((blobs, status, id)) => {
            delete_outcome(&blobs.accept_delete_error_body(status, id, unsafe { slice(body) }))
        }
        Err(status) => invalid(status),
    }
}

/// Writes one sentence naming what `outcome` says.
///
/// Returns the length of the whole sentence, which may be longer than `into`.
/// The part that fits is written. A null `outcome` writes nothing and returns
/// 0.
///
/// # Safety
///
/// `outcome` must be null or point at one readable value whose borrowed fields
/// still address valid bytes. `into` must address its stated length and be
/// reached through nothing else during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_describe(outcome: *const Outcome, into: BytesMut) -> usize {
    if outcome.is_null() {
        return 0;
    }
    // SAFETY: the caller states the contract of this function.
    unsafe { describe(&*outcome, parts_mut(into)) }
}

/// Writes one sentence naming what `status` says.
///
/// Returns the length of the whole sentence, exactly as `borink_describe`.
///
/// # Safety
///
/// `into` must address its stated length and be reached through nothing else
/// during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_describe_status(status: Status, into: BytesMut) -> usize {
    // SAFETY: the caller states the contract of this function.
    describe_status(status, unsafe { parts_mut(into) })
}

// ----------------------------------------------------------------- the steps

// What every call needs before the core crate sees it: a session whose three
// values name a container that can be addressed.
//
// # Safety
//
// As `borink_validate`.
unsafe fn usable<'a>(session: *const Session) -> Result<Blobs<'a>, Status> {
    if session.is_null() {
        return Err(unknown());
    }
    // SAFETY: the caller states that `session` points at one readable value.
    let session = unsafe { *session };
    // SAFETY: as above, for the three values it holds.
    let (endpoint, container, token) = unsafe {
        (
            slice(session.endpoint),
            slice(session.container),
            slice(session.token),
        )
    };
    // A value that is not text cannot be the thing it names. It fails as that
    // thing, not as a fourth kind of fault.
    let (Ok(endpoint), Ok(container), Ok(token)) = (
        core::str::from_utf8(endpoint),
        core::str::from_utf8(container),
        core::str::from_utf8(token),
    ) else {
        let code = match (
            core::str::from_utf8(endpoint).is_err(),
            core::str::from_utf8(container).is_err(),
        ) {
            (true, _) => ErrorCode::InvalidEndpoint,
            (_, true) => ErrorCode::InvalidContainer,
            _ => ErrorCode::InvalidToken,
        };
        return Err(Status {
            code: code as u16,
            detail: 0,
        });
    };
    Blobs::new(
        Container::new(endpoint, container).map_err(|error| status_of(&error))?,
        token,
    )
    .map_err(|error| status_of(&error))
}

// What every request needs on top of that: a shape that was passed, a key that
// is text, and the plan's shape as the core crate spells it.
//
// # Safety
//
// As `borink_encode_get`.
unsafe fn planning<'a, V, S>(
    session: *const Session,
    shape: *const V,
    convert: impl FnOnce(&V) -> Result<S, Status>,
    key: Bytes,
) -> Result<(Blobs<'a>, S, &'a str), Status> {
    // SAFETY: the caller states the contract of this function.
    let blobs = unsafe { usable(session) }?;
    if shape.is_null() {
        return Err(unknown());
    }
    // SAFETY: as above.
    let Ok(key) = core::str::from_utf8(unsafe { slice(key) }) else {
        return Err(status_of(&Error::InvalidPlan(InvalidPlan::Key)));
    };
    // SAFETY: as above.
    Ok((blobs, convert(unsafe { &*shape })?, key))
}

// What every reading call needs: the same shape the request was planned with,
// and the head where your HTTP library already put it.
//
// # Safety
//
// As `borink_accept_get_head`.
unsafe fn reading<'a, V, S>(
    session: *const Session,
    shape: *const V,
    convert: impl FnOnce(&V) -> Result<S, Status>,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Result<(Blobs<'a>, S, ResponseHead<'a>), Status> {
    // SAFETY: the caller states the contract of this function.
    let blobs = unsafe { usable(session) }?;
    if shape.is_null() {
        return Err(unknown());
    }
    // SAFETY: as above.
    let shape = convert(unsafe { &*shape })?;
    // SAFETY: as above.
    Ok((blobs, shape, unsafe {
        head_of(status, parts(headers, header_count))
    }))
}

// What every finishing call needs. The status and the request identifier are
// the plain values the outcome carried, so nothing is read twice.
//
// # Safety
//
// As `borink_finish_get_error_body`.
unsafe fn finishing<'a>(
    session: *const Session,
    failure: *const Failure,
) -> Result<(Blobs<'a>, u16, Option<&'a [u8]>), Status> {
    // SAFETY: the caller states the contract of this function.
    let blobs = unsafe { usable(session) }?;
    if failure.is_null() {
        return Err(unknown());
    }
    // SAFETY: as above.
    let failure = unsafe { *failure };
    // SAFETY: as above, for the request identifier it borrows.
    Ok((blobs, failure.status, unsafe {
        maybe_slice(failure.request_id)
    }))
}

// The head, read where your HTTP library already put it. A name that is not
// text is skipped: the core crate looks for its headers by text, so such a
// name is none of them.
//
// # Safety
//
// Every `HeaderRef` in `headers` must address its stated bytes.
unsafe fn head_of<'a>(status: u16, headers: &[HeaderRef]) -> ResponseHead<'a> {
    ResponseHead::from_headers(
        status,
        headers.iter().filter_map(|header| {
            // SAFETY: the caller states that both values are readable.
            let (name, value) = unsafe { (slice(header.name), slice(header.value)) };
            Some((core::str::from_utf8(name).ok()?, value))
        }),
    )
}

// The written head, or the exact size that it needed, or why the plan was
// refused. All three are one status and one `required`.
fn written(request: proto::Result<WireRequest<'_>>) -> RequestHead {
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            let required = error.capacity().map_or(0, |capacity| capacity.required);
            return refused(status_of(&error), required);
        }
    };
    let mut headers = empty_headers();
    let mut end = request.url_span().start + request.url_span().len;
    for (slot, (name, value)) in headers.iter_mut().zip(request.header_spans()) {
        slot.name = span(name);
        slot.value = span(value);
        end = end.max(value.start + value.len);
    }
    RequestHead {
        status: Status { code: 0, detail: 0 },
        required: end,
        method: request.method() as u16,
        url: span(request.url_span()),
        header_count: request.header_spans().len(),
        headers,
    }
}

fn refused(status: Status, required: usize) -> RequestHead {
    RequestHead {
        status,
        required,
        method: Method::Get as u16,
        url: Span { start: 0, len: 0 },
        header_count: 0,
        headers: empty_headers(),
    }
}

fn empty_headers() -> [RequestHeader; BORINK_MAX_HEADERS] {
    [RequestHeader {
        name: Span { start: 0, len: 0 },
        value: Span { start: 0, len: 0 },
    }; BORINK_MAX_HEADERS]
}

fn span(span: proto::Span) -> Span {
    Span {
        start: span.start,
        len: span.len,
    }
}

fn status_of(error: &Error) -> Status {
    Status {
        code: error.code() as u16,
        detail: error.detail(),
    }
}

// A number that names no value of the core crate's enum. It is refused as an
// invalid plan, and the plan is never read as the value that happens to be
// oldest.
fn unknown() -> Status {
    status_of(&Error::InvalidPlan(InvalidPlan::Unknown))
}

// # Safety
//
// `value` must address its stated bytes.
unsafe fn condition<'a>(value: Bytes) -> Option<&'a [u8]> {
    // SAFETY: the caller states that `value` is readable.
    let value = unsafe { slice(value) };
    (!value.is_empty()).then_some(value)
}

// ---------------------------------------------------------- plans, both ways

fn get_shape(shape: &GetShape) -> Result<proto::GetShape, Status> {
    Ok(proto::GetShape {
        kind: proto::GetKind::from_discriminant(shape.kind).ok_or_else(unknown)?,
        range: proto::RequestedRange::from_parts(
            proto::RangeForm::from_discriminant(shape.range.form).ok_or_else(unknown)?,
            shape.range.start,
            shape.range.end,
        ),
        condition: condition_kind(shape.condition)?,
    })
}

fn put_shape(shape: &PutShape) -> Result<proto::PutShape, Status> {
    Ok(proto::PutShape {
        condition: condition_kind(shape.condition)?,
    })
}

fn delete_shape(shape: &DeleteShape) -> Result<proto::DeleteShape, Status> {
    Ok(proto::DeleteShape {
        kind: proto::DeleteKind::from_discriminant(shape.kind).ok_or_else(unknown)?,
        condition: condition_kind(shape.condition)?,
    })
}

fn condition_kind(condition: u16) -> Result<proto::ConditionKind, Status> {
    proto::ConditionKind::from_discriminant(condition).ok_or_else(unknown)
}

// ------------------------------------------------------- outcomes, both ways

fn class_of(class: u16) -> Option<proto::FailureClass> {
    proto::FailureClass::from_discriminant(class)
}

fn kind_view(kind: Option<proto::ServiceErrorKind>) -> u16 {
    kind.map_or(0, |kind| kind as u16)
}

fn kind_of(kind: u16) -> Option<proto::ServiceErrorKind> {
    proto::ServiceErrorKind::from_discriminant(kind)
}

fn failure_view(failure: &proto::Failure<'_>) -> Failure {
    Failure {
        status: failure.status,
        class: failure.class as u16,
        kind: kind_view(failure.kind),
        request_id: maybe_bytes(failure.request_id),
    }
}

// The failure that the twin carries, as the core crate's own record, so that
// the sentence for it is the core crate's own too. It is `None` only for a
// category that a later core crate defined and this crate cannot name.
//
// # Safety
//
// `failure.request_id` must still address its stated bytes.
unsafe fn failure_of<'a>(failure: &Failure) -> Option<proto::Failure<'a>> {
    Some(proto::Failure {
        status: failure.status,
        class: class_of(failure.class)?,
        kind: kind_of(failure.kind),
        // SAFETY: the caller states that the identifier is readable.
        request_id: unsafe { maybe_slice(failure.request_id) },
    })
}

// A named error and nothing else. A missing object is not a failure of the
// head: the core crate's variant carries a kind alone, and so does this.
fn named_error(kind: Option<proto::ServiceErrorKind>) -> Failure {
    Failure {
        status: 0,
        class: 0,
        kind: kind_view(kind),
        request_id: absent_bytes(),
    }
}

fn get_outcome(outcome: &GetHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(Disposition::Unsupported);
    match *outcome {
        GetHeadOutcome::Body { meta, body } => {
            view.disposition = Disposition::Body as u16;
            view.meta = meta_view(&meta);
            view.body = body_view(&body);
        }
        GetHeadOutcome::Complete { meta } => {
            view.disposition = Disposition::Complete as u16;
            view.meta = meta_view(&meta);
        }
        GetHeadOutcome::NotModified { e_tag } => {
            view.disposition = Disposition::NotModified as u16;
            view.meta.e_tag = maybe_bytes(e_tag);
        }
        GetHeadOutcome::PreconditionFailed => {
            view.disposition = Disposition::PreconditionFailed as u16;
        }
        GetHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound as u16;
            view.failure = named_error(kind);
        }
        GetHeadOutcome::RangeNotSatisfiable { object_size } => {
            view.disposition = Disposition::RangeNotSatisfiable as u16;
            view.body.object_size = maybe_number(object_size);
        }
        GetHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody as u16;
            view.failure = failure_view(&failure);
        }
        GetHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure as u16;
            view.failure = failure_view(&failure);
        }
        // The outcome is sealed, so a later version can add a variant. Report
        // one that this crate does not know rather than guessing at it.
        _ => {}
    }
    view
}

fn put_outcome(outcome: &PutHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(Disposition::Unsupported);
    match *outcome {
        PutHeadOutcome::Created { meta } => {
            view.disposition = Disposition::Done as u16;
            view.meta = meta_view(&meta);
        }
        PutHeadOutcome::PreconditionFailed => {
            view.disposition = Disposition::PreconditionFailed as u16;
        }
        PutHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound as u16;
            view.failure = named_error(kind);
        }
        PutHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody as u16;
            view.failure = failure_view(&failure);
        }
        PutHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure as u16;
            view.failure = failure_view(&failure);
        }
        _ => {}
    }
    view
}

fn delete_outcome(outcome: &DeleteHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(Disposition::Unsupported);
    match *outcome {
        // A removal returns no object, so Azure sends no metadata for one.
        DeleteHeadOutcome::Accepted => view.disposition = Disposition::Accepted as u16,
        DeleteHeadOutcome::PreconditionFailed => {
            view.disposition = Disposition::PreconditionFailed as u16;
        }
        DeleteHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound as u16;
            view.failure = named_error(kind);
        }
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody as u16;
            view.failure = failure_view(&failure);
        }
        DeleteHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure as u16;
            view.failure = failure_view(&failure);
        }
        _ => {}
    }
    view
}

fn invalid(status: Status) -> Outcome {
    let mut view = empty_outcome(Disposition::Invalid);
    view.error = status;
    view
}

fn empty_outcome(disposition: Disposition) -> Outcome {
    Outcome {
        disposition: disposition as u16,
        meta: ObjectMeta {
            size: absent_number(),
            e_tag: absent_bytes(),
            last_modified: absent_bytes(),
            version: absent_bytes(),
            content_encoding: absent_bytes(),
        },
        body: BodyWindow {
            object_offset: 0,
            expected_len: absent_number(),
            object_size: absent_number(),
        },
        failure: named_error(None),
        error: Status { code: 0, detail: 0 },
    }
}

fn meta_view(meta: &proto::ObjectMeta<'_>) -> ObjectMeta {
    ObjectMeta {
        size: maybe_number(meta.size),
        e_tag: maybe_bytes(meta.e_tag),
        last_modified: maybe_bytes(meta.last_modified),
        version: maybe_bytes(meta.version),
        content_encoding: maybe_bytes(meta.content_encoding),
    }
}

fn body_view(body: &proto::BodyWindow) -> BodyWindow {
    BodyWindow {
        object_offset: body.object_offset,
        expected_len: maybe_number(body.expected_len),
        object_size: maybe_number(body.object_size),
    }
}

fn maybe_bytes(value: Option<&[u8]>) -> MaybeBytes {
    match value {
        Some(bytes) => MaybeBytes {
            present: true,
            bytes: Bytes {
                ptr: bytes.as_ptr(),
                len: bytes.len(),
            },
        },
        None => absent_bytes(),
    }
}

fn absent_bytes() -> MaybeBytes {
    MaybeBytes {
        present: false,
        bytes: Bytes {
            ptr: core::ptr::null(),
            len: 0,
        },
    }
}

fn maybe_number(value: Option<u64>) -> MaybeU64 {
    match value {
        Some(value) => MaybeU64 {
            present: true,
            value,
        },
        None => absent_number(),
    }
}

fn number(value: MaybeU64) -> Option<u64> {
    value.present.then_some(value.value)
}

fn absent_number() -> MaybeU64 {
    MaybeU64 {
        present: false,
        value: 0,
    }
}

// ---------------------------------------------------------------- sentences

fn disposition_of(value: u16) -> Option<Disposition> {
    Some(match value {
        1 => Disposition::Body,
        2 => Disposition::Complete,
        3 => Disposition::NotModified,
        4 => Disposition::PreconditionFailed,
        5 => Disposition::NotFound,
        6 => Disposition::RangeNotSatisfiable,
        7 => Disposition::Done,
        8 => Disposition::Accepted,
        9 => Disposition::NeedErrorBody,
        10 => Disposition::ServiceFailure,
        11 => Disposition::Invalid,
        12 => Disposition::Unsupported,
        _ => return None,
    })
}

// # Safety
//
// Every borrowed field of `outcome` must still address its stated bytes.
unsafe fn describe(outcome: &Outcome, into: &mut [u8]) -> usize {
    match disposition_of(outcome.disposition) {
        Some(Disposition::Invalid) => describe_status(outcome.error, into),
        // The core crate wrote the sentence for a failure and for an
        // unsatisfiable range, and both carry numbers that no table holds.
        // The twin carries every field of them, so the sentence is borrowed.
        Some(Disposition::NeedErrorBody | Disposition::ServiceFailure) => {
            // SAFETY: the caller states that the identifier is readable.
            match unsafe { failure_of(&outcome.failure) } {
                Some(failure) => say(into, &failure),
                None => say(
                    into,
                    &"the service failed in a way that this crate cannot name",
                ),
            }
        }
        Some(Disposition::RangeNotSatisfiable) => say(
            into,
            &GetHeadOutcome::RangeNotSatisfiable {
                object_size: number(outcome.body.object_size),
            },
        ),
        // A missing object names an error and carries nothing else, so the
        // error is the whole sentence.
        Some(Disposition::NotFound) => match kind_of(outcome.failure.kind) {
            Some(kind) => say(into, &kind),
            None => say(into, &"the object or its container does not exist"),
        },
        // One literal per remaining disposition. They say less than the core
        // crate's own sentences, which name the operation: one outcome type
        // crosses for all three operations, so the sentence names none.
        settled => say(into, &settled_sentence(settled)),
    }
}

fn settled_sentence(disposition: Option<Disposition>) -> &'static str {
    match disposition {
        Some(Disposition::Body) => "the object follows in the response body",
        Some(Disposition::Complete) => "the response carries no body and is complete",
        Some(Disposition::NotModified) => "the object is not modified",
        Some(Disposition::PreconditionFailed) => "the condition did not hold",
        Some(Disposition::Done) => "the service stored the object",
        Some(Disposition::Accepted) => "the service accepted the removal",
        _ => "the core crate returned an outcome that this crate does not know",
    }
}

fn describe_status(status: Status, into: &mut [u8]) -> usize {
    let Some(code) = proto::ErrorCode::from_discriminant(status.code) else {
        return say(into, &"nothing failed");
    };
    match Error::from_parts(code, status.detail) {
        Some(error) => say(into, &error),
        // A capacity error carries sizes rather than a discriminant, and a
        // detail from a later version names nothing here.
        None => say(into, &code.as_str()),
    }
}

// Writes what `reason` says into `into`, and returns the length of the whole
// sentence. Writing keeps counting after the buffer is full, so a caller that
// wants all of it learns the size and calls again.
fn say(into: &mut [u8], reason: &dyn fmt::Display) -> usize {
    let mut sentence = Sentence { into, used: 0 };
    // Display for these types never fails, and a full buffer is not a failure
    // here: the count is the answer.
    let _ = write!(sentence, "{reason}");
    sentence.used
}

struct Sentence<'a> {
    into: &'a mut [u8],
    used: usize,
}

impl fmt::Write for Sentence<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.used + text.len();
        if end <= self.into.len() {
            self.into[self.used..end].copy_from_slice(text.as_bytes());
        }
        self.used = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borink_object_storage_proto::{
        BodyWindow as CoreBodyWindow, Failure as CoreFailure, FailureClass as CoreFailureClass,
        ObjectMeta as CoreObjectMeta, RequestedRange, ResponseFault, ServiceErrorKind,
    };
    use std::string::{String, ToString};
    use std::vec;
    use std::vec::Vec;

    // Two buffers, so that nothing here depends on one contiguous head.
    const VALUES: &[u8] = b"\"etag\"Wed, 26 Aug 2026 12:00:00 GMTversion-1gzip";
    const IDENTIFIER: &[u8] = b"request-123";
    const ENDPOINT: &[u8] = b"https://account.blob.core.windows.net";
    const CONTAINER: &[u8] = b"container";
    const TOKEN: &[u8] = b"token";

    fn e_tag() -> &'static [u8] {
        &VALUES[..6]
    }

    fn lent(value: &[u8]) -> Bytes {
        Bytes {
            ptr: value.as_ptr(),
            len: value.len(),
        }
    }

    fn writable(value: &mut [u8]) -> BytesMut {
        BytesMut {
            ptr: value.as_mut_ptr(),
            len: value.len(),
        }
    }

    fn opened(endpoint: &[u8], container: &[u8], token: &[u8]) -> Session {
        Session {
            endpoint: lent(endpoint),
            container: lent(container),
            token: lent(token),
        }
    }

    fn session() -> Session {
        opened(ENDPOINT, CONTAINER, TOKEN)
    }

    fn whole() -> Range {
        Range {
            form: RangeForm::Whole as u16,
            start: 0,
            end: 0,
        }
    }

    fn read_shape() -> GetShape {
        GetShape {
            kind: GetKind::Bytes as u16,
            range: whole(),
            condition: Condition::None as u16,
        }
    }

    fn write_shape() -> PutShape {
        PutShape {
            condition: Condition::None as u16,
        }
    }

    fn header(name: &'static str, value: &'static [u8]) -> HeaderRef {
        HeaderRef {
            name: lent(name.as_bytes()),
            value: lent(value),
        }
    }

    fn text(outcome: &Outcome) -> String {
        let mut into = [0; 256];
        // SAFETY: `outcome` and `into` are both live, and nothing else reaches
        // the buffer while the call writes it.
        let length = unsafe { borink_describe(outcome, writable(&mut into)) };
        assert!(length <= into.len(), "{length}");
        String::from_utf8(into[..length].to_vec()).unwrap()
    }

    fn full_meta() -> CoreObjectMeta<'static> {
        CoreObjectMeta {
            size: Some(10),
            e_tag: Some(e_tag()),
            last_modified: Some(&VALUES[6..35]),
            version: Some(&VALUES[35..44]),
            content_encoding: Some(&VALUES[44..]),
        }
    }

    fn every_failure() -> Vec<CoreFailure<'static>> {
        let mut failures = Vec::new();
        for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
            for id in [None, Some(IDENTIFIER)] {
                failures.push(CoreFailure {
                    status: 503,
                    class: CoreFailureClass::Server,
                    kind,
                    request_id: id,
                });
            }
        }
        failures
    }

    // The bytes of a value that a reading call borrowed.
    fn borrowed(value: MaybeBytes) -> Option<&'static [u8]> {
        // SAFETY: every caller below passes a value that points into a `const`
        // of this module, which outlives the test.
        unsafe { maybe_slice(value) }
    }

    // Every value that the core crate returns has one twin, the twin carries
    // every field of it, and every borrowed field points at the same bytes.
    #[test]
    fn every_read_outcome_crosses_whole() {
        let view = get_outcome(&GetHeadOutcome::Body {
            meta: full_meta(),
            body: CoreBodyWindow {
                object_offset: 2,
                expected_len: Some(4),
                object_size: Some(10),
            },
        });
        assert_eq!(view.disposition, Disposition::Body as u16);
        assert!(view.meta.size.present);
        assert_eq!(view.meta.size.value, 10);
        assert_eq!(view.meta.e_tag.bytes.ptr, e_tag().as_ptr());
        assert_eq!(borrowed(view.meta.e_tag), Some(e_tag()));
        assert!(view.meta.last_modified.present);
        assert!(view.meta.version.present);
        assert!(view.meta.content_encoding.present);
        assert_eq!(view.body.object_offset, 2);
        assert_eq!(view.body.expected_len.value, 4);
        assert_eq!(view.body.object_size.value, 10);

        let empty = get_outcome(&GetHeadOutcome::Body {
            meta: CoreObjectMeta::default(),
            body: CoreBodyWindow {
                object_offset: 0,
                expected_len: None,
                object_size: None,
            },
        });
        assert!(!empty.meta.size.present);
        assert!(!empty.meta.e_tag.present);
        assert_eq!(empty.meta.e_tag.bytes.len, 0);
        assert!(!empty.body.expected_len.present);

        let complete = get_outcome(&GetHeadOutcome::Complete { meta: full_meta() });
        assert_eq!(complete.disposition, Disposition::Complete as u16);
        assert!(complete.meta.e_tag.present);

        for tag in [None, Some(e_tag())] {
            let view = get_outcome(&GetHeadOutcome::NotModified { e_tag: tag });
            assert_eq!(view.disposition, Disposition::NotModified as u16);
            assert_eq!(view.meta.e_tag.present, tag.is_some());
        }

        assert_eq!(
            get_outcome(&GetHeadOutcome::PreconditionFailed).disposition,
            Disposition::PreconditionFailed as u16
        );

        // A missing object carries the error it named, and no status and no
        // category that the head never stated.
        for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
            let view = get_outcome(&GetHeadOutcome::NotFound { kind });
            assert_eq!(view.disposition, Disposition::NotFound as u16);
            assert_eq!(kind_of(view.failure.kind), kind);
            assert_eq!(view.failure.status, 0);
            assert_eq!(view.failure.class, 0);
        }

        for object_size in [None, Some(10)] {
            let view = get_outcome(&GetHeadOutcome::RangeNotSatisfiable { object_size });
            assert_eq!(view.disposition, Disposition::RangeNotSatisfiable as u16);
            assert_eq!(number(view.body.object_size), object_size);
            assert_eq!(
                text(&view),
                GetHeadOutcome::RangeNotSatisfiable { object_size }.to_string()
            );
        }

        for failure in every_failure() {
            for (outcome, expected) in [
                (
                    GetHeadOutcome::NeedErrorBody(failure),
                    Disposition::NeedErrorBody,
                ),
                (
                    GetHeadOutcome::ServiceFailure(failure),
                    Disposition::ServiceFailure,
                ),
            ] {
                let view = get_outcome(&outcome);
                assert_eq!(view.disposition, expected as u16);
                assert_eq!(view.failure.status, failure.status);
                assert_eq!(class_of(view.failure.class), Some(failure.class));
                assert_eq!(kind_of(view.failure.kind), failure.kind);
                assert_eq!(borrowed(view.failure.request_id), failure.request_id);
                assert_eq!(text(&view), outcome.to_string());
            }
        }
    }

    #[test]
    fn every_write_and_removal_outcome_crosses_whole() {
        let created = put_outcome(&PutHeadOutcome::Created { meta: full_meta() });
        assert_eq!(created.disposition, Disposition::Done as u16);
        assert!(created.meta.e_tag.present);

        assert_eq!(
            delete_outcome(&DeleteHeadOutcome::Accepted).disposition,
            Disposition::Accepted as u16
        );

        for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
            assert_eq!(
                kind_of(put_outcome(&PutHeadOutcome::NotFound { kind }).failure.kind),
                kind
            );
            assert_eq!(
                kind_of(
                    delete_outcome(&DeleteHeadOutcome::NotFound { kind })
                        .failure
                        .kind
                ),
                kind
            );
        }

        // A failure says the same thing whichever operation it answers, so the
        // twin needs no field naming the operation.
        for failure in every_failure() {
            for (put, delete) in [
                (
                    PutHeadOutcome::NeedErrorBody(failure),
                    DeleteHeadOutcome::NeedErrorBody(failure),
                ),
                (
                    PutHeadOutcome::ServiceFailure(failure),
                    DeleteHeadOutcome::ServiceFailure(failure),
                ),
            ] {
                assert_eq!(text(&put_outcome(&put)), put.to_string());
                assert_eq!(text(&delete_outcome(&delete)), delete.to_string());
            }
        }
    }

    // The sentence for a failure, a missing object and an unsatisfiable range
    // is the core crate's own. A settled outcome gets a literal, which names no
    // operation because one twin answers all three.
    #[test]
    fn every_disposition_says_something_of_its_own() {
        for kind in [
            ServiceErrorKind::NotFound,
            ServiceErrorKind::NoSuchContainer,
        ] {
            let outcome = GetHeadOutcome::NotFound { kind: Some(kind) };
            assert_eq!(text(&get_outcome(&outcome)), outcome.to_string());
        }
        // A head that named neither leaves both open, and one twin answers for
        // three operations, so the sentence says both.
        assert_eq!(
            text(&get_outcome(&GetHeadOutcome::NotFound { kind: None })),
            "the object or its container does not exist"
        );

        let mut said = Vec::new();
        for disposition in [
            Disposition::Body,
            Disposition::Complete,
            Disposition::NotModified,
            Disposition::PreconditionFailed,
            Disposition::Done,
            Disposition::Accepted,
        ] {
            let sentence = settled_sentence(Some(disposition));
            assert!(!sentence.is_empty());
            assert_eq!(text(&empty_outcome(disposition)), sentence);
            said.push(sentence);
        }
        said.sort_unstable();
        said.dedup();
        assert_eq!(said.len(), 6);

        // A disposition from a later version of this crate names nothing here.
        let mut later = empty_outcome(Disposition::Body);
        later.disposition = 4095;
        assert_eq!(text(&later), settled_sentence(None));
    }

    // Every enum crosses as its number, and comes back the same value. A number
    // that names nothing is refused, never read as another value.
    #[test]
    fn every_enum_crosses_by_its_number_and_refuses_the_rest() {
        for repr in 1..=u16::MAX {
            if let Some(kind) = ServiceErrorKind::from_discriminant(repr) {
                assert_eq!(kind_of(kind_view(Some(kind))), Some(kind), "{kind:?}");
                assert_eq!(kind_view(Some(kind)), repr);
            }
            if let Some(class) = CoreFailureClass::from_discriminant(repr) {
                assert_eq!(class_of(class as u16), Some(class), "{class:?}");
            }
            assert_eq!(
                disposition_of(repr).map(|disposition| disposition as u16),
                disposition_of(repr).map(|_| repr)
            );
        }
        assert_eq!(kind_of(kind_view(None)), None);
        assert_eq!(kind_of(4095), None);
        assert_eq!(class_of(4095), None);
        assert!(disposition_of(0).is_none());
        assert!(disposition_of(13).is_none());

        // The plan side, which crosses inwards and must refuse.
        for (kind, expected) in [
            (GetKind::Bytes as u16, Some(proto::GetKind::Bytes)),
            (GetKind::Metadata as u16, Some(proto::GetKind::Metadata)),
            (0, None),
            (4095, None),
        ] {
            let shape = GetShape {
                kind,
                ..read_shape()
            };
            assert_eq!(get_shape(&shape).map(|shape| shape.kind).ok(), expected);
        }
        for (form, expected) in [
            (RangeForm::Whole as u16, Some(RequestedRange::Whole)),
            (
                RangeForm::Bounded as u16,
                Some(RequestedRange::Bounded { start: 2, end: 6 }),
            ),
            (RangeForm::Offset as u16, Some(RequestedRange::Offset(2))),
            (RangeForm::Suffix as u16, Some(RequestedRange::Suffix(2))),
            (0, None),
        ] {
            let shape = GetShape {
                range: Range {
                    form,
                    start: 2,
                    end: 6,
                },
                ..read_shape()
            };
            assert_eq!(get_shape(&shape).map(|shape| shape.range).ok(), expected);
        }
        for (condition, expected) in [
            (Condition::None as u16, Some(proto::ConditionKind::None)),
            (
                Condition::IfMatch as u16,
                Some(proto::ConditionKind::IfMatch),
            ),
            (
                Condition::IfNoneMatch as u16,
                Some(proto::ConditionKind::IfNoneMatch),
            ),
            (0, None),
        ] {
            assert_eq!(condition_kind(condition).ok(), expected);
            assert_eq!(
                put_shape(&PutShape { condition })
                    .map(|shape| shape.condition)
                    .ok(),
                expected
            );
        }
        for (kind, expected) in [
            (DeleteKind::Object as u16, Some(proto::DeleteKind::Object)),
            (
                DeleteKind::ObjectAndSnapshots as u16,
                Some(proto::DeleteKind::ObjectAndSnapshots),
            ),
            (
                DeleteKind::SnapshotsOnly as u16,
                Some(proto::DeleteKind::SnapshotsOnly),
            ),
            (0, None),
        ] {
            let shape = DeleteShape {
                kind,
                condition: Condition::None as u16,
            };
            assert_eq!(delete_shape(&shape).map(|shape| shape.kind).ok(), expected);
        }
    }

    // A number that this crate does not define stops the call, and says so.
    #[test]
    fn an_unknown_number_is_refused_rather_than_read_as_another_value() {
        let session = session();
        let shape = GetShape {
            kind: 4095,
            ..read_shape()
        };
        let mut buf = vec![0; 512];
        // SAFETY: every pointer below addresses a live value of this test.
        let refused = unsafe {
            borink_encode_get(
                &session,
                &shape,
                lent(b"object.bin"),
                lent(b""),
                writable(&mut buf),
                1_787_400_000,
            )
        };
        assert_eq!(refused.status, unknown());
        assert_eq!(refused.status.code, ErrorCode::InvalidPlan as u16);
        assert_eq!(refused.status.detail, InvalidPlan::Unknown as u16);
        assert_eq!(refused.required, 0);

        // SAFETY: as above, with no headers.
        let outcome =
            unsafe { borink_accept_get_head(&session, &shape, 200, core::ptr::null(), 0) };
        assert_eq!(outcome.disposition, Disposition::Invalid as u16);
        assert_eq!(outcome.error, unknown());
        assert_eq!(
            text(&outcome),
            Error::InvalidPlan(InvalidPlan::Unknown).to_string()
        );
    }

    // A pointer that was never filled in is refused as an invalid plan, and
    // never read.
    #[test]
    fn a_null_pointer_is_refused_rather_than_read() {
        let session = session();
        let shape = read_shape();
        let mut buf = vec![0; 512];

        // SAFETY: the null pointers are the case under test, and the rest
        // address live values.
        unsafe {
            assert_eq!(borink_validate(core::ptr::null()), unknown());
            assert_eq!(
                borink_encode_get(
                    core::ptr::null(),
                    &shape,
                    lent(b"object.bin"),
                    lent(b""),
                    writable(&mut buf),
                    0,
                )
                .status,
                unknown()
            );
            assert_eq!(
                borink_encode_get(
                    &session,
                    core::ptr::null(),
                    lent(b"object.bin"),
                    lent(b""),
                    writable(&mut buf),
                    0,
                )
                .status,
                unknown()
            );
            assert_eq!(
                borink_encode_put(
                    &session,
                    core::ptr::null(),
                    lent(b"object.bin"),
                    lent(b""),
                    writable(&mut buf),
                    0,
                    0,
                )
                .status,
                unknown()
            );
            assert_eq!(
                borink_encode_delete(
                    &session,
                    core::ptr::null(),
                    lent(b"object.bin"),
                    lent(b""),
                    writable(&mut buf),
                    0,
                )
                .status,
                unknown()
            );
            assert_eq!(
                borink_accept_get_head(&session, core::ptr::null(), 200, core::ptr::null(), 0)
                    .error,
                unknown()
            );
            assert_eq!(
                borink_accept_put_head(&session, core::ptr::null(), 201, core::ptr::null(), 0)
                    .error,
                unknown()
            );
            assert_eq!(
                borink_accept_delete_head(&session, core::ptr::null(), 202, core::ptr::null(), 0)
                    .error,
                unknown()
            );
            assert_eq!(
                borink_finish_get_error_body(&session, core::ptr::null(), lent(b"")).error,
                unknown()
            );
            assert_eq!(
                borink_finish_put_error_body(&session, core::ptr::null(), lent(b"")).error,
                unknown()
            );
            assert_eq!(
                borink_finish_delete_error_body(&session, core::ptr::null(), lent(b"")).error,
                unknown()
            );
            // A sentence for nothing is no sentence, not a guess at one.
            assert_eq!(borink_describe(core::ptr::null(), writable(&mut buf)), 0);
        }
    }

    // Every error of the core crate crosses as two numbers and comes back as
    // the same sentence.
    #[test]
    fn every_error_crosses_as_a_status() {
        let mut checked = 0;
        for code in 1..=u16::MAX {
            let Some(code) = proto::ErrorCode::from_discriminant(code) else {
                continue;
            };
            for detail in 0..=u16::MAX {
                let Some(error) = Error::from_parts(code, detail) else {
                    continue;
                };
                let status = status_of(&error);
                assert_eq!(status.code, code as u16);
                assert_eq!(status.detail, detail);
                let mut into = [0; 256];
                // SAFETY: `into` is live and reached through nothing else.
                let length = unsafe { borink_describe_status(status, writable(&mut into)) };
                assert_eq!(
                    String::from_utf8(into[..length].to_vec()).unwrap(),
                    error.to_string(),
                    "{error:?}"
                );
                checked += 1;
            }
        }
        // Every variant of the two inner enums, and the three that carry no
        // inner value.
        assert_eq!(checked, 3 + 7 + 3);
        assert_eq!(
            ResponseFault::from_discriminant(3).map(Error::Response),
            Error::from_parts(proto::ErrorCode::Response, 3)
        );
    }

    // A capacity error carries sizes rather than a discriminant, so it crosses
    // as a code and the `required` field of the request head.
    #[test]
    fn a_buffer_that_is_too_small_reports_the_size_it_needs() {
        let session = session();
        let shape = read_shape();
        // SAFETY: the empty buffer is the case under test; the rest are live.
        let refused = unsafe {
            borink_encode_get(
                &session,
                &shape,
                lent(b"object.bin"),
                lent(b""),
                writable(&mut []),
                1_787_400_000,
            )
        };
        assert_eq!(refused.status.code, ErrorCode::Capacity as u16);
        assert!(refused.required > 0);

        let mut buf = vec![0; refused.required];
        // SAFETY: every pointer addresses a live value of this test.
        let written = unsafe {
            borink_encode_get(
                &session,
                &shape,
                lent(b"object.bin"),
                lent(b""),
                writable(&mut buf),
                1_787_400_000,
            )
        };
        assert_eq!(written.status.code, 0);
        assert_eq!(written.required, refused.required);
        assert_eq!(written.method, Method::Get as u16);
        assert_eq!(written.header_count, 3);
        let url = &buf[written.url.start..written.url.start + written.url.len];
        assert_eq!(
            core::str::from_utf8(url).unwrap(),
            "https://account.blob.core.windows.net/container/object.bin"
        );
        for index in 0..written.header_count {
            let header = written.headers[index];
            assert!(header.name.start + header.name.len <= buf.len());
            assert!(header.value.start + header.value.len <= buf.len());
        }
    }

    // A ranged, conditional read reaches the core crate from a stored shape and
    // the bytes that go with it.
    #[test]
    fn a_stored_shape_carries_the_whole_plan() {
        let session = session();
        let shape = GetShape {
            kind: GetKind::Bytes as u16,
            range: Range {
                form: RangeForm::Bounded as u16,
                start: 2,
                end: 6,
            },
            condition: Condition::IfNoneMatch as u16,
        };
        let mut buf = vec![0; 512];
        // SAFETY: every pointer addresses a live value of this test.
        let head = unsafe {
            borink_encode_get(
                &session,
                &shape,
                lent(b"object.bin"),
                lent(b"\"etag\""),
                writable(&mut buf),
                1_787_400_000,
            )
        };
        assert_eq!(head.status.code, 0);
        let named = |name: &str| {
            (0..head.header_count).find_map(|index| {
                let header = head.headers[index];
                let read = |span: Span| {
                    core::str::from_utf8(&buf[span.start..span.start + span.len]).unwrap()
                };
                (read(header.name) == name).then(|| read(header.value).to_string())
            })
        };
        assert_eq!(named("range").as_deref(), Some("bytes=2-5"));
        assert_eq!(named("if-none-match").as_deref(), Some("\"etag\""));
    }

    // The head reaches this crate as slices, from wherever the host keeps them.
    // Nothing here is one buffer, and the outcome points back at each.
    #[test]
    fn a_head_crosses_as_slices_of_whatever_holds_it() {
        let session = session();
        let headers = [
            header("ETag", e_tag()),
            header("Content-Length", b"10"),
            header("x-ms-request-id", IDENTIFIER),
            // A name that is not text is none of the ones the core crate reads,
            // so it is skipped rather than refused.
            HeaderRef {
                name: lent(b"\xff"),
                value: lent(b"value"),
            },
        ];
        // SAFETY: every pointer addresses a live value of this test.
        let outcome = unsafe {
            borink_accept_get_head(
                &session,
                &read_shape(),
                200,
                headers.as_ptr(),
                headers.len(),
            )
        };
        assert_eq!(outcome.disposition, Disposition::Body as u16);
        assert_eq!(outcome.meta.e_tag.bytes.ptr, e_tag().as_ptr());
        assert!(outcome.body.expected_len.present);
        assert_eq!(outcome.body.expected_len.value, 10);
    }

    // The head asked for the error body, and the body names the error. The
    // request id crosses as bytes the host still owns, both ways.
    #[test]
    fn the_error_body_finishes_what_the_head_left_open() {
        let session = session();
        let headers = [header("x-ms-request-id", IDENTIFIER)];
        // SAFETY: every pointer addresses a live value of this test.
        let outcome = unsafe {
            borink_accept_put_head(
                &session,
                &write_shape(),
                409,
                headers.as_ptr(),
                headers.len(),
            )
        };
        assert_eq!(outcome.disposition, Disposition::NeedErrorBody as u16);
        assert_eq!(outcome.failure.request_id.bytes.ptr, IDENTIFIER.as_ptr());

        // SAFETY: as above, and the body outlives the outcome it names.
        let finished = unsafe {
            borink_finish_put_error_body(
                &session,
                &outcome.failure,
                lent(b"<Error><Code>BlobAlreadyExists</Code></Error>"),
            )
        };
        assert_eq!(finished.disposition, Disposition::ServiceFailure as u16);
        assert_eq!(
            kind_of(finished.failure.kind),
            Some(ServiceErrorKind::AlreadyExists)
        );
        assert!(text(&finished).contains("already exists"));
        assert!(text(&finished).contains("request-123"));

        // A body that never arrived leaves the outcome final and unnamed.
        // SAFETY: as above.
        let unnamed =
            unsafe { borink_finish_put_error_body(&session, &outcome.failure, lent(b"")) };
        assert_eq!(unnamed.disposition, Disposition::ServiceFailure as u16);
        assert_eq!(kind_of(unnamed.failure.kind), None);
    }

    // A head that does not answer the plan is a status, not a sentence.
    #[test]
    fn an_invalid_head_carries_the_error_of_the_core_crate() {
        let session = session();
        // SAFETY: every pointer addresses a live value of this test.
        let outcome =
            unsafe { borink_accept_put_head(&session, &write_shape(), 412, core::ptr::null(), 0) };
        assert_eq!(outcome.disposition, Disposition::Invalid as u16);
        assert_eq!(outcome.error.code, ErrorCode::Response as u16);
        assert_eq!(outcome.error.detail, ResponseFault::Status as u16);
        assert_eq!(
            text(&outcome),
            Error::Response(ResponseFault::Status).to_string()
        );
    }

    #[test]
    fn a_session_that_cannot_be_used_says_which_value_is_wrong() {
        for (endpoint, container, token, expected) in [
            (
                b"account.example".as_slice(),
                b"container".as_slice(),
                b"token".as_slice(),
                ErrorCode::InvalidEndpoint,
            ),
            (
                b"https://account.example",
                b"",
                b"token",
                ErrorCode::InvalidContainer,
            ),
            (
                b"https://account.example",
                b"container",
                b"",
                ErrorCode::InvalidToken,
            ),
            (b"\xff", b"container", b"token", ErrorCode::InvalidEndpoint),
        ] {
            let session = opened(endpoint, container, token);
            // SAFETY: every pointer addresses a live value of this test.
            let status = unsafe { borink_validate(&session) };
            assert_eq!(status.code, expected as u16);
            // A session that cannot build a request cannot read the answer to
            // one, and says the same thing when asked to.
            // SAFETY: as above.
            let refused = unsafe {
                borink_encode_get(
                    &session,
                    &read_shape(),
                    lent(b"key"),
                    lent(b""),
                    writable(&mut []),
                    0,
                )
            };
            assert_eq!(refused.status, status);
            // SAFETY: as above.
            let outcome = unsafe {
                borink_accept_get_head(&session, &read_shape(), 200, core::ptr::null(), 0)
            };
            assert_eq!(outcome.disposition, Disposition::Invalid as u16);
            assert_eq!(outcome.error, status);
        }
        // SAFETY: the session addresses `const` bytes of this module.
        assert_eq!(unsafe { borink_validate(&session()) }.code, 0);
    }

    // A sentence longer than the buffer is counted, not cut off silently.
    #[test]
    fn a_short_buffer_still_learns_the_length_of_the_sentence() {
        let outcome = get_outcome(&GetHeadOutcome::ServiceFailure(CoreFailure {
            status: 503,
            class: CoreFailureClass::Server,
            kind: None,
            request_id: Some(IDENTIFIER),
        }));
        let mut small = [0; 4];
        // SAFETY: both values are live and reached through nothing else.
        let length = unsafe { borink_describe(&outcome, writable(&mut small)) };
        assert!(length > small.len());
        let mut whole = vec![0; length];
        // SAFETY: as above.
        assert_eq!(
            unsafe { borink_describe(&outcome, writable(&mut whole)) },
            length
        );
    }

    // The layout check reports what it is given, so a C program that disagrees
    // learns how many facts disagree rather than reading the wrong offset.
    #[test]
    fn the_layout_check_answers_for_the_layout_it_is_given() {
        let ours = layout();
        // SAFETY: `ours` is live, and null is the case the second call tests.
        unsafe {
            assert_eq!(borink_layout_disagrees(&ours), 0);
            let mut wrong = ours;
            wrong.sizeof_outcome += 1;
            wrong.offsetof_outcome_error += 1;
            assert_eq!(borink_layout_disagrees(&wrong), 2);
            assert_eq!(
                borink_layout_disagrees(core::ptr::null()),
                size_of::<Layout>() / size_of::<usize>()
            );
        }
    }
}
