//! A synchronous `ureq` host for `borink-object-storage`.
//!
//! [`azure::Blobs`] sends Azure requests, and [`s3::Objects`] sends S3
//! requests that it signs. Each gets, puts, deletes and lists in the same
//! way: it writes the template of each kind of request once, when it is
//! made, and writes every request into one buffer that it keeps.

pub mod azure;
pub mod s3;

// Error bodies are diagnostics, so this host caps what it will read for one.
const MAX_ERROR_BODY: u64 = 8 * 1024;

// A page is a document that this host holds whole, so it caps that too.
const MAX_PAGE: u64 = 8 * 1024 * 1024;

use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage_proto::{Error, Payload, ResponseHead, Timestamps, WireRequest};

// The current time, which the crate never reads itself.
pub(crate) fn now() -> Result<Timestamps, Box<dyn std::error::Error>> {
    Ok(Timestamps::from_unix(unix_now()?))
}

// The current time in seconds since the Unix epoch.
pub(crate) fn unix_now() -> Result<u64, Box<dyn std::error::Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

// Writes a template or a request with `write` into `buf`, and grows the
// buffer and writes again only when it was too small. What `write` returns
// borrows nothing, a template or a request, so a host that
// keeps `buf` between calls writes each later one once.
pub(crate) fn grown_to_fit<T>(
    buf: &mut Vec<u8>,
    write: impl Fn(&mut [u8]) -> Result<T, Error>,
) -> Result<T, Error> {
    match write(buf) {
        Err(Error::Capacity(capacity)) => {
            buf.resize(capacity.required, 0);
            write(buf)
        }
        written => written,
    }
}

// A template, and the bytes it was written into.
pub(crate) struct Prepared<T> {
    pub(crate) template: T,
    pub(crate) bytes: Vec<u8>,
}

impl<T> Prepared<T> {
    // Writes a template with `prepare`, into a buffer of its size.
    pub(crate) fn new(prepare: impl Fn(&mut [u8]) -> Result<T, Error>) -> Result<Self, Error> {
        let mut bytes = Vec::new();
        let template = grown_to_fit(&mut bytes, prepare)?;
        Ok(Self { template, bytes })
    }
}

// The agent that a client sends every request with, which keeps its
// connections. This host returns the stored bytes of an object and never
// decompresses them: see the `ureq` dependency in Cargo.toml, which turns
// the decoding off.
pub(crate) fn new_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

// Sends `request`, written into `buf`, with `agent` and with `content` for
// a write, and hands back the response head and its body as two values, so
// that the head can be read while the body is read too.
pub(crate) fn send(
    agent: &ureq::Agent,
    request: &WireRequest,
    buf: &[u8],
    content: Option<&[u8]>,
) -> Result<(ureq::http::response::Parts, ureq::Body), ureq::Error> {
    let mut builder = ureq::http::Request::builder()
        .method(request.method().as_str())
        .uri(request.url(buf));
    for (name, value) in request.headers(buf) {
        builder = builder.header(name, value);
    }
    let response = match (request.payload(buf), content) {
        (Payload::Slice(bytes), _) if !bytes.is_empty() => agent.run(builder.body(bytes)?)?,
        (_, Some(bytes)) => agent.run(builder.body(bytes)?)?,
        _ => agent.run(builder.body(())?)?,
    };
    Ok(response.into_parts())
}

// The head of a response, as the crate reads it.
pub(crate) fn response_head(parts: &ureq::http::response::Parts) -> ResponseHead<'_> {
    ResponseHead::from_headers(
        parts.status.as_u16(),
        parts
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    )
}

// Reads a page of a listing whole, up to `MAX_PAGE` bytes.
pub(crate) fn page_body(body: &mut ureq::Body) -> Result<Vec<u8>, ureq::Error> {
    body.with_config().limit(MAX_PAGE).read_to_vec()
}

// Reads at most `MAX_ERROR_BODY` bytes of an error body. An error body that
// does not arrive costs the name of the error, not the outcome.
pub(crate) fn error_body(body: &mut ureq::Body) -> Vec<u8> {
    body.with_config()
        .limit(MAX_ERROR_BODY)
        .read_to_vec()
        .unwrap_or_default()
}
