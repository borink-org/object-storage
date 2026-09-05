//! A server on the loopback interface that answers one request with a
//! recorded response.
//!
//! This is the one place the request head is checked as an HTTP message: what
//! the host puts on the wire, read back by a socket. The response it answers
//! with is one Azure sent, from the recorded corpus, so the host reads the
//! bytes a real account writes and not a document written for the test.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};

use test_support::recorded::RecordedResponse;

/// One request's worth of server.
pub struct Server {
    /// `http://127.0.0.1:port`, for the host to address.
    pub endpoint: String,
    thread: JoinHandle<String>,
}

impl Server {
    /// Listens for one request, answers it with `response`, and keeps the
    /// request head for [`Server::request`].
    pub fn answering(response: RecordedResponse) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).unwrap();
                assert_ne!(count, 0, "the request head never ended");
                request.extend_from_slice(&chunk[..count]);
            }
            stream.write_all(&wire(&response)).unwrap();
            String::from_utf8(request).unwrap()
        });
        Self { endpoint, thread }
    }

    /// The request head the host sent, once the exchange is over.
    pub fn request(self) -> String {
        self.thread.join().unwrap()
    }
}

// The response as a message on the wire. The file holds the body joined, so
// the framing header it kept is replaced by the length the body now has, and
// the connection is closed after it because the server answers once.
fn wire(response: &RecordedResponse) -> Vec<u8> {
    let mut out = format!("{}\r\n", response.status_line).into_bytes();
    for (name, value) in &response.headers {
        if matches!(name.as_str(), "transfer-encoding" | "content-length") {
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("content-length: {}\r\n", response.body.len()).as_bytes());
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(&response.body);
    out
}
