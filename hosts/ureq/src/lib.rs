//! A synchronous `ureq` host for `borink-object-storage` Azure GET requests.

use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage::{Blobs, GetOptions, RequestWorkspace, Response, Timestamps};

/// Builds and executes one GET request, returning an owned response body.
pub fn get(blobs: &Blobs<'_>, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let options = GetOptions::default();
    let required = blobs.get_request_requirements(key, &options)?;
    let mut storage = vec![0; required.packed];
    let mut workspace = RequestWorkspace::new(&mut storage);
    let request = blobs.get_request(&mut workspace, key, &options, &now)?;

    let mut outgoing = ureq::get(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let status = incoming.status().as_u16();
    blobs.interpret_get(
        Response::new(
            status,
            incoming.headers().iter().filter_map(|(name, value)| {
                value.to_str().ok().map(|value| (name.as_str(), value))
            }),
        ),
        &options,
    )?;
    incoming.body_mut().read_to_vec().map_err(Into::into)
}
