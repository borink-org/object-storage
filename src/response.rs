/// HTTP response metadata supplied by the host before it reads the body.
#[derive(Debug, Clone, Copy)]
pub struct Response<'a> {
    status: u16,
    headers: &'a [(&'a str, &'a str)],
}

impl<'a> Response<'a> {
    /// Borrows a status code and response headers supplied by the host.
    pub fn new(status: u16, headers: &'a [(&'a str, &'a str)]) -> Self {
        Self { status, headers }
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
}
