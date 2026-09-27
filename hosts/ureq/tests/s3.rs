//! Loopback integration test for the S3 requests of the synchronous `ureq`
//! host.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use borink_object_storage_crypto::SHA256_RUSTCRYPTO;
use borink_object_storage_proto::s3::{Bucket, Objects, Service};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_ureq::s3;

// Reads one request, head and body, and answers it with `response`.
fn answer(listener: &TcpListener, response: &[u8]) -> String {
    let (mut stream, _) = listener.accept().unwrap();
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    let head_end = loop {
        if let Some(at) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            break at + 4;
        }
        let count = stream.read(&mut chunk).unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&chunk[..count]);
    };
    let head = String::from_utf8(request[..head_end].to_vec())
        .unwrap()
        .to_ascii_lowercase();
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .map_or(0, |value| value.parse::<usize>().unwrap());
    while request.len() < head_end + content_length {
        let count = stream.read(&mut chunk).unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&chunk[..count]);
    }
    stream.write_all(response).unwrap();
    String::from_utf8(request).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn executes_the_generated_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let get = answer(
            &listener,
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody",
        );
        assert!(get.starts_with("GET /bucket/a%20key HTTP/1.1\r\n"), "{get}");
        assert!(
            get.to_ascii_lowercase()
                .contains("authorization: aws4-hmac-sha256 credential=akiaiosfodnn7example/"),
            "{get}"
        );

        let put = answer(
            &listener,
            b"HTTP/1.1 200 OK\r\nETag: \"tag\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        assert!(put.starts_with("PUT /bucket/a%20key HTTP/1.1\r\n"), "{put}");
        // The request signs the SHA-256 of the content it carries.
        let sha256 = hex(&SHA256_RUSTCRYPTO.hash(b"content"));
        assert!(
            put.to_ascii_lowercase()
                .contains(&format!("x-amz-content-sha256: {sha256}\r\n")),
            "{put}"
        );
        assert!(put.ends_with("\r\n\r\ncontent"), "{put}");

        let delete = answer(
            &listener,
            b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n",
        );
        assert!(
            delete.starts_with("DELETE /bucket/a%20key HTTP/1.1\r\n"),
            "{delete}"
        );
    });

    let bucket = Bucket::new(&endpoint, "bucket", "us-east-1", Service::Aws).unwrap();
    let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret").unwrap();
    let objects = Objects::new(bucket, credentials, SHA256_RUSTCRYPTO);
    assert_eq!(s3::get(&objects, "a key").unwrap(), b"body");
    s3::put(&objects, "a key", b"content").unwrap();
    s3::delete(&objects, "a key").unwrap();
    server.join().unwrap();
}
