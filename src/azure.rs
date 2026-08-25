use core::ops::Range;

use crate::request::{U64Decimal, Writer, text};
use crate::{
    CapacityError, ConditionKind, Error, GetKind, GetShape, InvalidPlan, ObjectMeta, PhysicalGet,
    RequestedRange, Response, Result, Timestamps, WireRequest,
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

    /// Validates successful response metadata before the host reads the body.
    ///
    /// The plan the response answers is passed back in, so the interpretation
    /// cannot disagree with the request: the host re-states nothing.
    pub fn interpret_get<'response>(
        &self,
        response: Response<'response>,
        shape: GetShape,
    ) -> Result<ObjectMeta<'response>> {
        match response.status() {
            200..=299 => Ok(ObjectMeta {
                size: response_size(&response, shape)?,
                e_tag: response.header("etag"),
                version: response.header("x-ms-version-id"),
            }),
            404 => Err(Error::NotFound),
            401 | 403 => Err(Error::Unauthorized),
            304 => Err(Error::NotModified),
            412 => Err(Error::Precondition),
            416 => Err(Error::RangeNotSatisfiable),
            status => Err(Error::Status(status)),
        }
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

fn response_size(response: &Response<'_>, shape: GetShape) -> Result<u64> {
    if shape.range != RequestedRange::Whole {
        return response
            .header("content-range")
            .and_then(|value| value.rsplit_once('/'))
            .and_then(|(_, total)| total.parse().ok())
            .ok_or(Error::Protocol("invalid or missing content-range"));
    }
    response
        .header("content-length")
        .ok_or(Error::Protocol("response has no object size"))?
        .parse()
        .map_err(|_| Error::Protocol("invalid content-length"))
}

fn valid_header(value: &[u8]) -> bool {
    !value.is_empty() && value.is_ascii() && !value.iter().any(u8::is_ascii_control)
}
