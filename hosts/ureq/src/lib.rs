//! A synchronous `ureq` host for `borink-object-storage` Azure GET requests.

use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage::{Blobs, GetHead, GetHeadOutcome, PhysicalGet, Timestamps, layered};

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
    // Automatic decompression must stay off: ranges and lengths are defined
    // over the stored representation, and a decoding client strips the very
    // headers that would reveal it changed the bytes.
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
        GetHeadOutcome::Complete(_) => Ok(Vec::new()),
        outcome => Err(format!("Azure GET failed: {outcome:?}").into()),
    }
}
