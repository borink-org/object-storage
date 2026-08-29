//! Loopback integration test for the synchronous `ureq` host.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use borink_object_storage_proto::{Blobs, Container};

#[test]
fn executes_the_generated_request() {
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
        assert!(request.starts_with("GET /container/a%20key HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer token\r\n")
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody")
            .unwrap();
    });

    let blobs = Blobs::new(Container::new(&endpoint, "container").unwrap(), "token").unwrap();
    assert_eq!(
        borink_azure_get_ureq::get(&blobs, "a key").unwrap(),
        b"body"
    );
    server.join().unwrap();
}
