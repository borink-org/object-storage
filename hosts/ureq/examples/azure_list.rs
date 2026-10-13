//! Lists every key under one prefix, one page at a time, from one template.

use std::env;

use borink_object_storage_proto::azure as proto;
use borink_object_storage_proto::azure::{AzureNamespace, Container};
use borink_object_storage_proto::{Error, ListEntry, ListItem, ListMarker, ListOptions};
use borink_object_storage_ureq::azure;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("AZURE_STORAGE_ENDPOINT")?;
    let container = env::var("AZURE_STORAGE_CONTAINER")?;
    let token = env::var("AZURE_STORAGE_ACCESS_TOKEN")?;
    let prefix = env::args().nth(1).unwrap_or_default();
    let container = Container::new(&endpoint, &container)?;

    // Every page shares the template: an empty buffer is refused with the
    // size it needs.
    let options = ListOptions {
        max_results: Some(1000),
        ..ListOptions::default()
    };
    let prepare = |buf: &mut [u8]| {
        proto::prepare_list(&container, &token, AzureNamespace::Unknown, &options, buf)
    };
    let size = match prepare(&mut []) {
        Err(Error::Capacity(capacity)) => capacity.required,
        other => return Err(format!("{other:?}").into()),
    };
    let mut bytes = vec![0; size];
    let template = prepare(&mut bytes)?;

    let mut marker: Option<String> = None;
    let mut body = Vec::new();
    loop {
        // The array holds a whole page, because it is as long as the page the
        // template asks for. It borrows the body, so it belongs to the round
        // that reads it.
        let mut entries = vec![ListEntry::default(); 1000];
        let item = ListItem {
            marker: marker.as_deref().map(ListMarker::Text),
            ..ListItem::new(&prefix)
        };
        let page = azure::list(&template, &bytes, &item, &mut body, &mut entries)?;
        for entry in &entries[..page.filled] {
            println!("{}", entry.key);
        }
        // The next request reads into another body, so the marker is copied
        // out of this one.
        match page.next_marker.and_then(ListMarker::text) {
            Some(next) => marker = Some(next.to_owned()),
            None => return Ok(()),
        }
    }
}
