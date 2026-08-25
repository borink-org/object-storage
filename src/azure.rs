use core::ops::Range;

use crate::request::{U64Decimal, Writer, text};
use crate::{
    BodyWindow, CapacityError, Classification, ConditionKind, Error, FailureClass, GetHeadOutcome,
    GetKind, GetShape, InvalidPlan, ObjectMeta, Payload, PhysicalGet, PhysicalPut, PutHeadOutcome,
    PutShape, RequestedRange, ResponseHead, Result, ServiceErrorKind, Timestamps, WireRequest,
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
    /// This crate performs no I/O and cannot read the clock, so pass the
    /// current time in `now`. This method copies the date into `buf` with the
    /// rest of the head, so `now` can be a temporary.
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
        let mut out = Writer::new(buf);
        let layout = self.build(&mut out, get, now);
        let required = out.position();
        let bytes = out.finish().ok_or(CapacityError {
            required,
            available,
        })?;
        let method = match get.kind {
            GetKind::Bytes => "GET",
            GetKind::Metadata => "HEAD",
        };
        let mut request =
            WireRequest::new(method, text(&bytes[..layout.url_end]), Payload::Slice(&[]));
        self.push_common(&mut request, bytes, &layout);
        if let Some(span) = layout.range {
            request.push("range", text(&bytes[span]));
        }
        if let Some((name, span)) = layout.condition {
            request.push(name, text(&bytes[span]));
        }
        Ok(request)
    }

    /// Writes the request head for `put` into `buf`.
    ///
    /// The head states the length of `content`. If you pass
    /// [`Payload::Slice`], the returned request borrows those bytes and copies
    /// none of them. If you pass [`Payload::Streamed`], the request carries no
    /// content and you send the stated number of bytes yourself.
    ///
    /// This crate performs no I/O and cannot read the clock, so pass the
    /// current time in `now`. This method copies the date into `buf` with the
    /// rest of the head, so `now` can be a temporary.
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
        let mut out = Writer::new(buf);
        let layout = self.build(
            &mut out,
            &PhysicalGet {
                key: put.key,
                kind: GetKind::Bytes,
                range: RequestedRange::Whole,
                condition: put.condition,
                condition_value: put.condition_value,
            },
            now,
        );
        // The content length is head bytes like any other, so it is written
        // into the caller's buffer rather than formatted at send time.
        let length_start = out.position();
        out.push(U64Decimal::new(content.len()).as_bytes());
        let length_end = out.position();
        let required = out.position();
        let bytes = out.finish().ok_or(CapacityError {
            required,
            available,
        })?;
        let mut request = WireRequest::new("PUT", text(&bytes[..layout.url_end]), content);
        self.push_common(&mut request, bytes, &layout);
        request.push("x-ms-blob-type", "BlockBlob");
        request.push("content-length", text(&bytes[length_start..length_end]));
        if let Some((name, span)) = layout.condition {
            request.push(name, text(&bytes[span]));
        }
        Ok(request)
    }

    fn push_common<'r>(&self, request: &mut WireRequest<'r>, bytes: &'r [u8], layout: &Layout) {
        request.push(
            "authorization",
            text(&bytes[layout.url_end..layout.authorization_end]),
        );
        request.push(
            "x-ms-date",
            text(&bytes[layout.authorization_end..layout.date_end]),
        );
        request.push("x-ms-version", VERSION);
    }

    fn build(&self, out: &mut Writer<'_>, get: &PhysicalGet<'_>, now: &Timestamps) -> Layout {
        out.push(self.container.endpoint.as_bytes());
        out.push(b"/");
        out.push(self.container.name.as_bytes());
        out.push(b"/");
        for part in crate::path::encode_object_key(get.key) {
            out.push(part.as_bytes());
        }
        let url_end = out.position();
        out.push(b"Bearer ");
        out.push(self.token.as_bytes());
        let authorization_end = out.position();
        out.push(now.rfc1123().as_bytes());
        let date_end = out.position();
        let range = match get.range {
            RequestedRange::Whole => None,
            range => {
                let start = out.position();
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
                Some(start..out.position())
            }
        };
        let condition = condition_header(get.condition).map(|name| {
            let start = out.position();
            out.push(get.condition_value.expect("the plan was validated"));
            (name, start..out.position())
        });
        Layout {
            url_end,
            authorization_end,
            date_end,
            range,
            condition,
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
    /// Returns [`Error::Protocol`] if the head is invalid or contradicts
    /// itself, such as a `Content-Range` whose end is before its start.
    ///
    /// Returns [`Error::ResponseMismatch`] if the head does not answer
    /// `shape`, such as a ranged plan that Azure answers with status 200, or a
    /// range that Azure serves only in part.
    pub fn accept_get_head<'h>(
        &self,
        shape: GetShape,
        head: ResponseHead<'h>,
    ) -> Result<GetHeadOutcome<'h>> {
        let ranged = shape.range != RequestedRange::Whole;
        match head.status {
            206 if !ranged => Err(Error::ResponseMismatch(
                "an unranged plan was answered with 206",
            )),
            200 if ranged => Err(Error::ResponseMismatch(
                "a ranged plan was answered without 206",
            )),
            200 | 206 => accept_success(shape, head),
            // A conditional status the plan did not ask for is a contradiction,
            // not an outcome: nothing in the plan explains it.
            304 if shape.condition != ConditionKind::IfNoneMatch => Err(Error::Protocol(
                "304 answered a plan without an If-None-Match condition",
            )),
            304 => Ok(GetHeadOutcome::NotModified { e_tag: head.e_tag }),
            412 if shape.condition != ConditionKind::IfMatch => Err(Error::Protocol(
                "412 answered a plan without an If-Match condition",
            )),
            412 => Ok(GetHeadOutcome::PreconditionFailed),
            // Azure repeats the header's code in the body, so only a
            // missing header is worth a body read. A header naming a code
            // this crate does not know is already decisive.
            404 if head.error_code.is_none() => Ok(need_error_body(404, head)),
            404 => Ok(GetHeadOutcome::NotFound {
                kind: kind_for_code(trim_ascii(head.error_code.unwrap_or_default())),
            }),
            416 => Ok(GetHeadOutcome::RangeNotSatisfiable {
                object_size: match head.content_range.map(parse_content_range) {
                    None => None,
                    // `bytes */N` is the only form 416 may carry.
                    Some(Some(ContentRange::Unsatisfied { total })) => total,
                    Some(_) => return Err(Error::Protocol("invalid 416 content-range")),
                },
            }),
            200..=299 => Err(Error::Protocol("unexpected success status")),
            status if head.error_code.is_none() => Ok(need_error_body(status, head)),
            status => {
                let kind = kind_for_code(trim_ascii(head.error_code.unwrap_or_default()));
                Ok(GetHeadOutcome::ServiceFailure {
                    status,
                    class: failure_class(status, kind),
                    kind,
                    request_id: head.request_id,
                })
            }
        }
    }

    /// Finishes a [`GetHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// The body names the error, exactly as the `x-ms-error-code` header would
    /// have. Pass an empty body if you could not read one: the outcome is then
    /// final with the error unnamed.
    ///
    /// Every other outcome is already final, and this method returns it
    /// unchanged.
    ///
    /// To tell a body that your read limit cut short from a body that names an
    /// error this crate does not recognize, call [`classify_error`] instead.
    pub fn accept_error_body<'h>(
        &self,
        outcome: GetHeadOutcome<'h>,
        body: &[u8],
    ) -> GetHeadOutcome<'h> {
        let GetHeadOutcome::NeedErrorBody {
            status, request_id, ..
        } = outcome
        else {
            return outcome;
        };
        let kind = crate::xml::error_code(body).and_then(|code| kind_for_code(code.as_bytes()));
        match status {
            404 => GetHeadOutcome::NotFound { kind },
            // The body's code refines the category too, exactly as the
            // header's would have.
            status => GetHeadOutcome::ServiceFailure {
                status,
                class: failure_class(status, kind),
                kind,
                request_id,
            },
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
    /// Returns [`Error::Protocol`] if the head is invalid, such as a success
    /// status that a write never returns.
    ///
    /// Returns [`Error::ResponseMismatch`] if the head does not answer
    /// `shape`, such as a failed condition on a write that carried none.
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
            412 if shape.condition == ConditionKind::None => Err(Error::ResponseMismatch(
                "412 answered a write without a condition",
            )),
            412 => Ok(PutHeadOutcome::PreconditionFailed),
            404 if head.error_code.is_none() => Ok(put_need_error_body(404, head)),
            404 => Ok(PutHeadOutcome::NotFound {
                kind: kind_for_code(trim_ascii(head.error_code.unwrap_or_default())),
            }),
            200..=299 => Err(Error::Protocol("a write returns 201, not another success")),
            status if head.error_code.is_none() => Ok(put_need_error_body(status, head)),
            status => {
                let kind = kind_for_code(trim_ascii(head.error_code.unwrap_or_default()));
                Ok(PutHeadOutcome::ServiceFailure {
                    status,
                    class: failure_class(status, kind),
                    kind,
                    request_id: head.request_id,
                })
            }
        }
    }

    /// Finishes a [`PutHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_error_body`] for a write, and reads the body the
    /// same way.
    pub fn accept_put_error_body<'h>(
        &self,
        outcome: PutHeadOutcome<'h>,
        body: &[u8],
    ) -> PutHeadOutcome<'h> {
        let PutHeadOutcome::NeedErrorBody {
            status, request_id, ..
        } = outcome
        else {
            return outcome;
        };
        let kind = crate::xml::error_code(body).and_then(|code| kind_for_code(code.as_bytes()));
        match status {
            404 => PutHeadOutcome::NotFound { kind },
            status => PutHeadOutcome::ServiceFailure {
                status,
                class: failure_class(status, kind),
                kind,
                request_id,
            },
        }
    }
}

fn put_need_error_body<'h>(status: u16, head: ResponseHead<'h>) -> PutHeadOutcome<'h> {
    PutHeadOutcome::NeedErrorBody {
        status,
        class: failure_class(status, None),
        request_id: head.request_id,
    }
}

fn need_error_body<'h>(status: u16, head: ResponseHead<'h>) -> GetHeadOutcome<'h> {
    GetHeadOutcome::NeedErrorBody {
        status,
        class: failure_class(status, None),
        request_id: head.request_id,
    }
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
        .ok_or(Error::ResponseMismatch("206 without a content-range"))?;
    let ContentRange::Satisfied { start, end, total } =
        parse_content_range(value).ok_or(Error::Protocol("invalid content-range"))?
    else {
        return Err(Error::Protocol("bytes */N is valid only in a 416"));
    };
    let served = end - start + 1;
    if content_length.is_some_and(|length| length != served) {
        return Err(Error::Protocol(
            "content-length disagrees with content-range",
        ));
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
        return Err(Error::ResponseMismatch("the served range starts elsewhere"));
    }
    if let Some(total) = total {
        let satisfiable = match shape.range {
            RequestedRange::Bounded { end, .. } => end.min(total),
            _ => total,
        };
        if end + 1 != satisfiable {
            return Err(Error::ResponseMismatch(
                "the service served less than the satisfiable range",
            ));
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
            .ok_or(Error::Protocol("invalid content-length")),
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

struct Layout {
    url_end: usize,
    authorization_end: usize,
    date_end: usize,
    range: Option<Range<usize>>,
    condition: Option<(&'static str, Range<usize>)>,
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

fn valid_header(value: &[u8]) -> bool {
    !value.is_empty() && value.is_ascii() && !value.iter().any(u8::is_ascii_control)
}
