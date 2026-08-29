//! Runs one Azure GET and writes the object body to standard output.

use std::env;
use std::io::Write;

use borink_object_storage_proto::{Blobs, Container};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("AZURE_STORAGE_ENDPOINT")?;
    let container = env::var("AZURE_STORAGE_CONTAINER")?;
    let token = env::var("AZURE_STORAGE_ACCESS_TOKEN")?;
    let key = env::args().nth(1).ok_or("missing object key")?;
    let blobs = Blobs::new(Container::new(&endpoint, &container)?, &token)?;
    std::io::stdout().write_all(&borink_azure_get_ureq::get(&blobs, &key)?)?;
    Ok(())
}
