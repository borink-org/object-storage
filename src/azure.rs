use crate::request::{Digits, Writer, text};
use crate::{
    CapacityError, Error, GetCondition, GetOptions, GetRange, ObjectMeta, Request,
    RequestRequirements, RequestWorkspace, Response, Result, Timestamps, WorkspaceExtent,
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
        if !valid_header(token) {
            return Err(Error::InvalidToken);
        }
        Ok(Self { container, token })
    }

    /// Builds a GET or HEAD request in `workspace`.
    ///
    /// A capacity error reports the exact packed extent size; the host may grow
    /// that extent and retry the same call.
    pub fn get_request<'request>(
        &self,
        workspace: &'request mut RequestWorkspace<'_>,
        key: &str,
        options: &GetOptions<'_>,
        now: &'request Timestamps,
    ) -> Result<Request<'request>> {
        validate_get(key, options)?;
        let available = workspace.capacity();
        // The storing writer keeps counting after capacity is exhausted. One
        // pass therefore produces either the request or its exact requirement.
        let mut out = Writer::storing(workspace.bytes());
        let layout = self.build(&mut out, key, options);
        let required = out.position();
        if required > available {
            return Err(CapacityError {
                extent: WorkspaceExtent::Packed,
                required,
                available,
            }
            .into());
        }
        let bytes = out.finish().expect("capacity was checked");
        Ok(Request::new(
            if options.head { "HEAD" } else { "GET" },
            text(&bytes[..layout.url_end]),
            text(&bytes[layout.url_end..layout.authorization_end]),
            now.rfc1123(),
            VERSION,
            layout.range.map(|span| text(&bytes[span])),
            layout
                .condition
                .map(|(name, span)| (name, text(&bytes[span]))),
        ))
    }

    /// Measures the packed extent required by [`Self::get_request`].
    pub fn get_request_requirements(
        &self,
        key: &str,
        options: &GetOptions<'_>,
    ) -> Result<RequestRequirements> {
        validate_get(key, options)?;
        let mut out = Writer::counting();
        self.build(&mut out, key, options);
        Ok(RequestRequirements {
            packed: out.position(),
        })
    }

    fn build(&self, out: &mut Writer<'_>, key: &str, options: &GetOptions<'_>) -> Layout {
        out.push(self.container.endpoint);
        out.push("/");
        out.push(self.container.name);
        out.push("/");
        for part in crate::path::encode_object_key(key) {
            out.push(part);
        }
        let url_end = out.position();
        out.push("Bearer ");
        out.push(self.token);
        let authorization_end = out.position();
        let range = options.range.as_ref().map(|range| {
            let start = out.position();
            out.push("bytes=");
            match range {
                GetRange::Bounded(range) => {
                    out.push(Digits::new(range.start).as_str());
                    out.push("-");
                    out.push(Digits::new(range.end - 1).as_str());
                }
                GetRange::Offset(start) => {
                    out.push(Digits::new(*start).as_str());
                    out.push("-");
                }
                GetRange::Suffix(_) => unreachable!("range was validated"),
            }
            start..out.position()
        });
        let condition = match options.condition {
            GetCondition::None => None,
            GetCondition::IfMatch(value) => Some(("if-match", value)),
            GetCondition::IfNoneMatch(value) => Some(("if-none-match", value)),
        }
        .map(|(name, value)| {
            let start = out.position();
            out.push(value);
            (name, start..out.position())
        });
        Layout {
            url_end,
            authorization_end,
            range,
            condition,
        }
    }

    /// Validates successful response metadata before the host reads the body.
    pub fn interpret_get<'response>(
        &self,
        response: Response<'response>,
        options: &GetOptions<'_>,
    ) -> Result<ObjectMeta<'response>> {
        match response.status() {
            200..=299 => Ok(ObjectMeta {
                size: response_size(&response, options)?,
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
    range: Option<core::ops::Range<usize>>,
    condition: Option<(&'static str, core::ops::Range<usize>)>,
}

fn validate_get(key: &str, options: &GetOptions<'_>) -> Result<()> {
    if key.is_empty() || key.chars().count() > MAX_BLOB_NAME_CHARS {
        return Err(Error::InvalidKey);
    }
    match &options.range {
        Some(GetRange::Bounded(range)) if range.start >= range.end => {
            return Err(Error::InvalidRange);
        }
        Some(GetRange::Suffix(_)) => return Err(Error::Unsupported("Azure suffix ranges")),
        _ => {}
    }
    let condition = match options.condition {
        GetCondition::None => None,
        GetCondition::IfMatch(value) | GetCondition::IfNoneMatch(value) => Some(value),
    };
    if condition.is_some_and(|value| !valid_header(value)) {
        return Err(Error::InvalidCondition);
    }
    Ok(())
}

fn response_size(response: &Response<'_>, options: &GetOptions<'_>) -> Result<u64> {
    if options.range.is_some() {
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

fn valid_header(value: &str) -> bool {
    !value.is_empty() && value.is_ascii() && !value.bytes().any(|byte| byte.is_ascii_control())
}
