//! Runs one Azure GET and writes the object body to standard output.

use std::env;
use std::io::Write;

use borink_object_storage_proto::ListOptions;
use borink_object_storage_proto::azure::Container;
use borink_object_storage_ureq::azure;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("AZURE_STORAGE_ENDPOINT")?;
    let container = env::var("AZURE_STORAGE_CONTAINER")?;
    let token = env::var("AZURE_STORAGE_ACCESS_TOKEN")?;
    let key = env::args().nth(1).ok_or("missing object key")?;
    let container = Container::new(&endpoint, &container)?;
    let mut blobs = azure::Blobs::new(&container, &token, &ListOptions::default())?;
    std::io::stdout().write_all(&blobs.get(&key)?)?;
    Ok(())
}
