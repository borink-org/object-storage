// TODO(doc-review): Public API rustdoc is an initial scaffold for manual review.

/// HTTP response metadata and body borrowed from the host.
#[derive(Debug, Clone, Copy)]
pub struct Response<'a> {
    status: u16,
    body: &'a [u8],
}

impl<'a> Response<'a> {
    /// Borrows a status code and response body supplied by the host.
    pub fn new(status: u16, body: &'a [u8]) -> Self {
        Self { status, body }
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Returns the response body without copying it.
    pub fn body(&self) -> &'a [u8] {
        self.body
    }
}
