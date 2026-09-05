//! The file format of a recorded response, and where the corpus is.
//!
//! `tests/azure-record` writes one file per response it received, and the
//! offline tests read them back. Both sides go through this module, so the
//! format is spelled once: the status line as it arrived, every header in
//! the order it arrived with its name lower-cased, a blank line, and the body
//! to the last byte. A body that arrived in chunks was joined before it was
//! written; the header that records the framing stays as it arrived.
//!
//! A header value is bytes rather than text, because a server may send one
//! that is not UTF-8. The line endings in a file are bare `\n`: the head is
//! for a reader, and a body holding `\r\n` is written as it was received.

use std::fs;
use std::path::{Path, PathBuf};

/// One response, as a storage account sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedResponse {
    /// The status line without its line ending: `HTTP/1.1 200 OK`.
    pub status_line: String,
    /// The status code out of that line.
    pub status: u16,
    /// Every header in arrival order, its name lower-cased.
    pub headers: Vec<(String, Vec<u8>)>,
    /// The message body, with any chunked framing removed.
    pub body: Vec<u8>,
}

impl RecordedResponse {
    /// Reads a file written by [`RecordedResponse::to_bytes`].
    pub fn parse(raw: &[u8]) -> Result<Self, String> {
        // The head ends at the first blank line. Everything after it is the
        // body, to the last byte, so a body holding a blank line is whole.
        let cut = raw
            .windows(2)
            .position(|window| window == b"\n\n")
            .ok_or("the head never ended")?;
        let mut lines = raw[..cut].split(|byte| *byte == b'\n');

        let status_line = lines.next().ok_or("the file is empty")?;
        let status_line =
            String::from_utf8(status_line.to_vec()).map_err(|_| "the status line is not text")?;
        let status = status_line
            .split(' ')
            .nth(1)
            .and_then(|status| status.parse().ok())
            .ok_or_else(|| format!("{status_line:?} names no status"))?;

        let mut headers = Vec::new();
        for line in lines {
            let colon = line
                .iter()
                .position(|byte| *byte == b':')
                .ok_or_else(|| format!("{:?} is not a header", String::from_utf8_lossy(line)))?;
            let name = String::from_utf8(line[..colon].to_vec())
                .map_err(|_| "a header name is not text")?;
            if name != name.to_ascii_lowercase() {
                return Err(format!("the header {name:?} is not lower-cased"));
            }
            let value = line[colon + 1..]
                .iter()
                .copied()
                .skip_while(|byte| *byte == b' ')
                .collect();
            headers.push((name, value));
        }

        Ok(Self {
            status_line,
            status,
            headers,
            body: raw[cut + 2..].to_vec(),
        })
    }

    /// The file: the status line, the headers, a blank line, and the body.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.status_line.as_bytes());
        out.push(b'\n');
        for (name, value) in &self.headers {
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(value);
            out.push(b'\n');
        }
        out.push(b'\n');
        out.extend_from_slice(&self.body);
        out
    }

    /// The first value of `name`, compared without case.
    pub fn header(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(got, _)| got.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_slice())
    }
}

/// The directory that holds the corpus. Every group is a directory under it,
/// and every file in a group is one response.
pub fn corpus_dir() -> PathBuf {
    // This crate sits at `tests/support`, two levels below the workspace.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/object-storage-proto/tests/fixtures")
        .canonicalize()
        .expect("the corpus directory exists")
}

/// Reads the response at `name`, a path under the corpus without the
/// `.http`: `azure-listing/list-page`.
pub fn load(name: &str) -> RecordedResponse {
    let path = corpus_dir().join(format!("{name}.http"));
    let raw = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    RecordedResponse::parse(&raw).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response() -> RecordedResponse {
        RecordedResponse {
            status_line: "HTTP/1.1 206 Partial Content".to_owned(),
            status: 206,
            headers: vec![
                ("content-length".to_owned(), b"5".to_vec()),
                ("x-ms-meta-raw".to_owned(), vec![0xFF, 0xFE]),
                ("etag".to_owned(), b"\"0x8DF\"".to_vec()),
            ],
            body: b"a\n\nb\r\n".to_vec(),
        }
    }

    #[test]
    fn a_response_survives_the_round_trip() {
        let response = response();
        assert_eq!(RecordedResponse::parse(&response.to_bytes()), Ok(response));
    }

    #[test]
    fn the_file_is_the_head_a_blank_line_and_the_body() {
        assert_eq!(
            response().to_bytes(),
            b"HTTP/1.1 206 Partial Content\ncontent-length: 5\nx-ms-meta-raw: \xFF\xFE\netag: \"0x8DF\"\n\na\n\nb\r\n"
        );
    }

    #[test]
    fn the_body_starts_after_the_first_blank_line_and_runs_to_the_end() {
        let parsed = RecordedResponse::parse(b"HTTP/1.1 200 OK\n\n\n\nx\n").unwrap();
        assert_eq!(parsed.body, b"\n\nx\n");
        assert!(parsed.headers.is_empty());
    }

    #[test]
    fn a_header_value_loses_only_the_space_after_the_colon() {
        let parsed = RecordedResponse::parse(b"HTTP/1.1 200 OK\na:   b: c \n\n").unwrap();
        assert_eq!(parsed.headers, [("a".to_owned(), b"b: c ".to_vec())]);
        assert_eq!(parsed.header("A"), Some(b"b: c ".as_slice()));
    }

    #[test]
    fn a_damaged_file_is_refused_rather_than_read() {
        assert!(RecordedResponse::parse(b"HTTP/1.1 200 OK\n").is_err());
        assert!(RecordedResponse::parse(b"HTTP/1.1\n\n").is_err());
        assert!(RecordedResponse::parse(b"HTTP/1.1 200 OK\nno colon\n\n").is_err());
        assert!(RecordedResponse::parse(b"HTTP/1.1 200 OK\nContent-Length: 1\n\n").is_err());
    }

    #[test]
    fn the_corpus_is_where_the_offline_tests_read_it() {
        assert!(corpus_dir().join("azure-listing").is_dir());
    }
}
