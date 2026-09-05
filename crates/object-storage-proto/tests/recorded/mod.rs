//! The recorded responses under `tests/fixtures`, as a test reads them.
//!
//! Each file is one response as a real storage account sent it, written by
//! `tests/azure-record`. The notes in each directory say which request
//! produced which file, and what it shows. Nothing here fixes a response up:
//! a test that reads one asserts against the bytes Azure sent.
//!
//! A test that needs a shape the service does not produce still writes its own
//! document. Say so above it, and say why the service will not send one.

#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

use borink_object_storage_proto::ResponseHead;

/// One recorded response.
pub struct Recorded {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

impl Recorded {
    /// Reads the response at `name`, a path under `tests/fixtures` without the
    /// `.http`: `azure-listing/list-page`.
    pub fn load(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(format!("{name}.http"));
        let raw = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));

        // The head ends at the first blank line. Everything after it is the
        // body, to the last byte, so a body holding a blank line is whole.
        let cut = raw
            .windows(2)
            .position(|window| window == b"\n\n")
            .unwrap_or_else(|| panic!("{}: the head never ended", path.display()));
        let mut lines = raw[..cut].split(|byte| *byte == b'\n');

        let status_line = str::from_utf8(lines.next().expect("a response has a status line"))
            .expect("a status line is text");
        let status = status_line
            .split(' ')
            .nth(1)
            .and_then(|status| status.parse().ok())
            .unwrap_or_else(|| panic!("{}: {status_line} names no status", path.display()));

        let headers = lines
            .map(|line| {
                let colon = line
                    .iter()
                    .position(|byte| *byte == b':')
                    .expect("a header line holds a colon");
                let name = str::from_utf8(&line[..colon]).expect("a header name is text");
                let value = line[colon + 1..]
                    .iter()
                    .copied()
                    .skip_while(|byte| *byte == b' ')
                    .collect();
                (name.to_owned(), value)
            })
            .collect();

        Self {
            status,
            headers,
            body: raw[cut + 2..].to_vec(),
        }
    }

    /// The status the service answered with.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The head this crate reads out of that response.
    pub fn head(&self) -> ResponseHead<'_> {
        ResponseHead::from_headers(
            self.status,
            self.headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_slice())),
        )
    }

    /// The response body, as a buffer the caller owns.
    ///
    /// Reading a page decodes values where they stand, so the reader needs a
    /// buffer it may write into. Each call hands out another copy of the
    /// recorded bytes.
    pub fn body(&self) -> Vec<u8> {
        self.body.clone()
    }

    /// The body as text, for a test that asks what the document holds. The
    /// recorded bytes stay as they are, so this may be read after a page has
    /// been decoded out of a copy of them.
    pub fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("a recorded document is UTF-8")
    }

    /// The value of one header, whether or not this crate reads that header.
    pub fn header(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(got, _)| got.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_slice())
    }
}
