//! Loopback integration test for the listing of the synchronous `ureq` host.

mod loopback;

use borink_object_storage_proto::{Blobs, Container, EntryKind, ListEntry, PhysicalList};
use test_support::azure::{FIXTURES_CONTAINER, FIXTURES_PREFIX};

/// The host sends the request that produced `azure-listing/list-page`, as the
/// notes beside that file spell it, and reads the page Azure answered with.
#[test]
fn reads_the_page_that_the_generated_request_asked_for() {
    let server =
        loopback::Server::answering(test_support::recorded::load("azure-listing/list-page"));

    let board = format!("{FIXTURES_PREFIX}board/");
    let blobs = Blobs::new(
        Container::new(&server.endpoint, FIXTURES_CONTAINER).unwrap(),
        "token",
    )
    .unwrap();
    let mut body = Vec::new();
    let mut entries = [ListEntry::default(); 4];
    let page =
        borink_azure_get_ureq::list(&blobs, &PhysicalList::new(&board), &mut body, &mut entries)
            .unwrap();

    assert_eq!(page.filled, 3);
    assert_eq!(page.next_marker, None);
    assert_eq!(
        entries[..3]
            .iter()
            .map(|entry| (entry.kind, entry.key, entry.size))
            .collect::<Vec<_>>(),
        [
            (EntryKind::Object, format!("{board}a.txt").as_str(), Some(8)),
            (
                EntryKind::Object,
                format!("{board}nested/c.txt").as_str(),
                Some(1)
            ),
            (EntryKind::Object, format!("{board}z.txt").as_str(), Some(2)),
        ]
    );

    let request = server.request();
    assert!(
        request.starts_with(&format!(
            "GET /{FIXTURES_CONTAINER}?restype=container&comp=list&prefix=borink-object-storage%2Ffixtures%2Fboard%2F HTTP/1.1\r\n"
        )),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer token\r\n")
    );
}
