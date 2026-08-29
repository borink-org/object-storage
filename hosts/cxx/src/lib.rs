//! The bridge that lets a C++ application drive `borink-object-storage`.
//!
//! The bridge plans a request, writes the request head into a buffer that C++
//! owns, and reads the response head that C++ collected. It opens no socket,
//! reads no clock, and keeps nothing between calls.
//!
//! No call returns `Result`, so no call throws. Each call reports what
//! happened in a field of the value that it returns.
//!
//! # What it costs
//!
//! `open_session` allocates four times: once each for the endpoint, the
//! container and the token, and once for the session itself. No other call
//! allocates.
//!
//! This crate needs the Rust standard library, so it does not build for a
//! freestanding target. The core crate is `no_std`; a binding for such a
//! target would be a separate glue crate over it.
//!
//! # Offsets out, slices in
//!
//! `encode_get` writes the head into your buffer and returns a `RequestHead`,
//! which names the URL and each header by offset and length. You resize and
//! reuse that buffer, so an offset keeps its meaning where a pointer would
//! not. Call `encode_get` with an empty buffer to learn the size: it reports
//! `ErrorCode::Capacity` and the exact number of bytes.
//!
//! A response head crosses the other way. You name each header with a
//! `HeaderRef` that points at bytes you already hold, and the `Outcome` points
//! at the same bytes. Nothing is copied, and no layout is required of you.
//!
//! Each borrowed field states under its own `# Lifetime` how long it is valid.
//! Those sentences are the contract.
//!
//! The six reading calls are `unsafe fn` below because `cxx` refuses an
//! `extern "Rust"` signature that names a lifetime unless it is. Their bodies
//! are safe, and this crate contains no `unsafe` block.
//!
//! # How a value crosses
//!
//! A failure crosses as a `Status`: the error code the core crate defines, and
//! the discriminant of the value inside it. `describe_status` writes the
//! sentence for one. A response that the service sends in normal operation is
//! not a failure. It is a `Disposition` on the `Outcome`.
//!
//! Every other enum crosses as the number the core crate gives it, in both
//! directions. A number that this bridge does not define is refused as
//! `ErrorCode::InvalidPlan`.
//!
//! # Examples
//!
//! ```cpp
//! rust::Box<borink::Session> session = borink::open_session(endpoint, container, token);
//! if (session->status().code != 0) { /* ... */ }
//!
//! borink::GetShapeView shape = read.shape();
//! borink::RequestHead head = session->encode_get(shape, key, {}, {buffer.data(), buffer.size()}, now);
//! if (head.status.code == static_cast<std::uint16_t>(borink::ErrorCode::Capacity)) {
//!     buffer.resize(head.required);
//!     head = session->encode_get(shape, key, {}, {buffer.data(), buffer.size()}, now);
//! }
//! // ... send head.url and head.headers with your HTTP client ...
//!
//! borink::Outcome outcome = session->accept_get_head(shape, status, header_refs);
//! if (outcome.disposition == borink::Disposition::Body) {
//!     // ... read the body ...
//! }
//! ```

use std::fmt::{self, Write as _};

use borink_object_storage::{
    Blobs, BodyWindow, ConditionKind, Container, DeleteHeadOutcome, DeleteKind, DeleteShape, Error,
    ErrorCode, Failure, FailureClass, GetHeadOutcome, GetKind, GetShape, InvalidPlan, ObjectMeta,
    Payload, PhysicalDelete, PhysicalGet, PhysicalPut, PutHeadOutcome, PutShape, RangeForm,
    RequestedRange, ResponseHead, ServiceErrorKind, Timestamps, WireRequest,
};

use ffi::{
    BodyWindowView, ConditionView, DeleteKindView, DeleteShapeView, Disposition, FailureClassView,
    FailureView, GetKindView, GetShapeView, HeaderRef, MaybeBytes, MaybeU64, Method,
    ObjectMetaView, Outcome, PutShapeView, RangeFormView, RequestHead, RequestHeader,
    ServiceErrorKindView, Span, Status,
};

/// The most headers that one request head carries.
///
/// This is the core crate's own bound. The twin's array has exactly this many
/// slots, so a header added to the core crate is a compile error here rather
/// than a header that this bridge drops.
const MAX_HEADERS: usize = borink_object_storage::MAX_HEADERS;

/// One container, and the token that opens it.
///
/// Build one per client. It holds the only memory that this bridge owns.
pub struct Session {
    endpoint: String,
    container: String,
    token: String,
    status: Status,
}

#[cxx::bridge(namespace = "borink")]
mod ffi {
    /// A range of bytes, as an offset from the start of your request buffer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Span {
        /// The offset of the first byte.
        start: usize,
        /// The number of bytes.
        len: usize,
    }

    /// Bytes that a response head may not carry.
    ///
    /// `present` and an empty `bytes` are different facts: a header that the
    /// service sent empty is present, and one it did not send is not.
    ///
    /// # Lifetime
    ///
    /// `bytes` points into the storage that the `HeaderRef`s of the call
    /// pointed into, or into the error body that you passed. It is valid until
    /// you release or reuse that storage.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct MaybeBytes<'h> {
        /// Whether the head carried this value.
        present: bool,
        /// The bytes of it.
        bytes: &'h [u8],
    }

    /// A number that a response head may not carry.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct MaybeU64 {
        /// Whether the head carried this number.
        present: bool,
        /// The number.
        value: u64,
    }

    /// One response header, as the bytes that you already hold.
    ///
    /// Build a small array of these from wherever your HTTP library keeps the
    /// head, and reuse the array. This bridge copies none of it.
    ///
    /// # Lifetime
    ///
    /// Both slices must stay valid for as long as you use the `Outcome` that
    /// the reading call returns.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct HeaderRef<'h> {
        /// The header name. A name that is not text is ignored.
        name: &'h [u8],
        /// The header value.
        value: &'h [u8],
    }

    /// A failure, as the two numbers that describe every error of the core
    /// crate.
    ///
    /// `code` is a `ErrorCode`, and `detail` is the discriminant of the value
    /// inside it. A `code` of 0 means that nothing failed. Both numbers are
    /// append-only: a value defined today keeps its meaning.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Status {
        /// The kind of failure, or 0 if there is none.
        code: u16,
        /// The discriminant of the value inside, or 0 if there is none.
        detail: u16,
    }

    /// Which kind of failure a `Status` carries.
    ///
    /// These are the numbers that the core crate's `ErrorCode` uses.
    #[derive(Debug)]
    #[repr(u16)]
    enum ErrorCode {
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
        /// The response head is invalid, or it contradicts itself.
        Protocol = 6,
        /// The response head does not answer the plan.
        ResponseMismatch = 7,
    }

    /// The HTTP method of a request.
    #[derive(Debug)]
    #[repr(u8)]
    enum Method {
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
    #[derive(Debug)]
    #[repr(u16)]
    enum GetKindView {
        /// The bytes of the object.
        Bytes = 1,
        /// The metadata of the object, without its bytes.
        Metadata = 2,
    }

    /// Which form of byte range a read requests.
    #[derive(Debug)]
    #[repr(u16)]
    enum RangeFormView {
        /// Every byte of the object. `start` and `end` are 0.
        Whole = 1,
        /// The half-open interval `start..end`.
        Bounded = 2,
        /// Every byte from `start` to the end of the object.
        Offset = 3,
        /// The last `start` bytes. Azure Blob Storage refuses this form.
        Suffix = 4,
    }

    /// The byte range that a read requests.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RangeView {
        /// Which form of range this is.
        form: RangeFormView,
        /// The first byte, or the length of a suffix.
        start: u64,
        /// The byte after the last byte, for a bounded range.
        end: u64,
    }

    /// The ETag precondition that a request carries.
    ///
    /// A request that carries one passes the entity tag as
    /// `condition_value`. A request that carries none passes an empty
    /// `condition_value`.
    #[derive(Debug)]
    #[repr(u16)]
    enum ConditionView {
        /// The request carries no precondition.
        None = 1,
        /// The request succeeds only if the current ETag matches.
        IfMatch = 2,
        /// The request succeeds only if the current ETag differs.
        IfNoneMatch = 3,
    }

    /// What a removal takes with it.
    #[derive(Debug)]
    #[repr(u16)]
    enum DeleteKindView {
        /// Remove the object alone. Azure refuses this if it has snapshots.
        Object = 1,
        /// Remove the object and its snapshots.
        ObjectAndSnapshots = 2,
        /// Remove the snapshots and keep the object.
        SnapshotsOnly = 3,
    }

    /// The part of a read plan that holds no borrows.
    ///
    /// Store one of these while the request is in flight, and pass it to
    /// `accept_get_head` when the response arrives. It is the whole
    /// per-request context: this bridge keeps none of its own.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct GetShapeView {
        /// Whether the read asks for bytes or for metadata.
        kind: GetKindView,
        /// The byte range that the read requests.
        range: RangeView,
        /// The precondition that the read carries.
        condition: ConditionView,
    }

    /// The part of a write plan that holds no borrows.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct PutShapeView {
        /// The precondition that the write carries.
        condition: ConditionView,
    }

    /// The part of a removal plan that holds no borrows.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct DeleteShapeView {
        /// What the removal takes with it.
        kind: DeleteKindView,
        /// The precondition that the removal carries.
        condition: ConditionView,
    }

    /// One request header, as two ranges of the request buffer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RequestHeader {
        /// The range that holds the header name.
        name: Span,
        /// The range that holds the header value.
        value: Span,
    }

    /// A request head, as ranges of the buffer that holds it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RequestHead {
        /// Whether the head was written, and what stopped it.
        ///
        /// A `code` of 0 means that the head is in your buffer.
        status: Status,
        /// The number of bytes that this request head needs.
        ///
        /// This is the exact size whenever the plan is valid, whether or not
        /// the head was written. Size one buffer by it and reuse that buffer.
        required: usize,
        /// The HTTP method.
        method: Method,
        /// The range that holds the complete object URL.
        url: Span,
        /// How many of `headers` this request uses.
        header_count: usize,
        /// The headers, in the order that the core crate wrote them.
        headers: [RequestHeader; 6],
    }

    /// Object metadata, borrowed from the response head.
    ///
    /// # Lifetime
    ///
    /// Every field points into the storage that the `HeaderRef`s pointed
    /// into, and is valid until you release or reuse it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct ObjectMetaView<'h> {
        /// The size of the whole object.
        size: MaybeU64,
        /// The entity tag.
        e_tag: MaybeBytes<'h>,
        /// The value of the `Last-Modified` header.
        last_modified: MaybeBytes<'h>,
        /// The version identifier.
        version: MaybeBytes<'h>,
        /// The value of the `Content-Encoding` header.
        content_encoding: MaybeBytes<'h>,
    }

    /// Where the bytes of the response body belong in the object.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct BodyWindowView {
        /// The offset in the object of the first byte of the response body.
        object_offset: u64,
        /// The exact length of the response body.
        expected_len: MaybeU64,
        /// The size of the whole object.
        object_size: MaybeU64,
    }

    /// The category of a service failure.
    ///
    /// These are the numbers that the core crate's `FailureClass` uses. A
    /// number that is not listed here comes from a later version of that
    /// crate. It crosses unchanged, never as a substitute.
    #[derive(Debug)]
    #[repr(u16)]
    enum FailureClassView {
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
    /// These are the numbers that the core crate's `ServiceErrorKind` uses,
    /// and they cross unchanged in both directions.
    #[derive(Debug)]
    #[repr(u16)]
    enum ServiceErrorKindView {
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

    /// A response head that reports a failure.
    ///
    /// Store one of these and pass it back to `finish_get_error_body` to
    /// finish a `Disposition::NeedErrorBody`.
    ///
    /// A `Disposition::NotFound` fills `kind` alone. A missing object is not a
    /// failure of the head: it names an error and carries no status and no
    /// category, so both are 0.
    ///
    /// # Lifetime
    ///
    /// `request_id` points into the storage that the `HeaderRef`s pointed
    /// into. Copy it if you keep this value past that storage.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FailureView<'h> {
        /// The HTTP status code.
        status: u16,
        /// The category of the failure. Use it to decide whether to retry.
        ///
        /// This is `class` in the core crate, which C++ cannot spell.
        category: FailureClassView,
        /// The specific error, if the head or the body named one.
        kind: ServiceErrorKindView,
        /// The value of the `x-ms-request-id` header.
        request_id: MaybeBytes<'h>,
    }

    /// What a response tells you to do.
    #[derive(Debug)]
    #[repr(u16)]
    enum Disposition {
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
        /// Read the response body, cap what you read, and pass it with
        /// `failure` to `finish_get_error_body` or one of its two siblings.
        NeedErrorBody = 9,
        /// Azure refused the request, or it failed to carry it out.
        ServiceFailure = 10,
        /// The call was refused, or the head does not answer the plan. Read
        /// `error`.
        Invalid = 11,
        /// The core crate returned a variant that this bridge does not know.
        Unsupported = 12,
    }

    /// The result of reading one response head.
    ///
    /// One value describes a read, a write and a removal. The fields that the
    /// operation does not fill are absent.
    ///
    /// # Lifetime
    ///
    /// Everything that this value borrows is valid until you release or reuse
    /// the storage that the `HeaderRef`s pointed into.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Outcome<'h> {
        /// What to do with the response.
        disposition: Disposition,
        /// The metadata from the head.
        meta: ObjectMetaView<'h>,
        /// Where the bytes of the body belong.
        body: BodyWindowView,
        /// The failure, for `NeedErrorBody`, `ServiceFailure` and `NotFound`.
        failure: FailureView<'h>,
        /// Why the call was refused, for `Invalid`.
        error: Status,
    }

    extern "Rust" {
        type Session;

        /// Opens a session against one container.
        ///
        /// This is the one call that allocates. It copies the three values, so
        /// none of them has to outlive the call. A value that is not usable
        /// leaves the session with a `status`, and every request refuses with
        /// that same status.
        fn open_session(endpoint: &[u8], container: &[u8], token: &[u8]) -> Box<Session>;

        /// Returns what is wrong with this session, if anything.
        ///
        /// A `code` of 0 means that the session can build requests.
        fn status(self: &Session) -> Status;

        /// Writes the request head of a read into `buf`.
        ///
        /// Pass an empty `condition_value` if `shape` carries no condition.
        /// Pass an empty buffer to learn the size that this request needs.
        fn encode_get(
            self: &Session,
            shape: &GetShapeView,
            key: &[u8],
            condition_value: &[u8],
            buf: &mut [u8],
            unix_seconds: u64,
        ) -> RequestHead;

        /// Writes the request head of a write into `buf`.
        ///
        /// The head states `content_len`. You send those bytes yourself.
        fn encode_put(
            self: &Session,
            shape: &PutShapeView,
            key: &[u8],
            condition_value: &[u8],
            buf: &mut [u8],
            content_len: u64,
            unix_seconds: u64,
        ) -> RequestHead;

        /// Writes the request head of a removal into `buf`.
        fn encode_delete(
            self: &Session,
            shape: &DeleteShapeView,
            key: &[u8],
            condition_value: &[u8],
            buf: &mut [u8],
            unix_seconds: u64,
        ) -> RequestHead;

        /// Reads the response head of a read.
        ///
        /// Pass the same `shape` that you passed to `encode_get`, and one
        /// `HeaderRef` per response header. The outcome points into the same
        /// bytes as those `HeaderRef`s.
        ///
        /// # Lifetime
        ///
        /// The bytes that `headers` points at must stay valid, and must not
        /// move, for as long as you use the returned `Outcome`.
        unsafe fn accept_get_head<'h>(
            self: &Session,
            shape: &GetShapeView,
            status: u16,
            headers: &[HeaderRef<'h>],
        ) -> Outcome<'h>;

        /// Reads the response head of a write.
        ///
        /// # Lifetime
        ///
        /// As `accept_get_head`.
        unsafe fn accept_put_head<'h>(
            self: &Session,
            shape: &PutShapeView,
            status: u16,
            headers: &[HeaderRef<'h>],
        ) -> Outcome<'h>;

        /// Reads the response head of a removal.
        ///
        /// # Lifetime
        ///
        /// As `accept_get_head`.
        unsafe fn accept_delete_head<'h>(
            self: &Session,
            shape: &DeleteShapeView,
            status: u16,
            headers: &[HeaderRef<'h>],
        ) -> Outcome<'h>;

        /// Finishes a read whose head asked for the error body.
        ///
        /// Pass the `failure` of that outcome and the body that you read. Pass
        /// an empty body if you read none: the outcome is then final with the
        /// error unnamed.
        ///
        /// # Lifetime
        ///
        /// `failure.request_id` must still point at valid bytes, and they must
        /// stay valid for as long as you use the returned `Outcome`.
        unsafe fn finish_get_error_body<'h>(
            self: &Session,
            failure: &FailureView<'h>,
            body: &[u8],
        ) -> Outcome<'h>;

        /// Finishes a write whose head asked for the error body.
        ///
        /// # Lifetime
        ///
        /// As `finish_get_error_body`.
        unsafe fn finish_put_error_body<'h>(
            self: &Session,
            failure: &FailureView<'h>,
            body: &[u8],
        ) -> Outcome<'h>;

        /// Finishes a removal whose head asked for the error body.
        ///
        /// # Lifetime
        ///
        /// As `finish_get_error_body`.
        unsafe fn finish_delete_error_body<'h>(
            self: &Session,
            failure: &FailureView<'h>,
            body: &[u8],
        ) -> Outcome<'h>;

        /// Writes one sentence naming what `outcome` says.
        ///
        /// Returns the length of the whole sentence, which may be longer than
        /// `into`: the part that fits is written, and nothing is allocated.
        fn describe(outcome: &Outcome, into: &mut [u8]) -> usize;

        /// Writes one sentence naming what `status` says.
        ///
        /// Returns the length of the whole sentence, exactly as `describe`.
        fn describe_status(status: Status, into: &mut [u8]) -> usize;
    }
}

// Every enum below crosses as a number, and this is where the two lists are
// pinned to each other. A value renumbered on either side stops this build,
// which is what makes each conversion a cast rather than a table.
const _: () = {
    use borink_object_storage as core;

    assert!(MAX_HEADERS == 6);

    assert!(ffi::ErrorCode::InvalidEndpoint.repr == ErrorCode::InvalidEndpoint as u16);
    assert!(ffi::ErrorCode::InvalidContainer.repr == ErrorCode::InvalidContainer as u16);
    assert!(ffi::ErrorCode::InvalidToken.repr == ErrorCode::InvalidToken as u16);
    assert!(ffi::ErrorCode::InvalidPlan.repr == ErrorCode::InvalidPlan as u16);
    assert!(ffi::ErrorCode::Capacity.repr == ErrorCode::Capacity as u16);
    assert!(ffi::ErrorCode::Protocol.repr == ErrorCode::Protocol as u16);
    assert!(ffi::ErrorCode::ResponseMismatch.repr == ErrorCode::ResponseMismatch as u16);

    assert!(Method::Get.repr == core::Method::Get as u8);
    assert!(Method::Head.repr == core::Method::Head as u8);
    assert!(Method::Put.repr == core::Method::Put as u8);
    assert!(Method::Delete.repr == core::Method::Delete as u8);

    assert!(GetKindView::Bytes.repr == GetKind::Bytes as u16);
    assert!(GetKindView::Metadata.repr == GetKind::Metadata as u16);

    assert!(RangeFormView::Whole.repr == RangeForm::Whole as u16);
    assert!(RangeFormView::Bounded.repr == RangeForm::Bounded as u16);
    assert!(RangeFormView::Offset.repr == RangeForm::Offset as u16);
    assert!(RangeFormView::Suffix.repr == RangeForm::Suffix as u16);

    assert!(ConditionView::None.repr == ConditionKind::None as u16);
    assert!(ConditionView::IfMatch.repr == ConditionKind::IfMatch as u16);
    assert!(ConditionView::IfNoneMatch.repr == ConditionKind::IfNoneMatch as u16);

    assert!(DeleteKindView::Object.repr == DeleteKind::Object as u16);
    assert!(DeleteKindView::ObjectAndSnapshots.repr == DeleteKind::ObjectAndSnapshots as u16);
    assert!(DeleteKindView::SnapshotsOnly.repr == DeleteKind::SnapshotsOnly as u16);

    assert!(FailureClassView::Auth.repr == FailureClass::Auth as u16);
    assert!(FailureClassView::Throttled.repr == FailureClass::Throttled as u16);
    assert!(FailureClassView::Server.repr == FailureClass::Server as u16);
    assert!(FailureClassView::Redirect.repr == FailureClass::Redirect as u16);
    assert!(FailureClassView::Other.repr == FailureClass::Other as u16);

    assert!(ServiceErrorKindView::NotFound.repr == ServiceErrorKind::NotFound as u16);
    assert!(ServiceErrorKindView::NoSuchContainer.repr == ServiceErrorKind::NoSuchContainer as u16);
    assert!(ServiceErrorKindView::AlreadyExists.repr == ServiceErrorKind::AlreadyExists as u16);
    assert!(ServiceErrorKindView::Unauthorized.repr == ServiceErrorKind::Unauthorized as u16);
    assert!(ServiceErrorKindView::Precondition.repr == ServiceErrorKind::Precondition as u16);
    assert!(
        ServiceErrorKindView::RangeNotSatisfiable.repr
            == ServiceErrorKind::RangeNotSatisfiable as u16
    );
    assert!(ServiceErrorKindView::Throttled.repr == ServiceErrorKind::Throttled as u16);
    assert!(ServiceErrorKindView::Timeout.repr == ServiceErrorKind::Timeout as u16);
    assert!(ServiceErrorKindView::Service.repr == ServiceErrorKind::Service as u16);
};

fn open_session(endpoint: &[u8], container: &[u8], token: &[u8]) -> Box<Session> {
    // A value that is not text cannot be the thing it names, so it fails as
    // that thing rather than as a fourth kind of fault.
    let (Ok(endpoint), Ok(container), Ok(token)) = (
        std::str::from_utf8(endpoint),
        std::str::from_utf8(container),
        std::str::from_utf8(token),
    ) else {
        let code = match (
            std::str::from_utf8(endpoint).is_err(),
            std::str::from_utf8(container).is_err(),
        ) {
            (true, _) => ErrorCode::InvalidEndpoint,
            (_, true) => ErrorCode::InvalidContainer,
            _ => ErrorCode::InvalidToken,
        };
        return Box::new(Session::faulted(code));
    };
    let session = Session {
        endpoint: endpoint.to_owned(),
        container: container.to_owned(),
        token: token.to_owned(),
        status: Status { code: 0, detail: 0 },
    };
    // Refuse an unusable session here, rather than once per request.
    match session.blobs() {
        Ok(_) => Box::new(session),
        Err(error) => Box::new(Session::faulted(error.code())),
    }
}

impl Session {
    fn faulted(code: ErrorCode) -> Self {
        Self {
            endpoint: String::new(),
            container: String::new(),
            token: String::new(),
            status: Status {
                code: code as u16,
                detail: 0,
            },
        }
    }

    fn status(&self) -> Status {
        self.status
    }

    fn blobs(&self) -> Result<Blobs<'_>, Error> {
        Blobs::new(
            Container::new(&self.endpoint, &self.container)?,
            &self.token,
        )
    }

    // What every call needs before the core crate sees it: a session that was
    // opened. It reports the fault it was opened with, not a second guess at
    // which of the three values was wrong.
    fn usable(&self) -> Result<Blobs<'_>, Status> {
        if self.status.code != 0 {
            return Err(self.status);
        }
        self.blobs().map_err(|error| status_of(&error))
    }

    // What every request needs on top of that: a key that is text, and the
    // plan's shape as the core crate spells it.
    fn planning<'s, 'k, V, S>(
        &'s self,
        shape: &V,
        convert: impl FnOnce(&V) -> Result<S, Status>,
        key: &'k [u8],
    ) -> Result<(Blobs<'s>, S, &'k str), Status> {
        let blobs = self.usable()?;
        let Ok(key) = std::str::from_utf8(key) else {
            return Err(status_of(&Error::InvalidPlan(InvalidPlan::Key)));
        };
        Ok((blobs, convert(shape)?, key))
    }

    // What every reading call needs: the same shape the request was planned
    // with, and the head where the host's HTTP library already put it.
    fn reading<'s, 'h, V, S>(
        &'s self,
        shape: &V,
        convert: impl FnOnce(&V) -> Result<S, Status>,
        status: u16,
        headers: &[HeaderRef<'h>],
    ) -> Result<(Blobs<'s>, S, ResponseHead<'h>), Status> {
        let blobs = self.usable()?;
        Ok((blobs, convert(shape)?, head_of(status, headers)))
    }

    // What every finishing call needs. The status and the request identifier
    // are the plain values the outcome carried, so nothing is read twice.
    fn finishing<'s, 'h>(
        &'s self,
        failure: &FailureView<'h>,
    ) -> Result<(Blobs<'s>, u16, Option<&'h [u8]>), Status> {
        Ok((self.usable()?, failure.status, bytes(failure.request_id)))
    }

    fn encode_get(
        &self,
        shape: &GetShapeView,
        key: &[u8],
        condition_value: &[u8],
        buf: &mut [u8],
        unix_seconds: u64,
    ) -> RequestHead {
        let (blobs, shape, key) = match self.planning(shape, get_shape, key) {
            Ok(planned) => planned,
            Err(status) => return refused(status, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let get = PhysicalGet::from_shape(shape, key, condition(condition_value));
        written(blobs.encode_get(buf, &get, &now))
    }

    fn encode_put(
        &self,
        shape: &PutShapeView,
        key: &[u8],
        condition_value: &[u8],
        buf: &mut [u8],
        content_len: u64,
        unix_seconds: u64,
    ) -> RequestHead {
        let (blobs, shape, key) = match self.planning(shape, put_shape, key) {
            Ok(planned) => planned,
            Err(status) => return refused(status, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let put = PhysicalPut::from_shape(shape, key, condition(condition_value));
        // The content stays in C++. Only its length reaches the head, so the
        // request borrows no content and you send the bytes yourself.
        let content = Payload::Streamed { len: content_len };
        written(blobs.encode_put(buf, &put, content, &now))
    }

    fn encode_delete(
        &self,
        shape: &DeleteShapeView,
        key: &[u8],
        condition_value: &[u8],
        buf: &mut [u8],
        unix_seconds: u64,
    ) -> RequestHead {
        let (blobs, shape, key) = match self.planning(shape, delete_shape, key) {
            Ok(planned) => planned,
            Err(status) => return refused(status, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let delete = PhysicalDelete::from_shape(shape, key, condition(condition_value));
        written(blobs.encode_delete(buf, &delete, &now))
    }

    fn accept_get_head<'h>(
        &self,
        shape: &GetShapeView,
        status: u16,
        headers: &[HeaderRef<'h>],
    ) -> Outcome<'h> {
        match self.reading(shape, get_shape, status, headers) {
            Ok((blobs, shape, head)) => match blobs.accept_get_head(shape, head) {
                Ok(outcome) => get_outcome(&outcome),
                Err(error) => invalid(status_of(&error)),
            },
            Err(status) => invalid(status),
        }
    }

    fn accept_put_head<'h>(
        &self,
        shape: &PutShapeView,
        status: u16,
        headers: &[HeaderRef<'h>],
    ) -> Outcome<'h> {
        match self.reading(shape, put_shape, status, headers) {
            Ok((blobs, shape, head)) => match blobs.accept_put_head(shape, head) {
                Ok(outcome) => put_outcome(&outcome),
                Err(error) => invalid(status_of(&error)),
            },
            Err(status) => invalid(status),
        }
    }

    fn accept_delete_head<'h>(
        &self,
        shape: &DeleteShapeView,
        status: u16,
        headers: &[HeaderRef<'h>],
    ) -> Outcome<'h> {
        match self.reading(shape, delete_shape, status, headers) {
            Ok((blobs, shape, head)) => match blobs.accept_delete_head(shape, head) {
                Ok(outcome) => delete_outcome(&outcome),
                Err(error) => invalid(status_of(&error)),
            },
            Err(status) => invalid(status),
        }
    }

    fn finish_get_error_body<'h>(&self, failure: &FailureView<'h>, body: &[u8]) -> Outcome<'h> {
        match self.finishing(failure) {
            Ok((blobs, status, id)) => get_outcome(&blobs.accept_error_body(status, id, body)),
            Err(status) => invalid(status),
        }
    }

    fn finish_put_error_body<'h>(&self, failure: &FailureView<'h>, body: &[u8]) -> Outcome<'h> {
        match self.finishing(failure) {
            Ok((blobs, status, id)) => put_outcome(&blobs.accept_put_error_body(status, id, body)),
            Err(status) => invalid(status),
        }
    }

    fn finish_delete_error_body<'h>(&self, failure: &FailureView<'h>, body: &[u8]) -> Outcome<'h> {
        match self.finishing(failure) {
            Ok((blobs, status, id)) => {
                delete_outcome(&blobs.accept_delete_error_body(status, id, body))
            }
            Err(status) => invalid(status),
        }
    }
}

// The head, read where the host's HTTP library already put it. A name that is
// not text is skipped: the core crate looks for its headers by text, so such a
// name is none of them.
fn head_of<'h>(status: u16, headers: &[HeaderRef<'h>]) -> ResponseHead<'h> {
    ResponseHead::from_headers(
        status,
        headers
            .iter()
            .filter_map(|header| Some((std::str::from_utf8(header.name).ok()?, header.value))),
    )
}

// The written head, or the exact size that it needed, or why the plan was
// refused. All three are one `Status` and one `required`.
fn written(request: Result<WireRequest<'_>, Error>) -> RequestHead {
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
        method: Method {
            repr: request.method() as u8,
        },
        url: span(request.url_span()),
        header_count: request.header_spans().len(),
        headers,
    }
}

fn refused(status: Status, required: usize) -> RequestHead {
    RequestHead {
        status,
        required,
        method: Method::Get,
        url: Span { start: 0, len: 0 },
        header_count: 0,
        headers: empty_headers(),
    }
}

fn empty_headers() -> [RequestHeader; MAX_HEADERS] {
    [(); MAX_HEADERS].map(|()| RequestHeader {
        name: Span { start: 0, len: 0 },
        value: Span { start: 0, len: 0 },
    })
}

fn span(span: borink_object_storage::Span) -> Span {
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

fn condition(value: &[u8]) -> Option<&[u8]> {
    (!value.is_empty()).then_some(value)
}

fn get_shape(shape: &GetShapeView) -> Result<GetShape, Status> {
    Ok(GetShape {
        kind: GetKind::from_discriminant(shape.kind.repr).ok_or_else(unknown)?,
        range: RequestedRange::from_parts(
            RangeForm::from_discriminant(shape.range.form.repr).ok_or_else(unknown)?,
            shape.range.start,
            shape.range.end,
        ),
        condition: condition_kind(shape.condition)?,
    })
}

fn put_shape(shape: &PutShapeView) -> Result<PutShape, Status> {
    Ok(PutShape {
        condition: condition_kind(shape.condition)?,
    })
}

fn delete_shape(shape: &DeleteShapeView) -> Result<DeleteShape, Status> {
    Ok(DeleteShape {
        kind: DeleteKind::from_discriminant(shape.kind.repr).ok_or_else(unknown)?,
        condition: condition_kind(shape.condition)?,
    })
}

fn condition_kind(condition: ConditionView) -> Result<ConditionKind, Status> {
    ConditionKind::from_discriminant(condition.repr).ok_or_else(unknown)
}

fn class_view(class: FailureClass) -> FailureClassView {
    FailureClassView { repr: class as u16 }
}

fn class_of(class: FailureClassView) -> Option<FailureClass> {
    FailureClass::from_discriminant(class.repr)
}

fn kind_view(kind: Option<ServiceErrorKind>) -> ServiceErrorKindView {
    ServiceErrorKindView {
        repr: kind.map_or(0, |kind| kind as u16),
    }
}

fn kind_of(kind: ServiceErrorKindView) -> Option<ServiceErrorKind> {
    ServiceErrorKind::from_discriminant(kind.repr)
}

fn failure_view<'h>(failure: &Failure<'h>) -> FailureView<'h> {
    FailureView {
        status: failure.status,
        category: class_view(failure.class),
        kind: kind_view(failure.kind),
        request_id: maybe_bytes(failure.request_id),
    }
}

// The failure that the twin carries, as the core crate's own record, so that
// the sentence for it is the core crate's own too. It is `None` only for a
// category that a later core crate defined and this bridge cannot name.
fn failure_of<'h>(failure: &FailureView<'h>) -> Option<Failure<'h>> {
    Some(Failure {
        status: failure.status,
        class: class_of(failure.category)?,
        kind: kind_of(failure.kind),
        request_id: bytes(failure.request_id),
    })
}

// A named error and nothing else. A missing object is not a failure of the
// head: the core crate's variant carries a kind alone, and so does this.
fn named_error<'h>(kind: Option<ServiceErrorKind>) -> FailureView<'h> {
    FailureView {
        status: 0,
        category: FailureClassView { repr: 0 },
        kind: kind_view(kind),
        request_id: absent_bytes(),
    }
}

fn get_outcome<'h>(outcome: &GetHeadOutcome<'h>) -> Outcome<'h> {
    let mut view = empty_outcome(Disposition::Unsupported);
    match *outcome {
        GetHeadOutcome::Body { meta, body } => {
            view.disposition = Disposition::Body;
            view.meta = meta_view(&meta);
            view.body = body_view(&body);
        }
        GetHeadOutcome::Complete { meta } => {
            view.disposition = Disposition::Complete;
            view.meta = meta_view(&meta);
        }
        GetHeadOutcome::NotModified { e_tag } => {
            view.disposition = Disposition::NotModified;
            view.meta.e_tag = maybe_bytes(e_tag);
        }
        GetHeadOutcome::PreconditionFailed => view.disposition = Disposition::PreconditionFailed,
        GetHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound;
            view.failure = named_error(kind);
        }
        GetHeadOutcome::RangeNotSatisfiable { object_size } => {
            view.disposition = Disposition::RangeNotSatisfiable;
            view.body.object_size = maybe_number(object_size);
        }
        GetHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody;
            view.failure = failure_view(&failure);
        }
        GetHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure;
            view.failure = failure_view(&failure);
        }
        // The outcome is sealed, so a later version can add a variant. Report
        // one that this bridge does not know rather than guessing at it.
        _ => {}
    }
    view
}

fn put_outcome<'h>(outcome: &PutHeadOutcome<'h>) -> Outcome<'h> {
    let mut view = empty_outcome(Disposition::Unsupported);
    match *outcome {
        PutHeadOutcome::Created { meta } => {
            view.disposition = Disposition::Done;
            view.meta = meta_view(&meta);
        }
        PutHeadOutcome::PreconditionFailed => view.disposition = Disposition::PreconditionFailed,
        PutHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound;
            view.failure = named_error(kind);
        }
        PutHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody;
            view.failure = failure_view(&failure);
        }
        PutHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure;
            view.failure = failure_view(&failure);
        }
        _ => {}
    }
    view
}

fn delete_outcome<'h>(outcome: &DeleteHeadOutcome<'h>) -> Outcome<'h> {
    let mut view = empty_outcome(Disposition::Unsupported);
    match *outcome {
        // A removal returns no object, so Azure sends no metadata for one.
        DeleteHeadOutcome::Accepted => view.disposition = Disposition::Accepted,
        DeleteHeadOutcome::PreconditionFailed => view.disposition = Disposition::PreconditionFailed,
        DeleteHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound;
            view.failure = named_error(kind);
        }
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody;
            view.failure = failure_view(&failure);
        }
        DeleteHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure;
            view.failure = failure_view(&failure);
        }
        _ => {}
    }
    view
}

fn invalid<'h>(status: Status) -> Outcome<'h> {
    let mut view = empty_outcome(Disposition::Invalid);
    view.error = status;
    view
}

fn empty_outcome<'h>(disposition: Disposition) -> Outcome<'h> {
    Outcome {
        disposition,
        meta: ObjectMetaView {
            size: absent_number(),
            e_tag: absent_bytes(),
            last_modified: absent_bytes(),
            version: absent_bytes(),
            content_encoding: absent_bytes(),
        },
        body: BodyWindowView {
            object_offset: 0,
            expected_len: absent_number(),
            object_size: absent_number(),
        },
        failure: named_error(None),
        error: Status { code: 0, detail: 0 },
    }
}

fn meta_view<'h>(meta: &ObjectMeta<'h>) -> ObjectMetaView<'h> {
    ObjectMetaView {
        size: maybe_number(meta.size),
        e_tag: maybe_bytes(meta.e_tag),
        last_modified: maybe_bytes(meta.last_modified),
        version: maybe_bytes(meta.version),
        content_encoding: maybe_bytes(meta.content_encoding),
    }
}

fn body_view(body: &BodyWindow) -> BodyWindowView {
    BodyWindowView {
        object_offset: body.object_offset,
        expected_len: maybe_number(body.expected_len),
        object_size: maybe_number(body.object_size),
    }
}

fn maybe_bytes(value: Option<&[u8]>) -> MaybeBytes<'_> {
    match value {
        Some(bytes) => MaybeBytes {
            present: true,
            bytes,
        },
        None => absent_bytes(),
    }
}

fn bytes(value: MaybeBytes<'_>) -> Option<&[u8]> {
    value.present.then_some(value.bytes)
}

fn absent_bytes<'h>() -> MaybeBytes<'h> {
    MaybeBytes {
        present: false,
        bytes: &[],
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

fn describe(outcome: &Outcome<'_>, into: &mut [u8]) -> usize {
    match outcome.disposition {
        Disposition::Invalid => describe_status(outcome.error, into),
        // The core crate wrote the sentence for a failure and for an
        // unsatisfiable range, and both carry numbers that no table holds.
        // The twin carries every field of them, so the sentence is borrowed.
        Disposition::NeedErrorBody | Disposition::ServiceFailure => {
            match failure_of(&outcome.failure) {
                Some(failure) => say(into, &failure),
                None => say(
                    into,
                    &"the service failed in a way that this bridge cannot name",
                ),
            }
        }
        Disposition::RangeNotSatisfiable => say(
            into,
            &GetHeadOutcome::RangeNotSatisfiable {
                object_size: number(outcome.body.object_size),
            },
        ),
        // A missing object names an error and carries nothing else, so the
        // error is the whole sentence.
        Disposition::NotFound => match kind_of(outcome.failure.kind) {
            Some(kind) => say(into, &kind),
            None => say(into, &"the object or its container does not exist"),
        },
        // One literal per remaining disposition. They say less than the core
        // crate's own sentences, which name the operation: one outcome type
        // crosses for all three operations, so the sentence names none.
        settled => say(into, &settled_sentence(settled)),
    }
}

fn settled_sentence(disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::Body => "the object follows in the response body",
        Disposition::Complete => "the response carries no body and is complete",
        Disposition::NotModified => "the object is not modified",
        Disposition::PreconditionFailed => "the condition did not hold",
        Disposition::Done => "the service stored the object",
        Disposition::Accepted => "the service accepted the removal",
        _ => "the core crate returned an outcome that this bridge does not know",
    }
}

fn describe_status(status: Status, into: &mut [u8]) -> usize {
    let Some(code) = ErrorCode::from_discriminant(status.code) else {
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
    use crate::ffi::RangeView;
    use borink_object_storage::{Mismatch, ProtocolFault};

    // Two buffers, so that nothing here depends on one contiguous head.
    const VALUES: &[u8] = b"\"etag\"Wed, 26 Aug 2026 12:00:00 GMTversion-1gzip";
    const IDENTIFIER: &[u8] = b"request-123";

    fn e_tag() -> &'static [u8] {
        &VALUES[..6]
    }

    fn session() -> Box<Session> {
        open_session(
            b"https://account.blob.core.windows.net",
            b"container",
            b"token",
        )
    }

    fn whole() -> RangeView {
        RangeView {
            form: RangeFormView::Whole,
            start: 0,
            end: 0,
        }
    }

    fn read_shape() -> GetShapeView {
        GetShapeView {
            kind: GetKindView::Bytes,
            range: whole(),
            condition: ConditionView::None,
        }
    }

    fn write_shape() -> PutShapeView {
        PutShapeView {
            condition: ConditionView::None,
        }
    }

    fn header(name: &'static str, value: &'static [u8]) -> HeaderRef<'static> {
        HeaderRef {
            name: name.as_bytes(),
            value,
        }
    }

    fn text(outcome: &Outcome<'_>) -> String {
        let mut into = [0; 256];
        let length = describe(outcome, &mut into);
        assert!(length <= into.len(), "{length}");
        String::from_utf8(into[..length].to_vec()).unwrap()
    }

    fn full_meta() -> ObjectMeta<'static> {
        ObjectMeta {
            size: Some(10),
            e_tag: Some(e_tag()),
            last_modified: Some(&VALUES[6..35]),
            version: Some(&VALUES[35..44]),
            content_encoding: Some(&VALUES[44..]),
        }
    }

    fn every_failure() -> Vec<Failure<'static>> {
        let mut failures = Vec::new();
        for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
            for id in [None, Some(IDENTIFIER)] {
                failures.push(Failure {
                    status: 503,
                    class: FailureClass::Server,
                    kind,
                    request_id: id,
                });
            }
        }
        failures
    }

    // Every value that the core crate returns has one twin, the twin carries
    // every field of it, and every borrowed field points at the same bytes.
    #[test]
    fn every_read_outcome_crosses_whole() {
        let view = get_outcome(&GetHeadOutcome::Body {
            meta: full_meta(),
            body: BodyWindow {
                object_offset: 2,
                expected_len: Some(4),
                object_size: Some(10),
            },
        });
        assert_eq!(view.disposition, Disposition::Body);
        assert_eq!(
            view.meta.size,
            MaybeU64 {
                present: true,
                value: 10
            }
        );
        assert_eq!(view.meta.e_tag.bytes.as_ptr(), e_tag().as_ptr());
        assert_eq!(view.meta.e_tag.bytes, e_tag());
        assert!(view.meta.last_modified.present);
        assert!(view.meta.version.present);
        assert!(view.meta.content_encoding.present);
        assert_eq!(view.body.object_offset, 2);
        assert_eq!(view.body.expected_len.value, 4);
        assert_eq!(view.body.object_size.value, 10);

        let empty = get_outcome(&GetHeadOutcome::Body {
            meta: ObjectMeta::default(),
            body: BodyWindow {
                object_offset: 0,
                expected_len: None,
                object_size: None,
            },
        });
        assert!(!empty.meta.size.present);
        assert!(!empty.meta.e_tag.present);
        assert!(empty.meta.e_tag.bytes.is_empty());
        assert!(!empty.body.expected_len.present);

        let complete = get_outcome(&GetHeadOutcome::Complete { meta: full_meta() });
        assert_eq!(complete.disposition, Disposition::Complete);
        assert!(complete.meta.e_tag.present);

        for tag in [None, Some(e_tag())] {
            let view = get_outcome(&GetHeadOutcome::NotModified { e_tag: tag });
            assert_eq!(view.disposition, Disposition::NotModified);
            assert_eq!(view.meta.e_tag.present, tag.is_some());
        }

        assert_eq!(
            get_outcome(&GetHeadOutcome::PreconditionFailed).disposition,
            Disposition::PreconditionFailed
        );

        // A missing object carries the error it named, and no status and no
        // category that the head never stated.
        for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
            let view = get_outcome(&GetHeadOutcome::NotFound { kind });
            assert_eq!(view.disposition, Disposition::NotFound);
            assert_eq!(kind_of(view.failure.kind), kind);
            assert_eq!(view.failure.status, 0);
            assert_eq!(view.failure.category.repr, 0);
        }

        for object_size in [None, Some(10)] {
            let view = get_outcome(&GetHeadOutcome::RangeNotSatisfiable { object_size });
            assert_eq!(view.disposition, Disposition::RangeNotSatisfiable);
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
                assert_eq!(view.disposition, expected);
                assert_eq!(view.failure.status, failure.status);
                assert_eq!(class_of(view.failure.category), Some(failure.class));
                assert_eq!(kind_of(view.failure.kind), failure.kind);
                assert_eq!(bytes(view.failure.request_id), failure.request_id);
                assert_eq!(text(&view), outcome.to_string());
            }
        }
    }

    #[test]
    fn every_write_and_removal_outcome_crosses_whole() {
        let created = put_outcome(&PutHeadOutcome::Created { meta: full_meta() });
        assert_eq!(created.disposition, Disposition::Done);
        assert!(created.meta.e_tag.present);

        assert_eq!(
            delete_outcome(&DeleteHeadOutcome::Accepted).disposition,
            Disposition::Accepted
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

        // A failure says the same thing whichever operation it answers, so
        // the twin needs no field naming the operation.
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
    // is the core crate's own. A settled outcome gets a literal, which names
    // no operation because one twin answers all three.
    #[test]
    fn every_disposition_says_something_of_its_own() {
        for kind in [
            ServiceErrorKind::NotFound,
            ServiceErrorKind::NoSuchContainer,
        ] {
            let outcome = GetHeadOutcome::NotFound { kind: Some(kind) };
            assert_eq!(text(&get_outcome(&outcome)), outcome.to_string());
        }
        // A head that named neither leaves both open, and one twin answers
        // for three operations, so the sentence says both.
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
            let sentence = settled_sentence(disposition);
            assert!(!sentence.is_empty());
            assert_eq!(text(&empty_outcome(disposition)), sentence);
            said.push(sentence);
        }
        said.sort_unstable();
        said.dedup();
        assert_eq!(said.len(), 6);
    }

    // Every enum crosses as its number, and comes back the same value. A
    // number that names nothing is refused, never read as another value.
    #[test]
    fn every_enum_crosses_by_its_number_and_refuses_the_rest() {
        for repr in 1..=u16::MAX {
            if let Some(kind) = ServiceErrorKind::from_discriminant(repr) {
                assert_eq!(kind_of(kind_view(Some(kind))), Some(kind), "{kind:?}");
                assert_eq!(kind_view(Some(kind)).repr, repr);
            }
            if let Some(class) = FailureClass::from_discriminant(repr) {
                assert_eq!(class_of(class_view(class)), Some(class), "{class:?}");
                assert_eq!(class_view(class).repr, repr);
            }
        }
        assert_eq!(kind_of(kind_view(None)), None);
        assert_eq!(kind_of(ServiceErrorKindView { repr: 4095 }), None);
        assert_eq!(class_of(FailureClassView { repr: 4095 }), None);

        // The plan side, which crosses inwards and must refuse.
        for (kind, expected) in [
            (GetKindView::Bytes, Some(GetKind::Bytes)),
            (GetKindView::Metadata, Some(GetKind::Metadata)),
            (GetKindView { repr: 0 }, None),
            (GetKindView { repr: 4095 }, None),
        ] {
            let shape = GetShapeView {
                kind,
                ..read_shape()
            };
            assert_eq!(get_shape(&shape).map(|shape| shape.kind).ok(), expected);
        }
        for (form, expected) in [
            (RangeFormView::Whole, Some(RequestedRange::Whole)),
            (
                RangeFormView::Bounded,
                Some(RequestedRange::Bounded { start: 2, end: 6 }),
            ),
            (RangeFormView::Offset, Some(RequestedRange::Offset(2))),
            (RangeFormView::Suffix, Some(RequestedRange::Suffix(2))),
            (RangeFormView { repr: 0 }, None),
        ] {
            let shape = GetShapeView {
                range: RangeView {
                    form,
                    start: 2,
                    end: 6,
                },
                ..read_shape()
            };
            assert_eq!(get_shape(&shape).map(|shape| shape.range).ok(), expected);
        }
        for (condition, expected) in [
            (ConditionView::None, Some(ConditionKind::None)),
            (ConditionView::IfMatch, Some(ConditionKind::IfMatch)),
            (ConditionView::IfNoneMatch, Some(ConditionKind::IfNoneMatch)),
            (ConditionView { repr: 0 }, None),
        ] {
            assert_eq!(condition_kind(condition).ok(), expected);
        }
        for (kind, expected) in [
            (DeleteKindView::Object, Some(DeleteKind::Object)),
            (
                DeleteKindView::ObjectAndSnapshots,
                Some(DeleteKind::ObjectAndSnapshots),
            ),
            (
                DeleteKindView::SnapshotsOnly,
                Some(DeleteKind::SnapshotsOnly),
            ),
            (DeleteKindView { repr: 0 }, None),
        ] {
            let shape = DeleteShapeView {
                kind,
                condition: ConditionView::None,
            };
            assert_eq!(delete_shape(&shape).map(|shape| shape.kind).ok(), expected);
        }
    }

    // A number that this bridge does not define stops the call, and says so.
    #[test]
    fn an_unknown_number_is_refused_rather_than_read_as_another_value() {
        let session = session();
        let shape = GetShapeView {
            kind: GetKindView { repr: 4095 },
            ..read_shape()
        };
        let mut buf = vec![0; 512];
        let refused = session.encode_get(&shape, b"object.bin", b"", &mut buf, 1_787_400_000);
        assert_eq!(refused.status, unknown());
        assert_eq!(refused.status.code, ErrorCode::InvalidPlan as u16);
        assert_eq!(refused.status.detail, InvalidPlan::Unknown as u16);
        assert_eq!(refused.required, 0);

        let outcome = session.accept_get_head(&shape, 200, &[]);
        assert_eq!(outcome.disposition, Disposition::Invalid);
        assert_eq!(outcome.error, unknown());
        assert_eq!(
            text(&outcome),
            Error::InvalidPlan(InvalidPlan::Unknown).to_string()
        );
    }

    // Every error of the core crate crosses as two numbers and comes back as
    // the same sentence.
    #[test]
    fn every_error_crosses_as_a_status() {
        let mut checked = 0;
        for code in 1..=u16::MAX {
            let Some(code) = ErrorCode::from_discriminant(code) else {
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
                let length = describe_status(status, &mut into);
                assert_eq!(
                    String::from_utf8(into[..length].to_vec()).unwrap(),
                    error.to_string(),
                    "{error:?}"
                );
                checked += 1;
            }
        }
        // Every variant of the three inner enums, and the three that carry no
        // inner value.
        assert_eq!(checked, 3 + 7 + 10 + 7);
        assert_eq!(
            ProtocolFault::from_discriminant(10).map(Error::Protocol),
            Error::from_parts(ErrorCode::Protocol, 10)
        );
        assert_eq!(
            Mismatch::from_discriminant(7).map(Error::ResponseMismatch),
            Error::from_parts(ErrorCode::ResponseMismatch, 7)
        );
    }

    // A capacity error carries sizes rather than a discriminant, so it crosses
    // as a code and the `required` field of the request head.
    #[test]
    fn a_buffer_that_is_too_small_reports_the_size_it_needs() {
        let session = session();
        let shape = read_shape();
        let refused = session.encode_get(&shape, b"object.bin", b"", &mut [], 1_787_400_000);
        assert_eq!(refused.status.code, ErrorCode::Capacity as u16);
        assert!(refused.required > 0);

        let mut buf = vec![0; refused.required];
        let written = session.encode_get(&shape, b"object.bin", b"", &mut buf, 1_787_400_000);
        assert_eq!(written.status.code, 0);
        assert_eq!(written.required, refused.required);
        assert_eq!(written.method, Method::Get);
        assert_eq!(written.header_count, 3);
        let url = &buf[written.url.start..written.url.start + written.url.len];
        assert_eq!(
            std::str::from_utf8(url).unwrap(),
            "https://account.blob.core.windows.net/container/object.bin"
        );
        for index in 0..written.header_count {
            let header = written.headers[index];
            assert!(header.name.start + header.name.len <= buf.len());
            assert!(header.value.start + header.value.len <= buf.len());
        }
    }

    // A ranged, conditional read reaches the core crate from a stored shape
    // and the bytes that go with it.
    #[test]
    fn a_stored_shape_carries_the_whole_plan() {
        let session = session();
        let shape = GetShapeView {
            kind: GetKindView::Bytes,
            range: RangeView {
                form: RangeFormView::Bounded,
                start: 2,
                end: 6,
            },
            condition: ConditionView::IfNoneMatch,
        };
        let mut buf = vec![0; 512];
        let head = session.encode_get(&shape, b"object.bin", b"\"etag\"", &mut buf, 1_787_400_000);
        assert_eq!(head.status.code, 0);
        let named = |name: &str| {
            (0..head.header_count).find_map(|index| {
                let header = head.headers[index];
                let read = |span: Span| {
                    std::str::from_utf8(&buf[span.start..span.start + span.len]).unwrap()
                };
                (read(header.name) == name).then(|| read(header.value).to_owned())
            })
        };
        assert_eq!(named("range").as_deref(), Some("bytes=2-5"));
        assert_eq!(named("if-none-match").as_deref(), Some("\"etag\""));
    }

    // The head reaches the bridge as slices, from wherever the host keeps
    // them. Nothing here is one buffer, and the outcome points back at each.
    #[test]
    fn a_head_crosses_as_slices_of_whatever_holds_it() {
        let session = session();
        let headers = [
            header("ETag", e_tag()),
            header("Content-Length", b"10"),
            header("x-ms-request-id", IDENTIFIER),
            // A name that is not text is none of the ones the core crate
            // reads, so it is skipped rather than refused.
            HeaderRef {
                name: b"\xff",
                value: b"value",
            },
        ];
        let outcome = session.accept_get_head(&read_shape(), 200, &headers);
        assert_eq!(outcome.disposition, Disposition::Body);
        assert_eq!(outcome.meta.e_tag.bytes.as_ptr(), e_tag().as_ptr());
        assert_eq!(
            outcome.body.expected_len,
            MaybeU64 {
                present: true,
                value: 10
            }
        );
    }

    // The head asked for the error body, and the body names the error. The
    // request id crosses as bytes the host still owns, both ways.
    #[test]
    fn the_error_body_finishes_what_the_head_left_open() {
        let session = session();
        let headers = [header("x-ms-request-id", IDENTIFIER)];
        let outcome = session.accept_put_head(&write_shape(), 409, &headers);
        assert_eq!(outcome.disposition, Disposition::NeedErrorBody);
        assert_eq!(
            outcome.failure.request_id.bytes.as_ptr(),
            IDENTIFIER.as_ptr()
        );

        let finished = session.finish_put_error_body(
            &outcome.failure,
            b"<Error><Code>BlobAlreadyExists</Code></Error>",
        );
        assert_eq!(finished.disposition, Disposition::ServiceFailure);
        assert_eq!(
            kind_of(finished.failure.kind),
            Some(ServiceErrorKind::AlreadyExists)
        );
        assert!(text(&finished).contains("already exists"));
        assert!(text(&finished).contains("request-123"));

        // A body that never arrived leaves the outcome final and unnamed.
        let unnamed = session.finish_put_error_body(&outcome.failure, b"");
        assert_eq!(unnamed.disposition, Disposition::ServiceFailure);
        assert_eq!(kind_of(unnamed.failure.kind), None);
    }

    // A head that does not answer the plan is a `Status`, not a sentence.
    #[test]
    fn an_invalid_head_carries_the_error_of_the_core_crate() {
        let session = session();
        let outcome = session.accept_put_head(&write_shape(), 412, &[]);
        assert_eq!(outcome.disposition, Disposition::Invalid);
        assert_eq!(outcome.error.code, ErrorCode::ResponseMismatch as u16);
        assert_eq!(outcome.error.detail, Mismatch::WriteWithoutCondition as u16);
        assert_eq!(
            text(&outcome),
            Error::ResponseMismatch(Mismatch::WriteWithoutCondition).to_string()
        );
    }

    #[test]
    fn a_session_that_cannot_be_opened_says_which_value_is_wrong() {
        for (endpoint, container, token, expected) in [
            (
                "account.example".as_bytes(),
                "container".as_bytes(),
                "token".as_bytes(),
                ErrorCode::InvalidEndpoint,
            ),
            (
                b"https://account.example",
                b"".as_slice(),
                b"token".as_slice(),
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
            let session = open_session(endpoint, container, token);
            assert_eq!(session.status().code, expected as u16);
            // A session that cannot build a request cannot read the answer to
            // one, and says the same thing when asked to.
            let refused = session.encode_get(&read_shape(), b"key", b"", &mut [], 0);
            assert_eq!(refused.status, session.status());
            let outcome = session.accept_get_head(&read_shape(), 200, &[]);
            assert_eq!(outcome.disposition, Disposition::Invalid);
            assert_eq!(outcome.error, session.status());
        }
        assert_eq!(session().status().code, 0);
    }

    // A sentence longer than the buffer is counted, not cut off silently.
    #[test]
    fn a_short_buffer_still_learns_the_length_of_the_sentence() {
        let outcome = get_outcome(&GetHeadOutcome::ServiceFailure(Failure {
            status: 503,
            class: FailureClass::Server,
            kind: None,
            request_id: Some(IDENTIFIER),
        }));
        let mut small = [0; 4];
        let length = describe(&outcome, &mut small);
        assert!(length > small.len());
        let mut whole = vec![0; length];
        assert_eq!(describe(&outcome, &mut whole), length);
    }
}
