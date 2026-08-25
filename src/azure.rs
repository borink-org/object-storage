use core::ops::Range;

use crate::request::{U64Decimal, Writer, text};
use crate::{
    BodyWindow, CapacityError, ConditionKind, Error, FailureClass, GetHead, GetHeadOutcome,
    GetKind, GetShape, InvalidPlan, ObjectMeta, PhysicalGet, RequestedRange, Result, Timestamps,
    WireRequest,
};

/// Latest Azure Storage version fully deployed in every region.
///
/// See the [Azure Storage service version lifecycle](https://learn.microsoft.com/en-us/rest/api/storageservices/versioning-for-the-azure-storage-services).
pub const VERSION: &str = "2026-04-06";

// Azure limits blob names to 1,024 characters.
const MAX_BLOB_NAME_CHARS: usize = 1024;

/// Borrowed Azure Blob endpoint and container configuration.
#[derive(Debug, Clone, Copy)]
pub struct Container<'a> {
    endpoint: &'a str,
    name: &'a str,
}

impl<'a> Container<'a> {
    /// Validates and borrows an HTTP(S) origin and container name.
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

/// Azure Blob operations authorized by a borrowed bearer token.
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
    /// Validates and borrows a container and bearer token.
    pub fn new(container: Container<'a>, token: &'a str) -> Result<Self> {
        if !valid_header(token.as_bytes()) {
            return Err(Error::InvalidToken);
        }
        Ok(Self { container, token })
    }

    /// Lowers a plan into a request head written into `buf`.
    ///
    /// The plan is validated exhaustively before any byte is written, so
    /// [`Error::InvalidPlan`] is never confused with [`Error::Capacity`]. A
    /// capacity refusal reports the exact requirement; the host may grow its
    /// storage and call again, or measure first with
    /// [`layered::requirements`](crate::layered::requirements).
    ///
    /// A sans-I/O core cannot read the clock, so `now` is explicit. It is
    /// copied into `buf` like every other head byte and may be a temporary.
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
        Ok(WireRequest::new(
            match get.shape.kind {
                GetKind::Bytes => "GET",
                GetKind::Metadata => "HEAD",
            },
            text(&bytes[..layout.url_end]),
            text(&bytes[layout.url_end..layout.authorization_end]),
            text(&bytes[layout.authorization_end..layout.date_end]),
            VERSION,
            layout.range.map(|span| text(&bytes[span])),
            layout
                .condition
                .map(|(name, span)| (name, text(&bytes[span]))),
        ))
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
        let range = match get.shape.range {
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
        let condition = condition_header(get.shape.condition_kind).map(|name| {
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

    /// Interprets a response head against the plan it answers.
    ///
    /// The plan is passed back in, so the interpretation cannot disagree with
    /// the request: the host re-states nothing the library already knows.
    ///
    /// Every head Azure actually sends maps to a [`GetHeadOutcome`]. `Err` is
    /// reserved for heads that are unparseable, self-contradictory, or that
    /// contradict `shape`. Azure names its errors in `x-ms-error-code`, so the
    /// head is always decisive here and no body continuation is needed.
    pub fn accept_get_head<'h>(
        &self,
        shape: GetShape,
        head: GetHead<'h>,
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
            304 if shape.condition_kind != ConditionKind::IfNoneMatch => Err(Error::Protocol(
                "304 answered a plan without an If-None-Match condition",
            )),
            304 => Ok(GetHeadOutcome::NotModified { etag: head.etag }),
            412 if shape.condition_kind != ConditionKind::IfMatch => Err(Error::Protocol(
                "412 answered a plan without an If-Match condition",
            )),
            412 => Ok(GetHeadOutcome::PreconditionFailed),
            404 => Ok(GetHeadOutcome::NotFound),
            416 => Ok(GetHeadOutcome::RangeNotSatisfiable {
                object_size: match head.content_range.map(parse_content_range) {
                    None => None,
                    // `bytes */N` is the only form 416 may carry.
                    Some(Some(ContentRange::Unsatisfied { total })) => total,
                    Some(_) => return Err(Error::Protocol("invalid 416 content-range")),
                },
            }),
            200..=299 => Err(Error::Protocol("unexpected success status")),
            status => Ok(GetHeadOutcome::ServiceFailure {
                status,
                class: failure_class(status),
                request_id: head.request_id,
            }),
        }
    }
}

fn accept_success<'h>(shape: GetShape, head: GetHead<'h>) -> Result<GetHeadOutcome<'h>> {
    let content_length = decimal_header(head.content_length)?;
    let meta = |size| ObjectMeta {
        size,
        e_tag: head.etag,
        last_modified: head.last_modified,
        version: head.version,
        content_encoding: head.content_encoding,
    };
    if head.status == 200 {
        // An unranged plan reads from byte zero, and Azure states the whole
        // object length, so `Content-Length` is both the window and the size.
        return Ok(match shape.kind {
            GetKind::Metadata => GetHeadOutcome::Complete(meta(content_length)),
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
                "Azure served less than the satisfiable range",
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

fn failure_class(status: u16) -> FailureClass {
    match status {
        300..=399 => FailureClass::Redirect,
        401 | 403 => FailureClass::Auth,
        408 | 429 => FailureClass::Throttled,
        500..=599 => FailureClass::Server,
        _ => FailureClass::Other,
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
    match get.shape.range {
        RequestedRange::Bounded { start, end } if start >= end => {
            return Err(InvalidPlan::Range.into());
        }
        RequestedRange::Suffix(_) => return Err(InvalidPlan::UnsupportedRange.into()),
        RequestedRange::Whole => {}
        _ if get.shape.kind == GetKind::Metadata => {
            return Err(InvalidPlan::RangedMetadata.into());
        }
        _ => {}
    }
    // The kind and the value must agree in both directions: a kind without a
    // value cannot be encoded, and a value without a kind would be dropped.
    match (get.shape.condition_kind, get.condition_value) {
        (ConditionKind::None, None) => Ok(()),
        (ConditionKind::IfMatch | ConditionKind::IfNoneMatch, Some(value))
            if valid_header(value) =>
        {
            Ok(())
        }
        _ => Err(InvalidPlan::Condition.into()),
    }
}

fn valid_header(value: &[u8]) -> bool {
    !value.is_empty() && value.is_ascii() && !value.iter().any(u8::is_ascii_control)
}
