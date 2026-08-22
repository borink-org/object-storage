/// HTTP response metadata supplied by the host before it reads the body.
#[derive(Debug, Clone, Copy)]
pub struct Response<'a> {
    status: u16,
    headers: &'a [(&'a str, &'a str)],
    body: &'a [u8],
}

impl<'a> Response<'a> {
    /// Borrows response metadata and any body bytes already read by the host.
    ///
    /// A successful GET does not require body bytes during interpretation, so
    /// the host may pass an empty slice and read the body afterward.
    pub fn new(status: u16, headers: &'a [(&'a str, &'a str)], body: &'a [u8]) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Returns the first header value with this case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&'a str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| *value)
    }

    pub(crate) fn body(&self) -> &'a [u8] {
        self.body
    }
}
