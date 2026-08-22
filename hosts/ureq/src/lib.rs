use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage::{Blobs, RequestWorkspace, Response, Timestamps};

pub fn get(blobs: &Blobs<'_>, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let required = blobs.get_request_requirements(key)?;
    let mut storage = vec![0; required.packed];
    let mut workspace = RequestWorkspace::new(&mut storage);
    let request = blobs.get_request(&mut workspace, key, &now)?;

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
    let body = incoming.body_mut().read_to_vec()?;
    blobs
        .interpret_get(Response::new(status, &body))
        .map(<[u8]>::to_vec)
        .map_err(Into::into)
}
