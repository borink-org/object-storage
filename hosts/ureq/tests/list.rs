//! Loopback integration test for the listing of the synchronous `ureq` host.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use borink_object_storage_proto::{Blobs, Container, Fill, ListEntry, PhysicalList};

const PAGE: &str = "<EnumerationResults><Blobs>\
                    <Blob><Name>directory/a.txt</Name><Properties>\
                    <Etag>0x1</Etag><Content-Length>4</Content-Length></Properties></Blob>\
                    <BlobPrefix><Name>directory/nested/</Name></BlobPrefix>\
                    </Blobs><NextMarker>next</NextMarker></EnumerationResults>";

#[test]
fn reads_the_page_that_the_generated_request_asked_for() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0; 1024];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&chunk[..count]);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with(
            "GET /container?restype=container&comp=list&prefix=directory%2F\
             &delimiter=%2F&maxresults=1000 HTTP/1.1\r\n"
        ));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                    PAGE.len()
                )
                .as_bytes(),
            )
            .unwrap();
    });

    let blobs = Blobs::new(Container::new(&endpoint, "container").unwrap(), "token").unwrap();
    let plan = PhysicalList {
        delimited: true,
        max_results: Some(1000),
        ..PhysicalList::new("directory/")
    };
    let mut body = Vec::new();
    let mut entries = [ListEntry::default(); 4];
    let fill = borink_azure_get_ureq::list(&blobs, &plan, &mut body, &mut entries).unwrap();

    let Fill::Page(page) = fill else {
        panic!("the array held the whole page: {fill:?}");
    };
    assert_eq!(page.filled, 2);
    assert_eq!(entries[0].key, "directory/a.txt");
    assert_eq!(entries[0].size, Some(4));
    // A delimited listing reports the level below as one group.
    assert_eq!(entries[1].key, "directory/nested/");
    assert_eq!(entries[1].size, None);
    assert_eq!(page.next_marker, Some(b"next".as_slice()));
    server.join().unwrap();
}
