//! Lists every key under one prefix, one page at a time.

use std::env;

use borink_object_storage_proto::{Blobs, Container, Fill, ListEntry, PhysicalList};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("AZURE_STORAGE_ENDPOINT")?;
    let container = env::var("AZURE_STORAGE_CONTAINER")?;
    let token = env::var("AZURE_STORAGE_ACCESS_TOKEN")?;
    let prefix = env::args().nth(1).unwrap_or_default();
    let blobs = Blobs::new(Container::new(&endpoint, &container)?, &token)?;

    let mut marker: Option<Vec<u8>> = None;
    let mut body = Vec::new();
    loop {
        // The array is this program's budget, and it holds a whole page, so no
        // page is ever read in more than one round. It borrows the body, so it
        // belongs to the round that reads it.
        let mut entries = vec![ListEntry::default(); 1000];
        let plan = PhysicalList {
            marker: marker.as_deref(),
            max_results: Some(1000),
            ..PhysicalList::new(&prefix)
        };
        let Fill::Page(page) = borink_azure_get_ureq::list(&blobs, &plan, &mut body, &mut entries)?
        else {
            return Err("the page did not fit an array of its own size".into());
        };
        for entry in &entries[..page.filled] {
            println!("{}", entry.key);
        }
        // The next request reads into another body, so the marker is copied
        // out of this one.
        match page.next_marker {
            Some(next) => marker = Some(next.to_vec()),
            None => return Ok(()),
        }
    }
}
