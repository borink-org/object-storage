//! Loopback integration test for the synchronous `ureq` host.

mod loopback;

use borink_object_storage_proto::{Blobs, Container};
use test_support::azure::{FIXTURES_CONTAINER, FIXTURES_PREFIX};

/// The host sends the request that produced `azure-get/get-whole`, as the
/// notes beside that file spell it, and returns the body Azure answered with.
#[test]
fn executes_the_generated_request() {
    let server = loopback::Server::answering(test_support::recorded::load("azure-get/get-whole"));

    let blobs = Blobs::new(
        Container::new(&server.endpoint, FIXTURES_CONTAINER).unwrap(),
        "token",
    )
    .unwrap();
    let key = format!("{FIXTURES_PREFIX}read/object.txt");
    assert_eq!(
        borink_azure_get_ureq::get(&blobs, &key).unwrap(),
        b"0123456789-azure-record-object"
    );

    let request = server.request();
    assert!(
        request.starts_with(&format!("GET /{FIXTURES_CONTAINER}/{key} HTTP/1.1\r\n")),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer token\r\n")
    );
}
