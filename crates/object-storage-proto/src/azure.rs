use crate::request::{HeadWriter, U64Decimal, Writer};
use crate::{
    BodyWindow, CapacityError, Classification, ConditionKind, DeleteHeadOutcome, DeleteKind,
    DeleteShape, Error, Failure, FailureClass, Fill, GetHeadOutcome, GetKind, GetShape,
    InvalidPlan, ListEntry, ListHeadOutcome, Method, ObjectMeta, Payload, PhysicalDelete,
    PhysicalGet, PhysicalList, PhysicalPut, PutHeadOutcome, PutShape, RequestedRange,
    ResponseFault, ResponseHead, Result, Resume, ServiceErrorKind, Timestamps, WireRequest,
};

/// The most recent Azure Storage version that every region supports.
///
/// See the [Azure Storage service version lifecycle](https://learn.microsoft.com/en-us/rest/api/storageservices/versioning-for-the-azure-storage-services).
pub const VERSION: &str = "2026-04-06";

// Azure limits blob names to 1,024 characters.
const MAX_BLOB_NAME_CHARS: usize = 1024;

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
/// This is a small borrowed value. Create it again whenever the token
/// changes.
///
/// Every method that encodes a request takes the current time in `now`,
/// because this crate never reads the clock.
#[derive(Clone, Copy)]
pub struct Blobs<'a> {
    container: Container<'a>,
    token: &'a str,
}

impl core::fmt::Debug for Blobs<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Blobs")
            .field("container", &self.container)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl<'a> Blobs<'a> {
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
        Ok(Self { container, token })
    }

    /// Writes the request head for `get` into `buf`.
    ///
    /// This method allocates nothing. It writes the URL and the header values
    /// into `buf`, and returns a [`WireRequest`] that borrows them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `get` cannot become an Azure
    /// request. This method validates the plan before it writes any byte, so
    /// it never reports an invalid plan as a capacity error.
    ///
    /// Returns [`Error::Capacity`] if `buf` is too small. The error states the
    /// exact number of bytes that the head needs. Grow `buf` and call this
    /// method again, or call
    /// [`layered::get_requirements`](crate::layered::get_requirements) first.
    pub fn encode_get<'r>(
        &self,
        buf: &'r mut [u8],
        get: &PhysicalGet<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_get(get)?;
        let available = buf.len();
        let mut head = HeadWriter::new(buf);
        self.build(&mut head, Some(get.key), &[], get.range, now);
        push_condition(&mut head, get.condition, get.condition_value);
        let method = match get.kind {
            GetKind::Bytes => Method::Get,
            GetKind::Metadata => Method::Head,
        };
        encoded(head, available, method, Payload::Slice(&[]))
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
    /// Returns [`Error::Capacity`] if `buf` is too small. The error states the
    /// exact number of bytes that the head needs. Grow `buf` and call this
    /// method again, or call
    /// [`layered::put_requirements`](crate::layered::put_requirements) first.
    pub fn encode_put<'r>(
        &self,
        buf: &'r mut [u8],
        put: &PhysicalPut<'_>,
        content: Payload<'r>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_put(put, content.len())?;
        let available = buf.len();
        let length = content.len();
        let mut head = HeadWriter::new(buf);
        self.build(&mut head, Some(put.key), &[], RequestedRange::Whole, now);
        head.header("x-ms-blob-type", |out| out.push(b"BlockBlob"));
        // The content length is head bytes like any other, so it is written
        // into the caller's buffer rather than formatted at send time.
        head.header("content-length", |out| {
            out.push(U64Decimal::new(length).as_bytes());
        });
        push_condition(&mut head, put.condition, put.condition_value);
        encoded(head, available, Method::Put, content)
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
                    out.push(part.as_bytes());
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
    /// [`Failure`](crate::Failure), and the body that you read. The body names
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
    /// Returns [`Error::Capacity`] if `buf` is too small. The error states the
    /// exact number of bytes that the head needs. Grow `buf` and call this
    /// method again, or call
    /// [`layered::delete_requirements`](crate::layered::delete_requirements)
    /// first.
    pub fn encode_delete<'r>(
        &self,
        buf: &'r mut [u8],
        delete: &PhysicalDelete<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_delete(delete)?;
        let available = buf.len();
        let mut head = HeadWriter::new(buf);
        self.build(&mut head, Some(delete.key), &[], RequestedRange::Whole, now);
        if let Some(value) = delete_snapshots(delete.kind) {
            head.header("x-ms-delete-snapshots", |out| out.push(value.as_bytes()));
        }
        push_condition(&mut head, delete.condition, delete.condition_value);
        encoded(head, available, Method::Delete, Payload::Slice(&[]))
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
                    last_modified: head.last_modified,
                    version: head.version,
                    content_encoding: head.content_encoding,
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
    /// Returns [`Error::Capacity`] if `buf` is too small. The error states the
    /// exact number of bytes that the head needs. Grow `buf` and call this
    /// method again, or call
    /// [`layered::list_requirements`](crate::layered::list_requirements)
    /// first.
    pub fn encode_list<'r>(
        &self,
        buf: &'r mut [u8],
        list: &PhysicalList<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_list(list)?;
        let available = buf.len();
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
                .map(|marker| ("marker", QueryValue::Encoded(marker))),
            list.max_results
                .map(|max_results| ("maxresults", QueryValue::Number(max_results))),
        ];

        let mut head = HeadWriter::new(buf);
        self.build(&mut head, None, &query, RequestedRange::Whole, now);
        encoded(head, available, Method::Get, Payload::Slice(&[]))
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
    /// Your array is the budget. A page that does not fit fills the array and
    /// returns [`Fill::Partial`], which carries the [`Resume`] that reads the
    /// rest with [`Self::resume_listing`]; no entry is lost or read twice. An
    /// array of `max_results` entries always holds the whole page.
    ///
    /// Only [`Fill::Page`] reports a marker, so continuing on the marker
    /// cannot step over entries that were not read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a listing page. This reads the grammar that Azure writes, not XML
    /// at large: a namespace prefix, a reference to an entity no listing
    /// declares, an entry tag spelled with an attribute, and anything else
    /// Azure does not write are refused rather than guessed at.
    pub fn fill_listing<'b>(
        &self,
        body: &'b mut [u8],
        into: &mut [ListEntry<'b>],
    ) -> Result<Fill<'b>> {
        crate::xml::fill_listing(body, into)
    }

    /// Reads the rest of a page that [`Fill::Partial`] stopped in.
    ///
    /// Pass the same `body`, unchanged, and the [`Resume`] that came with the
    /// entries you have finished with. Reading continues from where it
    /// stopped, so no entry is read twice and none is lost.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] as
    /// [`Self::fill_listing`] does. A [`Resume`] describes one body: keep each
    /// with the buffer it came from.
    pub fn resume_listing<'b>(
        &self,
        body: &'b mut [u8],
        resume: Resume,
        into: &mut [ListEntry<'b>],
    ) -> Result<Fill<'b>> {
        crate::xml::resume_listing(body, resume, into)
    }
}

// Both providers group keys at `/` and at nothing else, so the delimiter is
// what a plan turns on rather than a byte that it carries.
const DELIMITER: &[u8] = b"/";

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
                    out.push(part.as_bytes());
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

/// Reads the Azure error code from a failed response body.
///
/// Azure names the error in the `x-ms-error-code` header, and repeats it in an
/// XML body. [`Blobs::accept_get_head`] already reads the header, so call this
/// only when the outcome names no error. This function reads the header first
/// and falls back to the body. It allocates nothing and keeps nothing.
///
/// Set `truncated` if your read limit cut `body` short. The result then
/// separates a body that stopped early from a complete body that names a code
/// this crate does not recognize.
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
    let meta = |size| ObjectMeta {
        size,
        e_tag: head.e_tag,
        last_modified: head.last_modified,
        version: head.version,
        content_encoding: head.content_encoding,
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
    let served = end - start + 1;
    if content_length.is_some_and(|length| length != served) {
        return Err(ResponseFault::Head.into());
    }
    // Azure serves the whole satisfiable range, so a short serve is a
    // mismatch: silently accepting it would hand consumers a partial read.
    let requested_start = match shape.range {
        RequestedRange::Bounded { start, .. } | RequestedRange::Offset(start) => start,
        RequestedRange::Whole | RequestedRange::Suffix(_) => {
            unreachable!("an unranged plan cannot reach a 206")
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

// `bytes S-E/T`, with `*` allowed for either the range or the total. All
// arithmetic on the parsed values is checked by construction: S <= E < T.
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
    if start > end || total.is_some_and(|total| end >= total) {
        return None;
    }
    Some(ContentRange::Satisfied { start, end, total })
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
fn encoded<'r>(
    head: HeadWriter<'r>,
    available: usize,
    method: Method,
    payload: Payload<'r>,
) -> Result<WireRequest<'r>> {
    let required = head.position();
    head.finish(method, payload)
        .ok_or(Error::Capacity(CapacityError {
            required,
            available,
        }))
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

fn validate_get(get: &PhysicalGet<'_>) -> Result<()> {
    if get.key.is_empty() || get.key.chars().count() > MAX_BLOB_NAME_CHARS {
        return Err(InvalidPlan::Key.into());
    }
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

// Azure writes at most 5000 MiB of content in one Put Blob request. This is a
// `u64` because it does not fit a 32-bit `usize`.
const MAX_PUT_LEN: u64 = 5000 * 1024 * 1024;

fn validate_put(put: &PhysicalPut<'_>, len: u64) -> Result<()> {
    if put.key.is_empty() || put.key.chars().count() > MAX_BLOB_NAME_CHARS {
        return Err(InvalidPlan::Key.into());
    }
    if len > MAX_PUT_LEN {
        return Err(InvalidPlan::PayloadTooLarge.into());
    }
    validate_condition(put.condition, put.condition_value)
}

fn validate_list(list: &PhysicalList<'_>) -> Result<()> {
    // A prefix is the start of a key, so it is bounded like one. An empty
    // prefix lists the whole container and is valid.
    if list.prefix.chars().count() > MAX_BLOB_NAME_CHARS {
        return Err(InvalidPlan::Prefix.into());
    }
    if list.marker.is_some_and(<[u8]>::is_empty) {
        return Err(InvalidPlan::Marker.into());
    }
    if list.max_results == Some(0) {
        return Err(InvalidPlan::MaxResults.into());
    }
    Ok(())
}

fn validate_delete(delete: &PhysicalDelete<'_>) -> Result<()> {
    if delete.key.is_empty() || delete.key.chars().count() > MAX_BLOB_NAME_CHARS {
        return Err(InvalidPlan::Key.into());
    }
    validate_condition(delete.condition, delete.condition_value)
}

fn valid_header(value: &[u8]) -> bool {
    !value.is_empty() && value.is_ascii() && !value.iter().any(u8::is_ascii_control)
}
