use core::str;

pub struct RequestWorkspace<'a> {
    bytes: &'a mut [u8],
}

impl<'a> RequestWorkspace<'a> {
    pub fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes }
    }

    pub fn capacity(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        self.bytes
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    url: &'a str,
    headers: [(&'static str, &'a str); 3],
}

impl<'a> Request<'a> {
    pub(crate) fn new(
        url: &'a str,
        authorization: &'a str,
        date: &'a str,
        version: &'static str,
    ) -> Self {
        Self {
            url,
            headers: [
                ("authorization", authorization),
                ("x-ms-date", date),
                ("x-ms-version", version),
            ],
        }
    }

    pub fn method(&self) -> &'static str {
        "GET"
    }

    pub fn url(&self) -> &'a str {
        self.url
    }

    pub fn headers(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.headers.iter().copied()
    }
}

pub(crate) struct Writer<'a> {
    bytes: &'a mut [u8],
    position: usize,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn push(&mut self, value: &str) {
        let end = self.position + value.len();
        self.bytes[self.position..end].copy_from_slice(value.as_bytes());
        self.position = end;
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn finish(self) -> &'a [u8] {
        &self.bytes[..self.position]
    }
}

pub(crate) fn text(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).expect("request construction writes UTF-8")
}
