/// HTTP response metadata supplied by the host before it reads the body.
#[derive(Debug, Clone, Copy)]
pub struct Response {
    status: u16,
}

impl Response {
    /// Wraps the HTTP status code supplied by the host.
    pub fn new(status: u16) -> Self {
        Self { status }
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }
}
