//! A synchronous `ureq` host for `borink-object-storage` Azure GET requests.

use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage::{
    Blobs, Classification, GetHead, GetHeadOutcome, PhysicalGet, Timestamps, classify_error,
    layered,
};

// Error bodies are diagnostics, so this host caps what it will read for one.
const MAX_ERROR_BODY: u64 = 8 * 1024;

/// Builds and executes one GET request, returning an owned response body.
pub fn get(blobs: &Blobs<'_>, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let get = PhysicalGet::new(key);
    let mut buf = vec![0; layered::requirements(blobs, &get, &now)?];
    let request = blobs.encode_get(&mut buf, &get, &now)?;

    let mut outgoing = ureq::get(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    // This host returns the stored bytes of the blob, encoded as Azure holds
    // them. It never decompresses: `Content-Length`, `Content-Range` and so
    // the returned `BodyWindow` all count stored bytes, and a client that
    // decodes the body would return different bytes under those numbers. See
    // the `ureq` dependency in Cargo.toml, which turns the decoding off.
    // Read `ObjectMeta::content_encoding` to learn how the bytes are encoded.
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let status = incoming.status().as_u16();
    let headers = incoming.headers().clone();
    let head = GetHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match blobs.accept_get_head(get.shape(), head)? {
        GetHeadOutcome::Body { .. } => incoming.body_mut().read_to_vec().map_err(Into::into),
        GetHeadOutcome::Complete { .. } => Ok(Vec::new()),
        outcome => Err(failure(incoming.body_mut(), &head, outcome)),
    }
}

/// Describes a read that returned no object.
///
/// The outcome already names the error, because Azure sends it in a header.
/// This host reads the body only for the rare response that carries no such
/// header, so a body it cannot read costs a detail rather than the message.
fn failure(
    body: &mut ureq::Body,
    head: &GetHead<'_>,
    outcome: GetHeadOutcome<'_>,
) -> Box<dyn std::error::Error> {
    let unnamed = matches!(
        outcome,
        GetHeadOutcome::NotFound { kind: None } | GetHeadOutcome::ServiceFailure { kind: None, .. }
    );
    if !unnamed {
        return format!("Azure returned no object: {outcome}").into();
    }
    let body = body
        .with_config()
        .limit(MAX_ERROR_BODY)
        .read_to_vec()
        .unwrap_or_default();
    match classify_error(head, &body, body.len() as u64 >= MAX_ERROR_BODY) {
        Classification::Classified(kind) => {
            format!("Azure returned no object: {outcome} ({kind})")
        }
        _ => format!("Azure returned no object: {outcome}"),
    }
    .into()
}
