#[derive(Debug, Clone, Copy)]
pub struct Response<'a> {
    status: u16,
    body: &'a [u8],
}

impl<'a> Response<'a> {
    pub fn new(status: u16, body: &'a [u8]) -> Self {
        Self { status, body }
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn body(&self) -> &'a [u8] {
        self.body
    }
}
