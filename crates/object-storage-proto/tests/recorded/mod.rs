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

use borink_object_storage_proto::ResponseHead;
use test_support::recorded::RecordedResponse;

/// One recorded response.
pub struct Recorded(RecordedResponse);

impl Recorded {
    /// Reads the response at `name`, a path under `tests/fixtures` without the
    /// `.http`: `azure-listing/list-page`.
    pub fn load(name: &str) -> Self {
        Self(test_support::recorded::load(name))
    }

    /// The status the service answered with.
    pub fn status(&self) -> u16 {
        self.0.status
    }

    /// The head this crate reads out of that response.
    pub fn head(&self) -> ResponseHead<'_> {
        ResponseHead::from_headers(
            self.0.status,
            self.0
                .headers
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
        self.0.body.clone()
    }

    /// The body as text, for a test that asks what the document holds. The
    /// recorded bytes stay as they are, so this may be read after a page has
    /// been decoded out of a copy of them.
    pub fn text(&self) -> String {
        String::from_utf8(self.0.body.clone()).expect("a recorded document is UTF-8")
    }

    /// The value of one header, whether or not this crate reads that header.
    pub fn header(&self, name: &str) -> Option<&[u8]> {
        self.0.header(name)
    }
}
