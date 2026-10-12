//! A synchronous `ureq` host for `borink-object-storage`.
//!
//! [`azure`] sends Azure requests, and [`s3`] sends S3 requests that it
//! signs. Each module gets, puts, deletes and lists in the same way, from
//! templates.

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
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(Timestamps::from_unix(unix))
}

// The bytes that `encode` needs: it refuses an empty buffer with the
// number.
pub(crate) fn required<T>(
    encode: impl FnOnce(&mut [u8]) -> Result<T, Error>,
) -> Result<usize, Error> {
    match encode(&mut []) {
        Err(Error::Capacity(capacity)) => Ok(capacity.required),
        Err(error) => Err(error),
        Ok(_) => Ok(0),
    }
}

// Writes a template with `prepare` into a buffer of exactly its size, and
// returns both.
pub(crate) fn template<T>(
    prepare: impl Fn(&mut [u8]) -> Result<T, Error>,
) -> Result<(T, Vec<u8>), Error> {
    let mut bytes = vec![0; required(&prepare)?];
    let template = prepare(&mut bytes)?;
    Ok((template, bytes))
}

// Sends `request`, with `content` for a write, and hands back the response
// head and its body as two values, so that the head can be read while the
// body is read too.
pub(crate) fn send(
    request: &WireRequest<'_>,
    content: Option<&[u8]>,
) -> Result<(ureq::http::response::Parts, ureq::Body), ureq::Error> {
    let mut builder = ureq::http::Request::builder()
        .method(request.method().as_str())
        .uri(request.url());
    for (name, value) in request.headers() {
        builder = builder.header(name, value);
    }
    // This host returns the stored bytes of an object and never decompresses
    // them: see the `ureq` dependency in Cargo.toml, which turns the
    // decoding off.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let response = match (request.payload(), content) {
        (Payload::Slice(bytes), _) if !bytes.is_empty() => agent.run(builder.body(bytes)?)?,
        (_, Some(bytes)) => agent.run(builder.body(bytes)?)?,
        _ => agent.run(builder.body(())?)?,
    };
    Ok(response.into_parts())
}

// The head of a response, as the crate reads it.
pub(crate) fn head(parts: &ureq::http::response::Parts) -> ResponseHead<'_> {
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
