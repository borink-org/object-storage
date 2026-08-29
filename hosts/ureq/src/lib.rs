//! A synchronous `ureq` host for `borink-object-storage` Azure requests.

use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage_proto::{
    Blobs, DeleteHeadOutcome, GetHeadOutcome, Payload, PhysicalDelete, PhysicalGet, PhysicalPut,
    PutHeadOutcome, ResponseHead, Timestamps, layered,
};

// Error bodies are diagnostics, so this host caps what it will read for one.
const MAX_ERROR_BODY: u64 = 8 * 1024;

/// Builds and executes one GET request, returning an owned response body.
pub fn get(blobs: &Blobs<'_>, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let get = PhysicalGet::new(key);
    let mut buf = vec![0; layered::get_requirements(blobs, &get, &now)?];
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
    let head = ResponseHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match blobs.accept_get_head(get.shape(), head)? {
        GetHeadOutcome::Body { .. } => incoming.body_mut().read_to_vec().map_err(Into::into),
        GetHeadOutcome::Complete { .. } => Ok(Vec::new()),
        // Azure named no error in the head, so the body names it. This is the
        // only response whose body this host reads for a diagnostic, and it
        // caps the read: an error body that does not arrive costs the name of
        // the error, not the outcome.
        GetHeadOutcome::NeedErrorBody(failure) => {
            let body = incoming
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BODY)
                .read_to_vec()
                .unwrap_or_default();
            Err(no_object(blobs.accept_error_body(
                failure.status,
                failure.request_id,
                &body,
            )))
        }
        outcome => Err(no_object(outcome)),
    }
}

fn no_object(outcome: GetHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure returned no object: {outcome}").into()
}

/// Builds and executes one PUT request, storing `content` as the whole object.
pub fn put(blobs: &Blobs<'_>, key: &str, content: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let put = PhysicalPut::new(key);
    let content = Payload::Slice(content);
    let mut buf = vec![0; layered::put_requirements(blobs, &put, content, &now)?];
    let request = blobs.encode_put(&mut buf, &put, content, &now)?;

    let mut outgoing = ureq::put(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        // This host writes from memory, so it always has the bytes to send.
        .send(request.payload().bytes().unwrap_or_default())?;
    let status = incoming.status().as_u16();
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match blobs.accept_put_head(put.shape(), head)? {
        PutHeadOutcome::Created { .. } => Ok(()),
        PutHeadOutcome::NeedErrorBody(failure) => {
            let body = incoming
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BODY)
                .read_to_vec()
                .unwrap_or_default();
            Err(not_stored(blobs.accept_put_error_body(
                failure.status,
                failure.request_id,
                &body,
            )))
        }
        outcome => Err(not_stored(outcome)),
    }
}

fn not_stored(outcome: PutHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure stored no object: {outcome}").into()
}

/// Builds and executes one DELETE request, removing the whole object.
///
/// Reports a missing object rather than treating it as success: only the
/// caller knows whether it meant to remove an object that is already gone.
pub fn delete(blobs: &Blobs<'_>, key: &str) -> Result<(), Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let delete = PhysicalDelete::new(key);
    let mut buf = vec![0; layered::delete_requirements(blobs, &delete, &now)?];
    let request = blobs.encode_delete(&mut buf, &delete, &now)?;

    let mut outgoing = ureq::delete(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let status = incoming.status().as_u16();
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match blobs.accept_delete_head(delete.shape(), head)? {
        DeleteHeadOutcome::Accepted => Ok(()),
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            let body = incoming
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BODY)
                .read_to_vec()
                .unwrap_or_default();
            Err(not_removed(blobs.accept_delete_error_body(
                failure.status,
                failure.request_id,
                &body,
            )))
        }
        outcome => Err(not_removed(outcome)),
    }
}

fn not_removed(outcome: DeleteHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("Azure removed no object: {outcome}").into()
}
