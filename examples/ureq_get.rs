use std::env;
use std::io::Write;
use std::time::SystemTime;

use borink_object_storage::{Blobs, Container, RequestWorkspace, Response};

pub fn get(blobs: &Blobs<'_>, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let date = httpdate::fmt_http_date(SystemTime::now());
    let mut storage = [0; 4096];
    let mut workspace = RequestWorkspace::new(&mut storage);
    let request = blobs.get_request(&mut workspace, key, &date)?;

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
        .map(|bytes| bytes.to_vec())
        .map_err(Into::into)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("AZURE_STORAGE_ENDPOINT")?;
    let container = env::var("AZURE_STORAGE_CONTAINER")?;
    let token = env::var("AZURE_STORAGE_ACCESS_TOKEN")?;
    let key = env::args().nth(1).ok_or("missing object key")?;
    let blobs = Blobs::new(Container::new(&endpoint, &container)?, &token)?;
    std::io::stdout().write_all(&get(&blobs, &key)?)?;
    Ok(())
}
