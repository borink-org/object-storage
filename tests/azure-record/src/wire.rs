//! One HTTP/1.1 request over TLS, and the response as the socket carried it.
//!
//! The recorder writes what it received into a file, so it reads the response
//! itself rather than through an HTTP client. A client keeps a `Response`
//! type, not a message: it lower-cases and reorders header names, drops the
//! reason phrase, and may decode the body. What this module returns is the
//! status line as it arrived, every header in the order it arrived, and the
//! message body with only the chunked framing removed: a
//! `RecordedResponse`, which is what the file then holds.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::OnceLock;

use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::ServerName;
pub use test_support::recorded::RecordedResponse as Response;

/// One request to send, addressed by absolute URL.
pub struct Request {
    /// The HTTP method, upper case.
    pub method: String,
    /// The absolute URL, `https://host/path?query`.
    pub url: String,
    /// The headers to send, in this order. `host` and `connection` are added.
    pub headers: Vec<(String, String)>,
    /// The request body. An empty body sends no `content-length` of its own.
    pub body: Vec<u8>,
}

impl Request {
    /// A request with no headers and no body.
    pub fn new(method: &str, url: impl Into<String>) -> Self {
        Self {
            method: method.to_owned(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Adds one header, after the ones already there.
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    /// Sets the body, and states its length.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self.headers
            .push(("content-length".to_owned(), self.body.len().to_string()));
        self
    }
}

// How the head says the body is framed.
fn chunked(response: &Response) -> bool {
    response
        .header("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case(b"chunked"))
}

fn content_length(response: &Response) -> Result<Option<usize>, Box<dyn std::error::Error>> {
    match response.header("content-length") {
        Some(value) => Ok(Some(std::str::from_utf8(value)?.trim().parse()?)),
        None => Ok(None),
    }
}

/// Sends `request` and reads the whole response.
///
/// The connection carries this one request and is then dropped, but the
/// recorder does not ask the service to close it: a `connection: close` in the
/// request puts one in the response, and the file would then record a header
/// that the recorder itself provoked. So this reads the body by the framing
/// the response states, and stops where that framing ends.
pub fn send(request: &Request) -> Result<Response, Box<dyn std::error::Error>> {
    let (host, target) = split_url(&request.url)?;

    let mut head = format!("{} {target} HTTP/1.1\r\n", request.method);
    head.push_str(&format!("host: {host}\r\n"));
    for (name, value) in &request.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    // A write must state its length even when it sends nothing, so a request
    // that did not state one states it here.
    if request.method == "PUT"
        && !request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
    {
        head.push_str(&format!("content-length: {}\r\n", request.body.len()));
    }
    head.push_str("\r\n");

    let mut stream = connect(host)?;
    stream.write_all(head.as_bytes())?;
    stream.write_all(&request.body)?;
    stream.flush()?;

    let mut raw = Vec::new();
    let cut = read_until(&mut stream, &mut raw, |raw| {
        find(raw, b"\r\n\r\n").map(|at| at + 4)
    })?;
    let mut response = parse(&raw[..cut])?;

    // The head states how the body is framed, and the framing states where the
    // body ends. Reading past that would wait for a message that is not coming.
    let body_at = cut;
    if request.method != "HEAD" && !matches!(response.status, 204 | 304) {
        if chunked(&response) {
            read_until(&mut stream, &mut raw, |raw| {
                chunked_end(&raw[body_at..]).map(|at| body_at + at)
            })?;
            response.body = dechunk(&raw[body_at..])?;
        } else if let Some(length) = content_length(&response)? {
            read_until(&mut stream, &mut raw, |raw| {
                (raw.len() >= body_at + length).then_some(body_at + length)
            })?;
            response.body = raw[body_at..body_at + length].to_vec();
        }
    }
    Ok(response)
}

// Reads until `enough` says where the part being read ends.
fn read_until(
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
    raw: &mut Vec<u8>,
    enough: impl Fn(&[u8]) -> Option<usize>,
) -> Result<usize, Box<dyn std::error::Error>> {
    loop {
        if let Some(end) = enough(raw) {
            return Ok(end);
        }
        let mut chunk = [0; 16 * 1024];
        // A server that closes a TLS connection without a close_notify is an
        // error to rustls. The response is over either way, and the caller
        // decides whether what arrived was a whole one.
        let read = stream.read(&mut chunk).unwrap_or(0);
        if read == 0 {
            return enough(raw).ok_or_else(|| "the response was cut short".into());
        }
        raw.extend_from_slice(&chunk[..read]);
    }
}

// The offset just past the terminating zero-sized chunk, once it has arrived.
fn chunked_end(body: &[u8]) -> Option<usize> {
    let mut at = 0;
    loop {
        let end = at + find(&body[at..], b"\r\n")?;
        let size = std::str::from_utf8(&body[at..end]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        at = end + 2;
        if size == 0 {
            // The last chunk is followed by trailers, if any, and then a
            // blank line, which is where the message ends.
            loop {
                let line = find(&body[at..], b"\r\n")?;
                at += line + 2;
                if line == 0 {
                    return Some(at);
                }
            }
        }
        at += size + 2;
        if at > body.len() {
            return None;
        }
    }
}

fn connect(
    host: &str,
) -> Result<StreamOwned<ClientConnection, TcpStream>, Box<dyn std::error::Error>> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    let config = CONFIG
        .get_or_init(|| {
            let roots = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            Arc::new(
                ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )
        })
        .clone();

    let name = ServerName::try_from(host.to_owned())?;
    let connection = ClientConnection::new(config, name)?;
    let socket = TcpStream::connect((host, 443))?;
    Ok(StreamOwned::new(connection, socket))
}

// Splits `https://host/path?query` into the host and the request target.
fn split_url(url: &str) -> Result<(&str, &str), Box<dyn std::error::Error>> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("not an https URL: {url}"))?;
    Ok(match rest.find('/') {
        Some(cut) => (&rest[..cut], &rest[cut..]),
        None => (rest, "/"),
    })
}

// Reads the status line and the headers out of a complete response head.
fn parse(head: &[u8]) -> Result<Response, Box<dyn std::error::Error>> {
    let cut = find(head, b"\r\n\r\n").ok_or("the response head never ended")?;
    let mut lines = head[..cut].split(|byte| *byte == b'\n');

    let status_line = lines
        .next()
        .ok_or("the response was empty")?
        .strip_suffix(b"\r")
        .ok_or("the status line did not end in CRLF")?;
    let status_line = String::from_utf8(status_line.to_vec())?;
    let status = status_line
        .split(' ')
        .nth(1)
        .ok_or("the status line named no status")?
        .parse()?;

    let mut headers = Vec::new();
    for line in lines {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let colon = find(line, b":").ok_or("a header line held no colon")?;
        let name = String::from_utf8(line[..colon].to_vec())?.to_ascii_lowercase();
        let value: Vec<u8> = line[colon + 1..]
            .iter()
            .copied()
            .skip_while(|byte| *byte == b' ')
            .collect();
        headers.push((name, value));
    }

    Ok(Response {
        status_line,
        status,
        headers,
        body: Vec::new(),
    })
}

// Joins the chunks of a chunked body. The recorder keeps the header that says
// the body arrived this way, because that is what the service sent; what it
// stores under that header is the body, not the framing.
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    loop {
        let end = find(body, b"\r\n").ok_or("a chunk stated no size")?;
        let size = std::str::from_utf8(&body[..end])?;
        // A chunk size may carry extensions after a semicolon.
        let size = usize::from_str_radix(size.split(';').next().unwrap_or(size).trim(), 16)?;
        body = &body[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        out.extend_from_slice(body.get(..size).ok_or("a chunk was cut short")?);
        body = &body[size + 2..];
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_head_keeps_the_status_line_and_lower_cases_the_names() {
        let response = parse(
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-6/30\r\nx-ms-request-id:  abc\r\n\r\nbody",
        )
        .unwrap();
        assert_eq!(response.status_line, "HTTP/1.1 206 Partial Content");
        assert_eq!(response.status, 206);
        assert_eq!(
            response.headers,
            [
                ("content-range".to_owned(), b"bytes 2-6/30".to_vec()),
                ("x-ms-request-id".to_owned(), b"abc".to_vec()),
            ]
        );
        // The body is read by the framing the head states, not here.
        assert!(response.body.is_empty());
    }

    #[test]
    fn a_head_that_is_not_one_is_refused() {
        assert!(parse(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\nno-crlf: x\r\n\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n").is_err());
        assert!(parse(b"HTTP/1.1\r\n\r\n").is_err());
    }

    #[test]
    fn the_framing_is_read_off_the_head() {
        let chunked_head = parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n").unwrap();
        assert!(chunked(&chunked_head));
        assert_eq!(content_length(&chunked_head).unwrap(), None);

        let sized = parse(b"HTTP/1.1 200 OK\r\nContent-Length: 30 \r\n\r\n").unwrap();
        assert!(!chunked(&sized));
        assert_eq!(content_length(&sized).unwrap(), Some(30));

        let nonsense = parse(b"HTTP/1.1 200 OK\r\nContent-Length: many\r\n\r\n").unwrap();
        assert!(content_length(&nonsense).is_err());
    }

    #[test]
    fn chunks_are_joined_and_their_framing_dropped() {
        let body = b"4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\n\r\n";
        assert_eq!(dechunk(body).unwrap(), b"Wikipedia");
        assert_eq!(chunked_end(body), Some(body.len()));
    }

    #[test]
    fn a_chunked_body_ends_after_its_last_chunk_and_trailer() {
        let body = b"1\r\na\r\n0\r\nx-trailer: 1\r\n\r\nHTTP/1.1 200 OK\r\n";
        let end = chunked_end(body).unwrap();
        assert_eq!(&body[..end], b"1\r\na\r\n0\r\nx-trailer: 1\r\n\r\n");
        assert_eq!(dechunk(&body[..end]).unwrap(), b"a");
    }

    #[test]
    fn a_chunked_body_still_arriving_has_no_end_yet() {
        assert_eq!(chunked_end(b"4\r\nWi"), None);
        assert_eq!(chunked_end(b"4\r\nWiki\r\n0\r\n"), None);
        assert_eq!(chunked_end(b""), None);
    }

    #[test]
    fn a_chunk_cut_short_is_an_error_not_a_body() {
        assert!(dechunk(b"4\r\nWi").is_err());
        assert!(dechunk(b"zz\r\n").is_err());
        assert!(dechunk(b"4\r\nWiki\r\n").is_err());
    }

    #[test]
    fn the_url_splits_into_the_host_and_the_target() {
        assert_eq!(
            split_url("https://a.blob.core.windows.net/c/k?x=1").unwrap(),
            ("a.blob.core.windows.net", "/c/k?x=1")
        );
        assert_eq!(split_url("https://host").unwrap(), ("host", "/"));
        assert!(split_url("http://host/").is_err());
    }
}
