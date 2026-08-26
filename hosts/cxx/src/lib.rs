//! The bridge that lets a C++ application drive `borink-object-storage`.
//!
//! This is for an application that already has an HTTP client and its own
//! memory budget. It keeps both. The bridge plans a request, writes the
//! request head into the buffer that C++ handed it, and reads the response
//! head that C++ collected. It opens no socket, reads no clock, and keeps
//! nothing between calls.
//!
//! # What it costs
//!
//! One allocation per session, in `open_session`, for the endpoint, the
//! container and the token. **Nothing per request**: every other call writes
//! into memory that C++ supplied and returns a value that holds no pointer.
//!
//! No call returns `Result`, so no call throws. A C++ application built
//! without exceptions can use this bridge. Each call reports what happened in
//! a field of the value that it returns.
//!
//! # Everything is a range of your buffer
//!
//! `encode_get` writes the request head into your buffer and returns a
//! `RequestHead`, which names the URL and each header by offset and length.
//! The core crate writes every byte of the head into that one buffer,
//! including each header name, so there is nothing else to read.
//!
//! Call `encode_get` with an empty buffer to learn the size: it reports
//! `ErrorCode::Capacity` and the exact number of bytes. Size the buffer once
//! per client and reuse it.
//!
//! A response works the same way in reverse. You keep the response head in
//! your own bytes and name each header with a `HeaderField`. The outcome then
//! names each metadata value by offset and length into those same bytes.
//!
//! # How a failure crosses
//!
//! Every failure crosses as a `Status`, which is the error code and the
//! discriminant that the core crate defines. `describe_status` writes the
//! sentence for one. A response that the service sends in normal operation is
//! not a failure: it is a `Disposition` on the `Outcome`.
//!
//! # Examples
//!
//! ```cpp
//! rust::Box<borink::Session> session = borink::open_session(endpoint, container, token);
//! if (session->status().code != 0) { /* ... */ }
//!
//! borink::GetShapeView shape = borink::whole_object();
//! borink::RequestHead head = session->encode_get(shape, key, {}, {buffer.data(), buffer.size()}, now);
//! if (head.status.code == static_cast<std::uint16_t>(borink::ErrorCode::Capacity)) {
//!     buffer.resize(head.required);
//!     head = session->encode_get(shape, key, {}, {buffer.data(), buffer.size()}, now);
//! }
//! // ... send head.url and head.headers with your HTTP client ...
//!
//! borink::Outcome outcome = session->accept_get_head(shape, status, collected, fields);
//! if (outcome.disposition == borink::Disposition::Body) {
//!     // ... read the body ...
//! }
//! ```

use std::fmt::{self, Write as _};

use borink_object_storage::{
    Blobs, BodyWindow, ConditionKind, Container, DeleteHeadOutcome, DeleteKind, DeleteShape, Error,
    ErrorCode, Failure, FailureClass, GetHeadOutcome, GetKind, GetShape, InvalidPlan, ObjectMeta,
    Payload, PhysicalDelete, PhysicalGet, PhysicalPut, PutHeadOutcome, PutShape, RequestedRange,
    ResponseHead, ServiceErrorKind, Timestamps, WireRequest,
};

use ffi::{
    BodyWindowView, ConditionView, DeleteKindView, DeleteShapeView, Disposition, FailureClassView,
    FailureView, GetKindView, GetShapeView, HeaderField, MaybeSpan, MaybeU64, Method,
    ObjectMetaView, Operation, Outcome, PutShapeView, RangeForm, RequestHead, RequestHeader,
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
    /// A range of bytes, as an offset from the start of a buffer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Span {
        /// The offset of the first byte.
        start: usize,
        /// The number of bytes.
        len: usize,
    }

    /// A range that the bytes may not carry.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct MaybeSpan {
        /// Whether the value is present.
        present: bool,
        /// The range that holds it.
        span: Span,
    }

    /// A number that the response head may not carry.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct MaybeU64 {
        /// Whether the head carried this number.
        present: bool,
        /// The number.
        value: u64,
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
    /// These mirror the `ErrorCode` of the core crate.
    #[derive(Debug)]
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
    enum GetKindView {
        /// The bytes of the object.
        Bytes = 1,
        /// The metadata of the object, without its bytes.
        Metadata = 2,
    }

    /// Which form of byte range a read requests.
    #[derive(Debug)]
    enum RangeForm {
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
        form: RangeForm,
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
    /// `accept_get_head` when the response arrives.
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

    /// One response header, as two ranges of the bytes that you kept.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct HeaderField {
        /// The range that holds the header name.
        name: Span,
        /// The range that holds the header value.
        value: Span,
    }

    /// Object metadata, as ranges of the response head.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct ObjectMetaView {
        /// The size of the whole object.
        size: MaybeU64,
        /// The entity tag.
        e_tag: MaybeSpan,
        /// The value of the `Last-Modified` header.
        last_modified: MaybeSpan,
        /// The version identifier.
        version: MaybeSpan,
        /// The value of the `Content-Encoding` header.
        content_encoding: MaybeSpan,
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
    /// These mirror the `FailureClass` of the core crate.
    #[derive(Debug)]
    enum FailureClassView {
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
        /// A category that this bridge does not know.
        Unknown = 6,
    }

    /// The specific error that the service named.
    ///
    /// These mirror the `ServiceErrorKind` of the core crate.
    #[derive(Debug)]
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
        /// An error that this bridge does not know.
        Unknown = 10,
    }

    /// A response head that reports a failure.
    ///
    /// Store one of these and pass it back to `finish_get_error_body` to
    /// finish a `Disposition::NeedErrorBody`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FailureView {
        /// The HTTP status code.
        status: u16,
        /// The category of the failure. Use it to decide whether to retry.
        ///
        /// This is `class` in the core crate, which C++ cannot spell.
        category: FailureClassView,
        /// The specific error, if the head or the body named one.
        kind: ServiceErrorKindView,
        /// The range of the response head that holds `x-ms-request-id`.
        request_id: MaybeSpan,
    }

    /// Which operation a response answers.
    #[derive(Debug)]
    enum Operation {
        /// A read.
        Read = 1,
        /// A write.
        Write = 2,
        /// A removal.
        Removal = 3,
    }

    /// What a response tells you to do.
    #[derive(Debug)]
    enum Disposition {
        /// A body follows. Read it and put the bytes at `body`.
        Body = 1,
        /// No body follows and the read is complete.
        Complete = 2,
        /// The `If-None-Match` condition held, so Azure sent no body.
        NotModified = 3,
        /// The condition did not hold, so Azure changed nothing.
        PreconditionFailed = 4,
        /// The object or its container does not exist.
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
        /// The head is invalid, or it does not answer the plan. Read `error`.
        Invalid = 11,
        /// One `HeaderField` names a range outside the head that you passed.
        InvalidInput = 12,
        /// The core crate returned a variant that this bridge does not know.
        Unsupported = 13,
    }

    /// The result of reading one response head.
    ///
    /// One value describes a read, a write and a removal. The fields that the
    /// operation does not fill are absent.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Outcome {
        /// Which operation this answers.
        operation: Operation,
        /// What to do with the response.
        disposition: Disposition,
        /// The metadata from the head.
        meta: ObjectMetaView,
        /// Where the bytes of the body belong.
        body: BodyWindowView,
        /// The failure, for `NeedErrorBody`, `ServiceFailure` and `NotFound`.
        failure: FailureView,
        /// What is wrong with the head, for `Invalid`.
        error: Status,
    }

    extern "Rust" {
        type Session;

        /// Opens a session against one container.
        ///
        /// This is the one call that allocates. It copies the three values, so
        /// none of them has to outlive the call. A value that is not usable
        /// leaves the session with a `status`, and every request refuses.
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
        /// Pass the same `shape` that you passed to `encode_get`. `head` holds
        /// the response header bytes, and each `HeaderField` names one header
        /// inside it. The outcome names ranges of `head`.
        fn accept_get_head(
            self: &Session,
            shape: &GetShapeView,
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
        ) -> Outcome;

        /// Reads the response head of a write.
        fn accept_put_head(
            self: &Session,
            shape: &PutShapeView,
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
        ) -> Outcome;

        /// Reads the response head of a removal.
        fn accept_delete_head(
            self: &Session,
            shape: &DeleteShapeView,
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
        ) -> Outcome;

        /// Finishes a read whose head asked for the error body.
        ///
        /// Pass the `failure` of that outcome, the same `head` bytes, and the
        /// body that you read. Pass an empty body if you read none: the
        /// outcome is then final with the error unnamed.
        fn finish_get_error_body(
            self: &Session,
            failure: &FailureView,
            head: &[u8],
            body: &[u8],
        ) -> Outcome;

        /// Finishes a write whose head asked for the error body.
        fn finish_put_error_body(
            self: &Session,
            failure: &FailureView,
            head: &[u8],
            body: &[u8],
        ) -> Outcome;

        /// Finishes a removal whose head asked for the error body.
        fn finish_delete_error_body(
            self: &Session,
            failure: &FailureView,
            head: &[u8],
            body: &[u8],
        ) -> Outcome;

        /// Writes one sentence naming what `outcome` says.
        ///
        /// Pass the `head` bytes that the outcome names, so that the sentence
        /// can carry the request identifier. Returns the length of the whole
        /// sentence, which may be longer than `into`: the part that fits is
        /// written, and nothing is allocated.
        fn describe(outcome: &Outcome, head: &[u8], into: &mut [u8]) -> usize;

        /// Writes one sentence naming what `status` says.
        ///
        /// Returns the length of the whole sentence, exactly as `describe`.
        fn describe_status(status: Status, into: &mut [u8]) -> usize;
    }
}

// The twin carries the core crate's headers and no more. A header added there
// stops this build rather than being dropped at the boundary.
const _: () = assert!(MAX_HEADERS == 6);

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

    // What every request needs before the core crate sees it: a usable
    // session, and a key that is text.
    fn plan<'s, 'k>(&'s self, key: &'k [u8]) -> Result<(&'k str, Blobs<'s>), Status> {
        if self.status.code != 0 {
            return Err(self.status);
        }
        let Ok(key) = std::str::from_utf8(key) else {
            return Err(status_of(&Error::InvalidPlan(InvalidPlan::Key)));
        };
        match self.blobs() {
            Ok(blobs) => Ok((key, blobs)),
            Err(error) => Err(status_of(&error)),
        }
    }

    fn encode_get(
        &self,
        shape: &GetShapeView,
        key: &[u8],
        condition_value: &[u8],
        buf: &mut [u8],
        unix_seconds: u64,
    ) -> RequestHead {
        let (key, blobs) = match self.plan(key) {
            Ok(planned) => planned,
            Err(status) => return refused(status, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let get = PhysicalGet::from_shape(get_shape(shape), key, condition(condition_value));
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
        let (key, blobs) = match self.plan(key) {
            Ok(planned) => planned,
            Err(status) => return refused(status, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let put = PhysicalPut::from_shape(put_shape(shape), key, condition(condition_value));
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
        let (key, blobs) = match self.plan(key) {
            Ok(planned) => planned,
            Err(status) => return refused(status, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let delete =
            PhysicalDelete::from_shape(delete_shape(shape), key, condition(condition_value));
        written(blobs.encode_delete(buf, &delete, &now))
    }

    fn accept_get_head(
        &self,
        shape: &GetShapeView,
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
    ) -> Outcome {
        let (blobs, response) = match self.reading(status, head, fields) {
            Ok(reading) => reading,
            Err(outcome) => return outcome(Operation::Read),
        };
        match blobs.accept_get_head(get_shape(shape), response) {
            Ok(outcome) => get_outcome(Buffer::of(head), &outcome),
            Err(error) => invalid(Operation::Read, &error),
        }
    }

    fn accept_put_head(
        &self,
        shape: &PutShapeView,
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
    ) -> Outcome {
        let (blobs, response) = match self.reading(status, head, fields) {
            Ok(reading) => reading,
            Err(outcome) => return outcome(Operation::Write),
        };
        match blobs.accept_put_head(put_shape(shape), response) {
            Ok(outcome) => put_outcome(Buffer::of(head), &outcome),
            Err(error) => invalid(Operation::Write, &error),
        }
    }

    fn accept_delete_head(
        &self,
        shape: &DeleteShapeView,
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
    ) -> Outcome {
        let (blobs, response) = match self.reading(status, head, fields) {
            Ok(reading) => reading,
            Err(outcome) => return outcome(Operation::Removal),
        };
        match blobs.accept_delete_head(delete_shape(shape), response) {
            Ok(outcome) => delete_outcome(Buffer::of(head), &outcome),
            Err(error) => invalid(Operation::Removal, &error),
        }
    }

    fn finish_get_error_body(&self, failure: &FailureView, head: &[u8], body: &[u8]) -> Outcome {
        let (blobs, status, request_id) = match self.finishing(failure, head) {
            Ok(parts) => parts,
            Err(outcome) => return outcome(Operation::Read),
        };
        get_outcome(
            Buffer::of(head),
            &blobs.accept_error_body(status, request_id, body),
        )
    }

    fn finish_put_error_body(&self, failure: &FailureView, head: &[u8], body: &[u8]) -> Outcome {
        let (blobs, status, request_id) = match self.finishing(failure, head) {
            Ok(parts) => parts,
            Err(outcome) => return outcome(Operation::Write),
        };
        put_outcome(
            Buffer::of(head),
            &blobs.accept_put_error_body(status, request_id, body),
        )
    }

    fn finish_delete_error_body(&self, failure: &FailureView, head: &[u8], body: &[u8]) -> Outcome {
        let (blobs, status, request_id) = match self.finishing(failure, head) {
            Ok(parts) => parts,
            Err(outcome) => return outcome(Operation::Removal),
        };
        delete_outcome(
            Buffer::of(head),
            &blobs.accept_delete_error_body(status, request_id, body),
        )
    }

    // The head that C++ collected, read in place. A field that names a range
    // outside it is the caller's mistake and stops the call.
    fn reading<'h>(
        &self,
        status: u16,
        head: &'h [u8],
        fields: &[HeaderField],
    ) -> Result<(Blobs<'_>, ResponseHead<'h>), fn(Operation) -> Outcome> {
        let Ok(blobs) = self.blobs() else {
            return Err(unusable_session);
        };
        let Some(response) = response_head(status, head, fields) else {
            return Err(invalid_input);
        };
        Ok((blobs, response))
    }

    #[allow(clippy::type_complexity)]
    fn finishing<'h>(
        &self,
        failure: &FailureView,
        head: &'h [u8],
    ) -> Result<(Blobs<'_>, u16, Option<&'h [u8]>), fn(Operation) -> Outcome> {
        let Ok(blobs) = self.blobs() else {
            return Err(unusable_session);
        };
        let request_id = match failure.request_id.present {
            false => None,
            true => match at(head, failure.request_id.span) {
                Some(bytes) => Some(bytes),
                None => return Err(invalid_input),
            },
        };
        Ok((blobs, failure.status, request_id))
    }
}

fn unusable_session(operation: Operation) -> Outcome {
    // A session that cannot build a request cannot read the answer to one.
    let mut outcome = empty_outcome(operation, Disposition::Invalid);
    outcome.error = Status {
        code: ErrorCode::InvalidEndpoint as u16,
        detail: 0,
    };
    outcome
}

fn invalid_input(operation: Operation) -> Outcome {
    empty_outcome(operation, Disposition::InvalidInput)
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
        method: method_view(request.method()),
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

// Where a buffer starts and how long it is. An outcome borrows the response
// head, so its address and length give each borrowed value as a range of it.
#[derive(Debug, Clone, Copy)]
struct Buffer {
    base: usize,
    len: usize,
}

impl Buffer {
    fn of(bytes: &[u8]) -> Self {
        Self {
            base: bytes.as_ptr() as usize,
            len: bytes.len(),
        }
    }

    fn span(&self, part: &[u8]) -> Option<Span> {
        let start = part.as_ptr() as usize;
        let end = start.checked_add(part.len())?;
        (start >= self.base && end <= self.base + self.len).then(|| Span {
            start: start - self.base,
            len: part.len(),
        })
    }
}

fn span(span: borink_object_storage::Span) -> Span {
    Span {
        start: span.start,
        len: span.len,
    }
}

fn at(bytes: &[u8], range: Span) -> Option<&[u8]> {
    bytes.get(range.start..range.start.checked_add(range.len)?)
}

// Reads the head that the host collected. A field whose name is not text is
// skipped: the core crate reads header names as text, so such a name cannot be
// one that it looks for. A field that names a range outside `head` is not a
// header at all, and stops the call: dropping it silently would turn a named
// failure into one that asks for the error body.
fn response_head<'h>(
    status: u16,
    head: &'h [u8],
    fields: &[HeaderField],
) -> Option<ResponseHead<'h>> {
    let mut named = Vec::new();
    for field in fields {
        let (name, value) = (at(head, field.name)?, at(head, field.value)?);
        if let Ok(name) = std::str::from_utf8(name) {
            named.push((name, value));
        }
    }
    Some(ResponseHead::from_headers(status, named))
}

fn status_of(error: &Error) -> Status {
    Status {
        code: error.code() as u16,
        detail: error.detail(),
    }
}

fn method_view(method: borink_object_storage::Method) -> Method {
    match method {
        borink_object_storage::Method::Get => Method::Get,
        borink_object_storage::Method::Head => Method::Head,
        borink_object_storage::Method::Put => Method::Put,
        borink_object_storage::Method::Delete => Method::Delete,
        // The core enum is sealed. A method it adds later is not one that this
        // bridge can send, so it is reported rather than guessed.
        _ => Method::Get,
    }
}

fn condition(value: &[u8]) -> Option<&[u8]> {
    (!value.is_empty()).then_some(value)
}

fn get_shape(shape: &GetShapeView) -> GetShape {
    GetShape {
        kind: match shape.kind {
            GetKindView::Metadata => GetKind::Metadata,
            _ => GetKind::Bytes,
        },
        range: match shape.range.form {
            RangeForm::Bounded => RequestedRange::Bounded {
                start: shape.range.start,
                end: shape.range.end,
            },
            RangeForm::Offset => RequestedRange::Offset(shape.range.start),
            RangeForm::Suffix => RequestedRange::Suffix(shape.range.start),
            _ => RequestedRange::Whole,
        },
        condition: condition_kind(shape.condition),
    }
}

fn put_shape(shape: &PutShapeView) -> PutShape {
    PutShape {
        condition: condition_kind(shape.condition),
    }
}

fn delete_shape(shape: &DeleteShapeView) -> DeleteShape {
    DeleteShape {
        kind: match shape.kind {
            DeleteKindView::ObjectAndSnapshots => DeleteKind::ObjectAndSnapshots,
            DeleteKindView::SnapshotsOnly => DeleteKind::SnapshotsOnly,
            _ => DeleteKind::Object,
        },
        condition: condition_kind(shape.condition),
    }
}

fn condition_kind(condition: ConditionView) -> ConditionKind {
    match condition {
        ConditionView::IfMatch => ConditionKind::IfMatch,
        ConditionView::IfNoneMatch => ConditionKind::IfNoneMatch,
        _ => ConditionKind::None,
    }
}

fn class_view(class: FailureClass) -> FailureClassView {
    match class {
        FailureClass::Auth => FailureClassView::Auth,
        FailureClass::Throttled => FailureClassView::Throttled,
        FailureClass::Server => FailureClassView::Server,
        FailureClass::Redirect => FailureClassView::Redirect,
        FailureClass::Other => FailureClassView::Other,
        _ => FailureClassView::Unknown,
    }
}

fn class_of(class: FailureClassView) -> FailureClass {
    match class {
        FailureClassView::Auth => FailureClass::Auth,
        FailureClassView::Throttled => FailureClass::Throttled,
        FailureClassView::Server => FailureClass::Server,
        FailureClassView::Redirect => FailureClass::Redirect,
        _ => FailureClass::Other,
    }
}

fn kind_view(kind: Option<ServiceErrorKind>) -> ServiceErrorKindView {
    match kind {
        None => ServiceErrorKindView::None,
        Some(ServiceErrorKind::NotFound) => ServiceErrorKindView::NotFound,
        Some(ServiceErrorKind::NoSuchContainer) => ServiceErrorKindView::NoSuchContainer,
        Some(ServiceErrorKind::AlreadyExists) => ServiceErrorKindView::AlreadyExists,
        Some(ServiceErrorKind::Unauthorized) => ServiceErrorKindView::Unauthorized,
        Some(ServiceErrorKind::Precondition) => ServiceErrorKindView::Precondition,
        Some(ServiceErrorKind::RangeNotSatisfiable) => ServiceErrorKindView::RangeNotSatisfiable,
        Some(ServiceErrorKind::Throttled) => ServiceErrorKindView::Throttled,
        Some(ServiceErrorKind::Timeout) => ServiceErrorKindView::Timeout,
        Some(ServiceErrorKind::Service) => ServiceErrorKindView::Service,
        Some(_) => ServiceErrorKindView::Unknown,
    }
}

fn kind_of(kind: ServiceErrorKindView) -> Option<ServiceErrorKind> {
    ServiceErrorKind::from_discriminant(kind.repr as u16)
}

fn failure_view(head: Buffer, failure: &Failure<'_>) -> FailureView {
    FailureView {
        status: failure.status,
        category: class_view(failure.class),
        kind: kind_view(failure.kind),
        request_id: maybe_span(head, failure.request_id),
    }
}

// A `NotFound` names an error without being a failure of the head. It rides in
// the same field, so one shape carries every named error.
fn not_found_view(status: u16, kind: Option<ServiceErrorKind>) -> FailureView {
    FailureView {
        status,
        category: FailureClassView::Other,
        kind: kind_view(kind),
        request_id: absent_span(),
    }
}

fn get_outcome(head: Buffer, outcome: &GetHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(Operation::Read, Disposition::Unsupported);
    match outcome {
        GetHeadOutcome::Body { meta, body, .. } => {
            view.disposition = Disposition::Body;
            view.meta = meta_view(head, meta);
            view.body = body_view(body);
        }
        GetHeadOutcome::Complete { meta } => {
            view.disposition = Disposition::Complete;
            view.meta = meta_view(head, meta);
        }
        GetHeadOutcome::NotModified { e_tag } => {
            view.disposition = Disposition::NotModified;
            view.meta.e_tag = maybe_span(head, *e_tag);
        }
        GetHeadOutcome::PreconditionFailed => view.disposition = Disposition::PreconditionFailed,
        GetHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound;
            view.failure = not_found_view(404, *kind);
        }
        GetHeadOutcome::RangeNotSatisfiable { object_size } => {
            view.disposition = Disposition::RangeNotSatisfiable;
            view.body.object_size = maybe_number(*object_size);
        }
        GetHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody;
            view.failure = failure_view(head, failure);
        }
        GetHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure;
            view.failure = failure_view(head, failure);
        }
        // The outcome is sealed, so a later version can add a variant. Report
        // one that this bridge does not know rather than guessing at it.
        _ => {}
    }
    view
}

fn put_outcome(head: Buffer, outcome: &PutHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(Operation::Write, Disposition::Unsupported);
    match outcome {
        PutHeadOutcome::Created { meta, .. } => {
            view.disposition = Disposition::Done;
            view.meta = meta_view(head, meta);
        }
        PutHeadOutcome::PreconditionFailed => view.disposition = Disposition::PreconditionFailed,
        PutHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound;
            view.failure = not_found_view(404, *kind);
        }
        PutHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody;
            view.failure = failure_view(head, failure);
        }
        PutHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure;
            view.failure = failure_view(head, failure);
        }
        _ => {}
    }
    view
}

fn delete_outcome(head: Buffer, outcome: &DeleteHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(Operation::Removal, Disposition::Unsupported);
    match outcome {
        // A removal returns no object, so Azure sends no metadata for one.
        DeleteHeadOutcome::Accepted => view.disposition = Disposition::Accepted,
        DeleteHeadOutcome::PreconditionFailed => view.disposition = Disposition::PreconditionFailed,
        DeleteHeadOutcome::NotFound { kind } => {
            view.disposition = Disposition::NotFound;
            view.failure = not_found_view(404, *kind);
        }
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            view.disposition = Disposition::NeedErrorBody;
            view.failure = failure_view(head, failure);
        }
        DeleteHeadOutcome::ServiceFailure(failure) => {
            view.disposition = Disposition::ServiceFailure;
            view.failure = failure_view(head, failure);
        }
        _ => {}
    }
    view
}

fn invalid(operation: Operation, error: &Error) -> Outcome {
    let mut view = empty_outcome(operation, Disposition::Invalid);
    view.error = status_of(error);
    view
}

fn empty_outcome(operation: Operation, disposition: Disposition) -> Outcome {
    Outcome {
        operation,
        disposition,
        meta: ObjectMetaView {
            size: absent_number(),
            e_tag: absent_span(),
            last_modified: absent_span(),
            version: absent_span(),
            content_encoding: absent_span(),
        },
        body: BodyWindowView {
            object_offset: 0,
            expected_len: absent_number(),
            object_size: absent_number(),
        },
        failure: FailureView {
            status: 0,
            category: FailureClassView::Other,
            kind: ServiceErrorKindView::None,
            request_id: absent_span(),
        },
        error: Status { code: 0, detail: 0 },
    }
}

fn meta_view(head: Buffer, meta: &ObjectMeta<'_>) -> ObjectMetaView {
    ObjectMetaView {
        size: maybe_number(meta.size),
        e_tag: maybe_span(head, meta.e_tag),
        last_modified: maybe_span(head, meta.last_modified),
        version: maybe_span(head, meta.version),
        content_encoding: maybe_span(head, meta.content_encoding),
    }
}

fn body_view(body: &BodyWindow) -> BodyWindowView {
    BodyWindowView {
        object_offset: body.object_offset,
        expected_len: maybe_number(body.expected_len),
        object_size: maybe_number(body.object_size),
    }
}

fn maybe_span(head: Buffer, value: Option<&[u8]>) -> MaybeSpan {
    match value.and_then(|value| head.span(value)) {
        Some(span) => MaybeSpan {
            present: true,
            span,
        },
        None => absent_span(),
    }
}

fn absent_span() -> MaybeSpan {
    MaybeSpan {
        present: false,
        span: Span { start: 0, len: 0 },
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

fn absent_number() -> MaybeU64 {
    MaybeU64 {
        present: false,
        value: 0,
    }
}

fn describe(outcome: &Outcome, head: &[u8], into: &mut [u8]) -> usize {
    let request_id = match outcome.failure.request_id.present {
        true => at(head, outcome.failure.request_id.span),
        false => None,
    };
    let failure = Failure {
        status: outcome.failure.status,
        class: class_of(outcome.failure.category),
        kind: kind_of(outcome.failure.kind),
        request_id,
    };
    let object_size = outcome
        .body
        .object_size
        .present
        .then_some(outcome.body.object_size.value);
    match outcome.disposition {
        Disposition::Invalid => describe_status(outcome.error, into),
        Disposition::InvalidInput => say(
            into,
            &"a response header names bytes outside the head that you passed",
        ),
        Disposition::Unsupported => say(
            into,
            &"the core crate returned an outcome that this bridge does not know",
        ),
        disposition => match outcome.operation {
            Operation::Write => say(into, &put_of(disposition, failure)),
            Operation::Removal => say(into, &delete_of(disposition, failure)),
            _ => say(into, &get_of(disposition, failure, object_size)),
        },
    }
}

// The core crate wrote the sentence for each of these. Rebuilding its outcome
// is how this bridge borrows that sentence instead of writing its own.
fn get_of<'h>(
    disposition: Disposition,
    failure: Failure<'h>,
    object_size: Option<u64>,
) -> GetHeadOutcome<'h> {
    match disposition {
        Disposition::Body => GetHeadOutcome::Body {
            meta: ObjectMeta::default(),
            body: BodyWindow {
                object_offset: 0,
                expected_len: None,
                object_size: None,
            },
        },
        Disposition::Complete => GetHeadOutcome::Complete {
            meta: ObjectMeta::default(),
        },
        Disposition::NotModified => GetHeadOutcome::NotModified { e_tag: None },
        Disposition::PreconditionFailed => GetHeadOutcome::PreconditionFailed,
        Disposition::NotFound => GetHeadOutcome::NotFound { kind: failure.kind },
        Disposition::RangeNotSatisfiable => GetHeadOutcome::RangeNotSatisfiable { object_size },
        Disposition::NeedErrorBody => GetHeadOutcome::NeedErrorBody(failure),
        _ => GetHeadOutcome::ServiceFailure(failure),
    }
}

fn put_of(disposition: Disposition, failure: Failure<'_>) -> PutHeadOutcome<'_> {
    match disposition {
        Disposition::Done => PutHeadOutcome::Created {
            meta: ObjectMeta::default(),
        },
        Disposition::PreconditionFailed => PutHeadOutcome::PreconditionFailed,
        Disposition::NotFound => PutHeadOutcome::NotFound { kind: failure.kind },
        Disposition::NeedErrorBody => PutHeadOutcome::NeedErrorBody(failure),
        _ => PutHeadOutcome::ServiceFailure(failure),
    }
}

fn delete_of(disposition: Disposition, failure: Failure<'_>) -> DeleteHeadOutcome<'_> {
    match disposition {
        Disposition::Accepted => DeleteHeadOutcome::Accepted,
        Disposition::PreconditionFailed => DeleteHeadOutcome::PreconditionFailed,
        Disposition::NotFound => DeleteHeadOutcome::NotFound { kind: failure.kind },
        Disposition::NeedErrorBody => DeleteHeadOutcome::NeedErrorBody(failure),
        _ => DeleteHeadOutcome::ServiceFailure(failure),
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
    use borink_object_storage::{ErrorCode, Mismatch, ProtocolFault};

    const HEAD: &[u8] = b"\"etag\"Wed, 26 Aug 2026 12:00:00 GMTversion-1gziprequest-123";

    fn e_tag() -> &'static [u8] {
        &HEAD[..6]
    }

    fn request_id() -> &'static [u8] {
        &HEAD[48..]
    }

    fn session() -> Box<Session> {
        open_session(
            b"https://account.blob.core.windows.net",
            b"container",
            b"token",
        )
    }

    fn text(outcome: &Outcome) -> String {
        let mut into = [0; 256];
        let length = describe(outcome, HEAD, &mut into);
        assert!(length <= into.len(), "{length}");
        String::from_utf8(into[..length].to_vec()).unwrap()
    }

    fn full_meta() -> ObjectMeta<'static> {
        ObjectMeta {
            size: Some(10),
            e_tag: Some(e_tag()),
            last_modified: Some(&HEAD[6..35]),
            version: Some(&HEAD[35..44]),
            content_encoding: Some(&HEAD[44..48]),
        }
    }

    fn every_failure() -> Vec<Failure<'static>> {
        let mut failures = Vec::new();
        for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
            for id in [None, Some(request_id())] {
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

    // Every value that the core crate returns has one twin, and the twin
    // carries every field of it.
    #[test]
    fn every_read_outcome_crosses_whole() {
        let head = Buffer::of(HEAD);
        let window = BodyWindow {
            object_offset: 2,
            expected_len: Some(4),
            object_size: Some(10),
        };
        let view = get_outcome(
            head,
            &GetHeadOutcome::Body {
                meta: full_meta(),
                body: window,
            },
        );
        assert_eq!(view.operation, Operation::Read);
        assert_eq!(view.disposition, Disposition::Body);
        assert_eq!(
            view.meta.size,
            MaybeU64 {
                present: true,
                value: 10
            }
        );
        assert_eq!(view.meta.e_tag.span, Span { start: 0, len: 6 });
        assert!(view.meta.last_modified.present);
        assert!(view.meta.version.present);
        assert!(view.meta.content_encoding.present);
        assert_eq!(view.body.object_offset, 2);
        assert_eq!(view.body.expected_len.value, 4);
        assert_eq!(view.body.object_size.value, 10);

        let empty = get_outcome(
            head,
            &GetHeadOutcome::Body {
                meta: ObjectMeta::default(),
                body: BodyWindow {
                    object_offset: 0,
                    expected_len: None,
                    object_size: None,
                },
            },
        );
        assert!(!empty.meta.size.present);
        assert!(!empty.meta.e_tag.present);
        assert!(!empty.body.expected_len.present);

        let complete = get_outcome(head, &GetHeadOutcome::Complete { meta: full_meta() });
        assert_eq!(complete.disposition, Disposition::Complete);
        assert!(complete.meta.e_tag.present);

        for e_tag in [None, Some(e_tag())] {
            let view = get_outcome(head, &GetHeadOutcome::NotModified { e_tag });
            assert_eq!(view.disposition, Disposition::NotModified);
            assert_eq!(view.meta.e_tag.present, e_tag.is_some());
        }

        assert_eq!(
            get_outcome(head, &GetHeadOutcome::PreconditionFailed).disposition,
            Disposition::PreconditionFailed
        );

        for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
            let view = get_outcome(head, &GetHeadOutcome::NotFound { kind });
            assert_eq!(view.disposition, Disposition::NotFound);
            assert_eq!(kind_of(view.failure.kind), kind);
        }

        for object_size in [None, Some(10)] {
            let view = get_outcome(head, &GetHeadOutcome::RangeNotSatisfiable { object_size });
            assert_eq!(view.disposition, Disposition::RangeNotSatisfiable);
            assert_eq!(view.body.object_size.present, object_size.is_some());
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
                let view = get_outcome(head, &outcome);
                assert_eq!(view.disposition, expected);
                assert_eq!(view.failure.status, failure.status);
                assert_eq!(class_of(view.failure.category), failure.class);
                assert_eq!(kind_of(view.failure.kind), failure.kind);
                assert_eq!(
                    view.failure.request_id.present,
                    failure.request_id.is_some()
                );
                assert_eq!(text(&view), outcome.to_string());
            }
        }
    }

    #[test]
    fn every_write_and_removal_outcome_crosses_whole() {
        let head = Buffer::of(HEAD);
        let created = put_outcome(head, &PutHeadOutcome::Created { meta: full_meta() });
        assert_eq!(created.operation, Operation::Write);
        assert_eq!(created.disposition, Disposition::Done);
        assert!(created.meta.e_tag.present);

        assert_eq!(
            delete_outcome(head, &DeleteHeadOutcome::Accepted).disposition,
            Disposition::Accepted
        );

        for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
            assert_eq!(
                kind_of(
                    put_outcome(head, &PutHeadOutcome::NotFound { kind })
                        .failure
                        .kind
                ),
                kind
            );
            assert_eq!(
                kind_of(
                    delete_outcome(head, &DeleteHeadOutcome::NotFound { kind })
                        .failure
                        .kind
                ),
                kind
            );
        }

        for failure in every_failure() {
            for outcome in [
                PutHeadOutcome::NeedErrorBody(failure),
                PutHeadOutcome::ServiceFailure(failure),
                PutHeadOutcome::PreconditionFailed,
            ] {
                assert_eq!(text(&put_outcome(head, &outcome)), outcome.to_string());
            }
            for outcome in [
                DeleteHeadOutcome::NeedErrorBody(failure),
                DeleteHeadOutcome::ServiceFailure(failure),
                DeleteHeadOutcome::PreconditionFailed,
            ] {
                assert_eq!(text(&delete_outcome(head, &outcome)), outcome.to_string());
            }
        }
    }

    // The sentence for a read is the core crate's own, for every disposition.
    #[test]
    fn a_read_is_described_in_the_words_of_the_core_crate() {
        let head = Buffer::of(HEAD);
        for outcome in [
            GetHeadOutcome::Body {
                meta: full_meta(),
                body: BodyWindow {
                    object_offset: 0,
                    expected_len: None,
                    object_size: None,
                },
            },
            GetHeadOutcome::Complete { meta: full_meta() },
            GetHeadOutcome::NotModified { e_tag: None },
            GetHeadOutcome::PreconditionFailed,
            GetHeadOutcome::NotFound { kind: None },
            GetHeadOutcome::NotFound {
                kind: Some(ServiceErrorKind::NoSuchContainer),
            },
            GetHeadOutcome::RangeNotSatisfiable {
                object_size: Some(10),
            },
            GetHeadOutcome::RangeNotSatisfiable { object_size: None },
        ] {
            assert_eq!(text(&get_outcome(head, &outcome)), outcome.to_string());
        }
    }

    // Both vocabularies cross by discriminant and come back the same value.
    #[test]
    fn the_vocabularies_are_mirrored_one_to_one() {
        for detail in 1..=u16::MAX {
            if let Some(kind) = ServiceErrorKind::from_discriminant(detail) {
                assert_eq!(kind_of(kind_view(Some(kind))), Some(kind), "{kind:?}");
            }
            if let Some(class) = FailureClass::from_discriminant(detail) {
                assert_eq!(class_of(class_view(class)), class, "{class:?}");
            }
        }
        assert_eq!(kind_of(kind_view(None)), None);
        for method in [Method::Get, Method::Head, Method::Put, Method::Delete] {
            let core = borink_object_storage::Method::from_discriminant(method.repr).unwrap();
            assert_eq!(method_view(core), method);
        }
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
        assert_eq!(checked, 3 + 6 + 10 + 7);
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
        let shape = GetShapeView {
            kind: GetKindView::Bytes,
            range: RangeView {
                form: RangeForm::Whole,
                start: 0,
                end: 0,
            },
            condition: ConditionView::None,
        };
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
                form: RangeForm::Bounded,
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

    // A field that names bytes outside the head is the caller's mistake.
    // Dropping it would turn a named failure into one that asks for a body.
    #[test]
    fn a_header_field_outside_the_head_stops_the_call() {
        let session = session();
        let shape = PutShapeView {
            condition: ConditionView::None,
        };
        let outside = [HeaderField {
            name: Span { start: 0, len: 6 },
            value: Span {
                start: HEAD.len(),
                len: 4,
            },
        }];
        let outcome = session.accept_put_head(&shape, 500, HEAD, &outside);
        assert_eq!(outcome.disposition, Disposition::InvalidInput);
        assert_eq!(outcome.operation, Operation::Write);
        assert!(text(&outcome).contains("outside the head"));
    }

    // The head asked for the error body, and the body names the error.
    #[test]
    fn the_error_body_finishes_what_the_head_left_open() {
        let session = session();
        let shape = PutShapeView {
            condition: ConditionView::None,
        };
        let fields = [HeaderField {
            name: Span { start: 44, len: 4 },
            value: Span { start: 48, len: 11 },
        }];
        // `gzip: request-123` is not a header the core crate reads, so the
        // head names no error and asks for the body.
        let outcome = session.accept_put_head(&shape, 409, HEAD, &fields);
        assert_eq!(outcome.disposition, Disposition::NeedErrorBody);

        let finished = session.finish_put_error_body(
            &outcome.failure,
            HEAD,
            b"<Error><Code>BlobAlreadyExists</Code></Error>",
        );
        assert_eq!(finished.disposition, Disposition::ServiceFailure);
        assert_eq!(
            kind_of(finished.failure.kind),
            Some(ServiceErrorKind::AlreadyExists)
        );
        assert!(text(&finished).contains("already exists"));

        // A body that never arrived leaves the outcome final and unnamed.
        let unnamed = session.finish_put_error_body(&outcome.failure, HEAD, b"");
        assert_eq!(unnamed.disposition, Disposition::ServiceFailure);
        assert_eq!(kind_of(unnamed.failure.kind), None);
    }

    // A head that does not answer the plan is a `Status`, not a sentence.
    #[test]
    fn an_invalid_head_carries_the_error_of_the_core_crate() {
        let session = session();
        let shape = PutShapeView {
            condition: ConditionView::None,
        };
        let outcome = session.accept_put_head(&shape, 412, HEAD, &[]);
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
        assert_eq!(
            open_session(b"account.example", b"container", b"token")
                .status()
                .code,
            ErrorCode::InvalidEndpoint as u16
        );
        assert_eq!(
            open_session(b"https://account.example", b"", b"token")
                .status()
                .code,
            ErrorCode::InvalidContainer as u16
        );
        assert_eq!(
            open_session(b"https://account.example", b"container", b"")
                .status()
                .code,
            ErrorCode::InvalidToken as u16
        );
        assert_eq!(
            open_session(b"\xff", b"container", b"token").status().code,
            ErrorCode::InvalidEndpoint as u16
        );
        assert_eq!(session().status().code, 0);
    }

    // A sentence longer than the buffer is counted, not cut off silently.
    #[test]
    fn a_short_buffer_still_learns_the_length_of_the_sentence() {
        let outcome = get_outcome(
            Buffer::of(HEAD),
            &GetHeadOutcome::ServiceFailure(Failure {
                status: 503,
                class: FailureClass::Server,
                kind: None,
                request_id: Some(request_id()),
            }),
        );
        let mut small = [0; 4];
        let length = describe(&outcome, HEAD, &mut small);
        assert!(length > small.len());
        let mut whole = vec![0; length];
        assert_eq!(describe(&outcome, HEAD, &mut whole), length);
    }
}
