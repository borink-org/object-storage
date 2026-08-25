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
//! One allocation per session, in [`open_session`], for the endpoint, the
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
//! `RequestHead`, which names the URL and each header by offset and length. A
//! few header names and values are constants of the core crate rather than
//! bytes of the head. Those are copied into the reserve at the end of your
//! buffer, so one buffer holds the whole request and you read every part of it
//! the same way.
//!
//! Call `encode_get` with an empty buffer to learn the size: it reports
//! `PlanOutcome::NeedsBuffer` and the exact number of bytes. Size the buffer
//! once per client and reuse it.
//!
//! A response works the same way in reverse. You keep the response head in
//! your own bytes and name each header with a `HeaderField`. The outcome then
//! names each metadata value by offset and length into those same bytes.
//!
//! # Examples
//!
//! ```cpp
//! rust::Box<borink::Session> session = borink::open_session(endpoint, container, token);
//! if (session->fault() != borink::SessionFault::None) { /* ... */ }
//!
//! borink::RequestHead head = session->encode_get(key, {buffer.data(), buffer.size()}, now);
//! if (head.outcome == borink::PlanOutcome::NeedsBuffer) {
//!     buffer.resize(head.required);
//!     head = session->encode_get(key, {buffer.data(), buffer.size()}, now);
//! }
//! // ... send head.url and head.headers with your HTTP client ...
//!
//! borink::GetOutcome outcome = session->accept_get_head(key, status, collected, fields);
//! if (outcome.disposition == borink::GetDisposition::Body) {
//!     // ... read the body ...
//! }
//! ```

use std::fmt::{self, Write as _};

use borink_object_storage::{
    Blobs, BodyWindow, Container, DeleteHeadOutcome, Error, GetHeadOutcome, InvalidPlan,
    ObjectMeta, Payload, PhysicalDelete, PhysicalGet, PhysicalPut, PutHeadOutcome, ResponseHead,
    Timestamps, WireRequest, layered,
};

use ffi::{
    BodyWindowView, GetDisposition, GetOutcome, HeaderField, MaybeSpan, MaybeU64, Method,
    ObjectMetaView, PlanOutcome, RequestHead, RequestHeader, SessionFault, Span, WriteDisposition,
    WriteOutcome,
};

/// The most headers that one request head carries.
///
/// The core crate writes at most six. The two spare slots leave room for a
/// header that a later version adds.
const MAX_HEADERS: usize = 8;

/// The bytes reserved at the end of the request buffer.
///
/// Header names, and the few header values that are constants of the core
/// crate, are copied here so that every part of the request is a range of one
/// buffer. The longest set of them is well under this.
const RESERVE: usize = 256;

/// One container, and the token that opens it.
///
/// Build one per client. It holds the only memory that this bridge owns.
pub struct Session {
    endpoint: String,
    container: String,
    token: String,
    fault: SessionFault,
}

#[cxx::bridge(namespace = "borink")]
mod ffi {
    /// A range of bytes, as an offset from the start of a buffer.
    struct Span {
        /// The offset of the first byte.
        start: usize,
        /// The number of bytes.
        len: usize,
    }

    /// A range that the response head may not carry.
    struct MaybeSpan {
        /// Whether the head carried this value.
        present: bool,
        /// The range of the head that holds it.
        span: Span,
    }

    /// A number that the response head may not carry.
    struct MaybeU64 {
        /// Whether the head carried this number.
        present: bool,
        /// The number.
        value: u64,
    }

    /// What is wrong with a session.
    enum SessionFault {
        /// Nothing. The session can build requests.
        None,
        /// The endpoint is not an ASCII HTTP or HTTPS origin.
        Endpoint,
        /// The container name is empty, or it holds bytes that would change
        /// the structure of the request.
        Container,
        /// The token is not usable as one HTTP header value.
        Token,
        /// One of the three is not text.
        NotText,
    }

    /// The HTTP method of a request.
    enum Method {
        /// `GET`.
        Get,
        /// `HEAD`.
        Head,
        /// `PUT`.
        Put,
        /// `DELETE`.
        Delete,
    }

    /// Whether the request head was written, and why not.
    enum PlanOutcome {
        /// The head is in your buffer.
        Written,
        /// Your buffer is too small. Grow it to `required` and call again.
        NeedsBuffer,
        /// The session cannot build requests. Read `Session::fault`.
        InvalidSession,
        /// The object key is empty, too long, or not text.
        InvalidKey,
        /// The content is longer than one request can carry.
        ContentTooLarge,
        /// The request cannot be built for another reason.
        InvalidRequest,
        /// The request has more headers than this bridge carries.
        TooManyHeaders,
    }

    /// One request header, as two ranges of the request buffer.
    struct RequestHeader {
        /// The range that holds the header name.
        name: Span,
        /// The range that holds the header value.
        value: Span,
    }

    /// A request head, as ranges of the buffer that holds it.
    struct RequestHead {
        /// Whether the head was written.
        outcome: PlanOutcome,
        /// The number of bytes that this request head needs.
        ///
        /// This is the exact size, whether or not the head was written. Size
        /// one buffer by it and reuse that buffer.
        required: usize,
        /// The HTTP method.
        method: Method,
        /// The range that holds the complete object URL.
        url: Span,
        /// How many of `headers` this request uses.
        header_count: usize,
        /// The headers, in the order that the core crate wrote them.
        headers: [RequestHeader; 8],
    }

    /// One response header, as two ranges of the bytes that you kept.
    struct HeaderField {
        /// The range that holds the header name.
        name: Span,
        /// The range that holds the header value.
        value: Span,
    }

    /// Object metadata, as ranges of the response head.
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
    struct BodyWindowView {
        /// The offset in the object of the first byte of the response body.
        object_offset: u64,
        /// The exact length of the response body.
        expected_len: MaybeU64,
        /// The size of the whole object.
        object_size: MaybeU64,
    }

    /// What a read response tells you to do.
    enum GetDisposition {
        /// A body follows. Read it.
        Body,
        /// No body follows and the request is complete.
        Complete,
        /// The `If-None-Match` condition held, so Azure sent no body.
        NotModified,
        /// The `If-Match` condition did not hold, so Azure sent no body.
        PreconditionFailed,
        /// The object does not exist, or its container does not.
        NotFound,
        /// Azure cannot serve the requested range.
        RangeNotSatisfiable,
        /// The head reports a failure but names no error.
        ///
        /// Read the response body, cap what you read, and pass it to
        /// `describe_get`, which names the error.
        NeedErrorBody,
        /// Azure refused the request, or it failed to serve it.
        ServiceFailure,
        /// The head is invalid, or it does not answer this request.
        ///
        /// `describe_get` states what is wrong with it.
        Invalid,
    }

    /// What a write or a removal response tells you to do.
    enum WriteDisposition {
        /// Azure carried out the write or the removal.
        Done,
        /// The condition did not hold, so Azure changed nothing.
        PreconditionFailed,
        /// The object or its container does not exist.
        NotFound,
        /// The head reports a failure but names no error.
        ///
        /// Read the response body, cap what you read, and pass it to
        /// `describe_put` or `describe_delete`, which names the error.
        NeedErrorBody,
        /// Azure refused the request, or it failed to carry it out.
        ServiceFailure,
        /// The head is invalid, or it does not answer this request.
        Invalid,
    }

    /// The result of reading the response head of a read.
    struct GetOutcome {
        /// What to do with the response.
        disposition: GetDisposition,
        /// The metadata from the head.
        meta: ObjectMetaView,
        /// Where the bytes of the body belong.
        body: BodyWindowView,
    }

    /// The result of reading the response head of a write or a removal.
    struct WriteOutcome {
        /// What to do with the response.
        disposition: WriteDisposition,
        /// The metadata from the head.
        meta: ObjectMetaView,
    }

    extern "Rust" {
        type Session;

        /// Opens a session against one container.
        ///
        /// This is the one call that allocates. It copies the three values, so
        /// none of them has to outlive the call. A value that is not usable
        /// leaves the session with a `fault`, and every request refuses.
        fn open_session(endpoint: &[u8], container: &[u8], token: &[u8]) -> Box<Session>;

        /// Returns what is wrong with this session, if anything.
        fn fault(self: &Session) -> SessionFault;

        /// Writes the request head of a read into `buf`.
        ///
        /// Pass an empty buffer to learn the size that this request needs.
        fn encode_get(self: &Session, key: &[u8], buf: &mut [u8], unix_seconds: u64)
        -> RequestHead;

        /// Writes the request head of a write into `buf`.
        ///
        /// The head states `content_len`. You send those bytes yourself.
        fn encode_put(
            self: &Session,
            key: &[u8],
            buf: &mut [u8],
            content_len: u64,
            unix_seconds: u64,
        ) -> RequestHead;

        /// Writes the request head of a removal into `buf`.
        fn encode_delete(
            self: &Session,
            key: &[u8],
            buf: &mut [u8],
            unix_seconds: u64,
        ) -> RequestHead;

        /// Reads the response head of a read.
        ///
        /// `head` holds the response header bytes, and each `HeaderField`
        /// names one header inside it. The outcome points back into `head`.
        fn accept_get_head(
            self: &Session,
            key: &[u8],
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
        ) -> GetOutcome;

        /// Reads the response head of a write.
        fn accept_put_head(
            self: &Session,
            key: &[u8],
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
        ) -> WriteOutcome;

        /// Reads the response head of a removal.
        fn accept_delete_head(
            self: &Session,
            key: &[u8],
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
        ) -> WriteOutcome;

        /// Writes one sentence naming what a read returned instead of the
        /// object.
        ///
        /// Pass the error body if you read one, and an empty body if you did
        /// not. Returns the length of the sentence, which may be longer than
        /// `into`: the part that fits is written, and nothing is allocated.
        fn describe_get(
            self: &Session,
            key: &[u8],
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
            body: &[u8],
            into: &mut [u8],
        ) -> usize;

        /// Writes one sentence naming why Azure stored no object.
        fn describe_put(
            self: &Session,
            key: &[u8],
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
            body: &[u8],
            into: &mut [u8],
        ) -> usize;

        /// Writes one sentence naming why Azure removed no object.
        fn describe_delete(
            self: &Session,
            key: &[u8],
            status: u16,
            head: &[u8],
            fields: &[HeaderField],
            body: &[u8],
            into: &mut [u8],
        ) -> usize;
    }
}

fn open_session(endpoint: &[u8], container: &[u8], token: &[u8]) -> Box<Session> {
    let (endpoint, container, token) = match (
        std::str::from_utf8(endpoint),
        std::str::from_utf8(container),
        std::str::from_utf8(token),
    ) {
        (Ok(endpoint), Ok(container), Ok(token)) => (endpoint, container, token),
        _ => return Box::new(Session::faulted(SessionFault::NotText)),
    };
    let session = Session {
        endpoint: endpoint.to_owned(),
        container: container.to_owned(),
        token: token.to_owned(),
        fault: SessionFault::None,
    };
    // Refuse an unusable session here, rather than once per request.
    match session.blobs() {
        Ok(_) => Box::new(session),
        Err(Error::InvalidEndpoint) => Box::new(Session::faulted(SessionFault::Endpoint)),
        Err(Error::InvalidContainer) => Box::new(Session::faulted(SessionFault::Container)),
        Err(_) => Box::new(Session::faulted(SessionFault::Token)),
    }
}

impl Session {
    fn faulted(fault: SessionFault) -> Self {
        Self {
            endpoint: String::new(),
            container: String::new(),
            token: String::new(),
            fault,
        }
    }

    fn fault(&self) -> SessionFault {
        self.fault
    }

    fn blobs(&self) -> Result<Blobs<'_>, Error> {
        Blobs::new(
            Container::new(&self.endpoint, &self.container)?,
            &self.token,
        )
    }

    fn encode_get(&self, key: &[u8], buf: &mut [u8], unix_seconds: u64) -> RequestHead {
        let (key, blobs) = match self.plan(key) {
            Ok(planned) => planned,
            Err(outcome) => return refused(outcome, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let get = PhysicalGet::new(key);
        let required = match layered::get_requirements(&blobs, &get, &now) {
            Ok(required) => required + RESERVE,
            Err(error) => return refused(plan_outcome(&error), 0),
        };
        let Some((whole, head, reserve)) = split(buf, required) else {
            return refused(PlanOutcome::NeedsBuffer, required);
        };
        match blobs.encode_get(head, &get, &now) {
            Ok(request) => describe_request(whole, reserve, required, &request),
            Err(error) => refused(plan_outcome(&error), required),
        }
    }

    fn encode_put(
        &self,
        key: &[u8],
        buf: &mut [u8],
        content_len: u64,
        unix_seconds: u64,
    ) -> RequestHead {
        let (key, blobs) = match self.plan(key) {
            Ok(planned) => planned,
            Err(outcome) => return refused(outcome, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let put = PhysicalPut::new(key);
        // The content stays in C++. Only its length reaches the head, so the
        // request borrows no content and you send the bytes yourself.
        let content = Payload::Streamed { len: content_len };
        let required = match layered::put_requirements(&blobs, &put, content, &now) {
            Ok(required) => required + RESERVE,
            Err(error) => return refused(plan_outcome(&error), 0),
        };
        let Some((whole, head, reserve)) = split(buf, required) else {
            return refused(PlanOutcome::NeedsBuffer, required);
        };
        match blobs.encode_put(head, &put, content, &now) {
            Ok(request) => describe_request(whole, reserve, required, &request),
            Err(error) => refused(plan_outcome(&error), required),
        }
    }

    fn encode_delete(&self, key: &[u8], buf: &mut [u8], unix_seconds: u64) -> RequestHead {
        let (key, blobs) = match self.plan(key) {
            Ok(planned) => planned,
            Err(outcome) => return refused(outcome, 0),
        };
        let now = Timestamps::from_unix(unix_seconds);
        let delete = PhysicalDelete::new(key);
        let required = match layered::delete_requirements(&blobs, &delete, &now) {
            Ok(required) => required + RESERVE,
            Err(error) => return refused(plan_outcome(&error), 0),
        };
        let Some((whole, head, reserve)) = split(buf, required) else {
            return refused(PlanOutcome::NeedsBuffer, required);
        };
        match blobs.encode_delete(head, &delete, &now) {
            Ok(request) => describe_request(whole, reserve, required, &request),
            Err(error) => refused(plan_outcome(&error), required),
        }
    }

    fn accept_get_head(
        &self,
        key: &[u8],
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
    ) -> GetOutcome {
        match self.read_get(key, status, head, fields) {
            Ok(outcome) => get_outcome(Buffer::of(head), &outcome),
            Err(_) => GetOutcome {
                disposition: GetDisposition::Invalid,
                meta: empty_meta(),
                body: empty_body(),
            },
        }
    }

    fn accept_put_head(
        &self,
        key: &[u8],
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
    ) -> WriteOutcome {
        match self.read_put(key, status, head, fields) {
            Ok(outcome) => put_outcome(Buffer::of(head), &outcome),
            Err(_) => invalid_write(),
        }
    }

    fn accept_delete_head(
        &self,
        key: &[u8],
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
    ) -> WriteOutcome {
        match self.read_delete(key, status, head, fields) {
            Ok(outcome) => delete_outcome(&outcome),
            Err(_) => invalid_write(),
        }
    }

    // The three functions below read the head a second time. An outcome
    // borrows the header bytes, so it cannot cross the bridge and come back.
    // Reading a head again costs nothing: it moves no bytes and opens nothing.
    fn describe_get(
        &self,
        key: &[u8],
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
        body: &[u8],
        into: &mut [u8],
    ) -> usize {
        let blobs = match self.blobs() {
            Ok(blobs) => blobs,
            Err(error) => return say(into, &error),
        };
        match self.read_get(key, status, head, fields) {
            Ok(outcome) => say(into, &blobs.accept_error_body(outcome, body)),
            Err(error) => say(into, &error),
        }
    }

    fn describe_put(
        &self,
        key: &[u8],
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
        body: &[u8],
        into: &mut [u8],
    ) -> usize {
        let blobs = match self.blobs() {
            Ok(blobs) => blobs,
            Err(error) => return say(into, &error),
        };
        match self.read_put(key, status, head, fields) {
            Ok(outcome) => say(into, &blobs.accept_put_error_body(outcome, body)),
            Err(error) => say(into, &error),
        }
    }

    fn describe_delete(
        &self,
        key: &[u8],
        status: u16,
        head: &[u8],
        fields: &[HeaderField],
        body: &[u8],
        into: &mut [u8],
    ) -> usize {
        let blobs = match self.blobs() {
            Ok(blobs) => blobs,
            Err(error) => return say(into, &error),
        };
        match self.read_delete(key, status, head, fields) {
            Ok(outcome) => say(into, &blobs.accept_delete_error_body(outcome, body)),
            Err(error) => say(into, &error),
        }
    }
}

// What every request does before the core crate sees it, and what every
// response does before it is read.
impl Session {
    // Reads the key as text and builds the requests of this session.
    fn plan<'s, 'k>(&'s self, key: &'k [u8]) -> Result<(&'k str, Blobs<'s>), PlanOutcome> {
        let Ok(key) = std::str::from_utf8(key) else {
            return Err(PlanOutcome::InvalidKey);
        };
        match self.blobs() {
            Ok(blobs) => Ok((key, blobs)),
            Err(error) => Err(plan_outcome(&error)),
        }
    }

    fn read_get<'h>(
        &self,
        key: &[u8],
        status: u16,
        head: &'h [u8],
        fields: &[HeaderField],
    ) -> Result<GetHeadOutcome<'h>, Error> {
        let key = std::str::from_utf8(key).map_err(|_| Error::InvalidPlan(InvalidPlan::Key))?;
        self.blobs()?.accept_get_head(
            PhysicalGet::new(key).shape(),
            response_head(status, head, fields),
        )
    }

    fn read_put<'h>(
        &self,
        key: &[u8],
        status: u16,
        head: &'h [u8],
        fields: &[HeaderField],
    ) -> Result<PutHeadOutcome<'h>, Error> {
        let key = std::str::from_utf8(key).map_err(|_| Error::InvalidPlan(InvalidPlan::Key))?;
        self.blobs()?.accept_put_head(
            PhysicalPut::new(key).shape(),
            response_head(status, head, fields),
        )
    }

    fn read_delete<'h>(
        &self,
        key: &[u8],
        status: u16,
        head: &'h [u8],
        fields: &[HeaderField],
    ) -> Result<DeleteHeadOutcome<'h>, Error> {
        let key = std::str::from_utf8(key).map_err(|_| Error::InvalidPlan(InvalidPlan::Key))?;
        self.blobs()?.accept_delete_head(
            PhysicalDelete::new(key).shape(),
            response_head(status, head, fields),
        )
    }
}

// Splits the request buffer into the part that the core crate writes and the
// reserve that holds what the head does not.
fn split(buf: &mut [u8], required: usize) -> Option<(Buffer, &mut [u8], Reserve<'_>)> {
    if buf.len() < required {
        return None;
    }
    let whole = Buffer::of(buf);
    let offset = required - RESERVE;
    let (head, bytes) = buf.split_at_mut(offset);
    Some((
        whole,
        head,
        Reserve {
            offset,
            bytes,
            used: 0,
        },
    ))
}

fn refused(outcome: PlanOutcome, required: usize) -> RequestHead {
    RequestHead {
        outcome,
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

// Where a buffer starts and how long it is. A request borrows the buffer that
// it is written into, so its address and length are read before that call.
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

    // Where `part` sits in this buffer, if it sits in it at all. Addresses
    // give that range without copying a byte.
    fn span(&self, part: &[u8]) -> Option<Span> {
        let start = part.as_ptr() as usize;
        let end = start.checked_add(part.len())?;
        (start >= self.base && end <= self.base + self.len).then(|| Span {
            start: start - self.base,
            len: part.len(),
        })
    }
}

// The end of the request buffer, where the parts of a request that are not
// bytes of the head are copied.
struct Reserve<'a> {
    offset: usize,
    bytes: &'a mut [u8],
    used: usize,
}

impl Reserve<'_> {
    fn push(&mut self, value: &str) -> Option<Span> {
        let end = self.used + value.len();
        if end > self.bytes.len() {
            return None;
        }
        self.bytes[self.used..end].copy_from_slice(value.as_bytes());
        let span = Span {
            start: self.offset + self.used,
            len: value.len(),
        };
        self.used = end;
        Some(span)
    }
}

// A header name, and a value such as the service version, is a constant of the
// core crate rather than a byte of the head. Copying those into the reserve
// makes every part of the request one range of one buffer.
fn place(whole: Buffer, reserve: &mut Reserve<'_>, part: &str) -> Option<Span> {
    whole.span(part.as_bytes()).or_else(|| reserve.push(part))
}

fn describe_request(
    whole: Buffer,
    mut reserve: Reserve<'_>,
    required: usize,
    request: &WireRequest<'_>,
) -> RequestHead {
    if request.headers().len() > MAX_HEADERS {
        return refused(PlanOutcome::TooManyHeaders, required);
    }
    let mut headers = empty_headers();
    for (slot, (name, value)) in headers.iter_mut().zip(request.headers()) {
        let (Some(name), Some(value)) = (
            place(whole, &mut reserve, name),
            place(whole, &mut reserve, value),
        ) else {
            return refused(PlanOutcome::NeedsBuffer, required);
        };
        slot.name = name;
        slot.value = value;
    }
    let (Some(method), Some(url)) = (
        method(request.method()),
        place(whole, &mut reserve, request.url()),
    ) else {
        return refused(PlanOutcome::InvalidRequest, required);
    };
    RequestHead {
        outcome: PlanOutcome::Written,
        required,
        method,
        url,
        header_count: request.headers().len(),
        headers,
    }
}

fn method(name: &str) -> Option<Method> {
    match name {
        "GET" => Some(Method::Get),
        "HEAD" => Some(Method::Head),
        "PUT" => Some(Method::Put),
        "DELETE" => Some(Method::Delete),
        _ => None,
    }
}

fn plan_outcome(error: &Error) -> PlanOutcome {
    match error {
        Error::InvalidEndpoint | Error::InvalidContainer | Error::InvalidToken => {
            PlanOutcome::InvalidSession
        }
        Error::InvalidPlan(InvalidPlan::Key) => PlanOutcome::InvalidKey,
        Error::InvalidPlan(InvalidPlan::PayloadTooLarge) => PlanOutcome::ContentTooLarge,
        Error::Capacity(_) => PlanOutcome::NeedsBuffer,
        _ => PlanOutcome::InvalidRequest,
    }
}

// Reads the head that the host collected. A field whose range falls outside
// `head`, or whose name is not text, is skipped: the core crate reads header
// names as text, and neither of those can be a name that it looks for.
fn response_head<'h>(status: u16, head: &'h [u8], fields: &[HeaderField]) -> ResponseHead<'h> {
    ResponseHead::from_headers(
        status,
        fields.iter().filter_map(move |field| {
            let name = head.get(field.name.start..field.name.start.checked_add(field.name.len)?)?;
            let value =
                head.get(field.value.start..field.value.start.checked_add(field.value.len)?)?;
            Some((std::str::from_utf8(name).ok()?, value))
        }),
    )
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

fn get_outcome(head: Buffer, outcome: &GetHeadOutcome<'_>) -> GetOutcome {
    let mut view = GetOutcome {
        disposition: GetDisposition::ServiceFailure,
        meta: empty_meta(),
        body: empty_body(),
    };
    match outcome {
        GetHeadOutcome::Body { meta, body, .. } => {
            view.disposition = GetDisposition::Body;
            view.meta = meta_view(head, meta);
            view.body = body_view(body);
        }
        GetHeadOutcome::Complete { meta } => {
            view.disposition = GetDisposition::Complete;
            view.meta = meta_view(head, meta);
        }
        GetHeadOutcome::NotModified { e_tag } => {
            view.disposition = GetDisposition::NotModified;
            view.meta.e_tag = maybe_span(head, *e_tag);
        }
        GetHeadOutcome::PreconditionFailed => view.disposition = GetDisposition::PreconditionFailed,
        GetHeadOutcome::NotFound { .. } => view.disposition = GetDisposition::NotFound,
        GetHeadOutcome::RangeNotSatisfiable { object_size } => {
            view.disposition = GetDisposition::RangeNotSatisfiable;
            view.body.object_size = maybe_number(*object_size);
        }
        GetHeadOutcome::NeedErrorBody { .. } => view.disposition = GetDisposition::NeedErrorBody,
        GetHeadOutcome::ServiceFailure { .. } => view.disposition = GetDisposition::ServiceFailure,
        // The outcome is sealed, so a later version can add a variant. Report
        // one that this bridge does not know as a failure, not as an object.
        _ => view.disposition = GetDisposition::ServiceFailure,
    }
    view
}

fn put_outcome(head: Buffer, outcome: &PutHeadOutcome<'_>) -> WriteOutcome {
    let mut view = WriteOutcome {
        disposition: WriteDisposition::ServiceFailure,
        meta: empty_meta(),
    };
    match outcome {
        PutHeadOutcome::Created { meta, .. } => {
            view.disposition = WriteDisposition::Done;
            view.meta = meta_view(head, meta);
        }
        PutHeadOutcome::PreconditionFailed => {
            view.disposition = WriteDisposition::PreconditionFailed;
        }
        PutHeadOutcome::NotFound { .. } => view.disposition = WriteDisposition::NotFound,
        PutHeadOutcome::NeedErrorBody { .. } => view.disposition = WriteDisposition::NeedErrorBody,
        PutHeadOutcome::ServiceFailure { .. } => {
            view.disposition = WriteDisposition::ServiceFailure
        }
        _ => view.disposition = WriteDisposition::ServiceFailure,
    }
    view
}

fn delete_outcome(outcome: &DeleteHeadOutcome<'_>) -> WriteOutcome {
    WriteOutcome {
        disposition: match outcome {
            DeleteHeadOutcome::Accepted => WriteDisposition::Done,
            DeleteHeadOutcome::PreconditionFailed => WriteDisposition::PreconditionFailed,
            DeleteHeadOutcome::NotFound { .. } => WriteDisposition::NotFound,
            DeleteHeadOutcome::NeedErrorBody { .. } => WriteDisposition::NeedErrorBody,
            DeleteHeadOutcome::ServiceFailure { .. } => WriteDisposition::ServiceFailure,
            _ => WriteDisposition::ServiceFailure,
        },
        // A removal returns no object, so Azure sends no metadata for one.
        meta: empty_meta(),
    }
}

fn invalid_write() -> WriteOutcome {
    WriteOutcome {
        disposition: WriteDisposition::Invalid,
        meta: empty_meta(),
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

fn empty_meta() -> ObjectMetaView {
    ObjectMetaView {
        size: absent_number(),
        e_tag: absent_span(),
        last_modified: absent_span(),
        version: absent_span(),
        content_encoding: absent_span(),
    }
}

fn empty_body() -> BodyWindowView {
    BodyWindowView {
        object_offset: 0,
        expected_len: absent_number(),
        object_size: absent_number(),
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
