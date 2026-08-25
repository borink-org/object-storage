use core::str;

/// A request that borrows its URL and header values from your buffer.
///
/// Send this with the HTTP client of your choice. Read the method, the URL,
/// the headers and the body, and give them to the client.
///
/// The encoding methods copy every byte of the head into your buffer. The head
/// therefore borrows nothing that you passed to them, and each of those
/// arguments can be a temporary. The body is the one exception: it stays where
/// you put it and this request borrows it.
///
/// # Lifetime
///
/// The request borrows the buffer, so the buffer stays locked until you drop
/// the request. Encode, send, then drop the request to use the buffer again.
/// The compiler enforces this order.
#[derive(Debug, Clone, Copy)]
pub struct WireRequest<'r> {
    method: &'static str,
    url: &'r str,
    headers: [(&'static str, &'r str); MAX_HEADERS],
    header_count: usize,
    body: &'r [u8],
}

// authorization, x-ms-date, x-ms-version, x-ms-blob-type, content-length and
// one condition: the longest head this crate writes.
const MAX_HEADERS: usize = 6;

impl<'r> WireRequest<'r> {
    pub(crate) fn new(method: &'static str, url: &'r str, body: &'r [u8]) -> Self {
        Self {
            method,
            url,
            headers: [("", ""); MAX_HEADERS],
            header_count: 0,
            body,
        }
    }

    pub(crate) fn push(&mut self, name: &'static str, value: &'r str) {
        self.headers[self.header_count] = (name, value);
        self.header_count += 1;
    }

    /// Returns the HTTP method.
    pub fn method(&self) -> &'static str {
        self.method
    }

    /// Returns the complete object URL.
    pub fn url(&self) -> &'r str {
        self.url
    }

    /// Returns an iterator over the request headers.
    ///
    /// The order of the headers does not matter to Azure.
    pub fn headers(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.headers[..self.header_count].iter().copied()
    }

    /// Returns the request body.
    ///
    /// A read has no body, so this is empty for a read.
    pub fn body(&self) -> &'r [u8] {
        self.body
    }
}

// The writer keeps counting after capacity is exhausted, so one pass produces
// either the request or its exact requirement. Partial bytes are never returned.
pub(crate) struct Writer<'a> {
    bytes: &'a mut [u8],
    position: usize,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn push(&mut self, value: &[u8]) {
        let end = self.position + value.len();
        if end <= self.bytes.len() {
            self.bytes[self.position..end].copy_from_slice(value);
        }
        self.position = end;
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn finish(self) -> Option<&'a [u8]> {
        (self.position <= self.bytes.len()).then(|| &self.bytes[..self.position])
    }
}

pub(crate) fn text(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).expect("request construction writes UTF-8")
}

// Unlike the fixed-width date fields in `time`, range offsets need the shortest
// decimal representation. This buffer owns that representation without allocating.
pub(crate) struct U64Decimal {
    bytes: [u8; 20],
    start: usize,
}

impl U64Decimal {
    pub(crate) fn new(mut value: u64) -> Self {
        let mut bytes = [0; 20];
        let mut start = bytes.len();
        loop {
            start -= 1;
            bytes[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        Self { bytes, start }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[self.start..]
    }
}

#[cfg(test)]
mod tests {
    use super::{U64Decimal, Writer};

    #[test]
    fn an_exactly_sized_writer_returns_the_written_bytes() {
        let mut bytes = [0; 5];
        let mut writer = Writer::new(&mut bytes);
        writer.push(b"one");
        writer.push("\u{e9}".as_bytes());

        assert_eq!(writer.position(), 5);
        assert_eq!(writer.finish().unwrap(), "one\u{e9}".as_bytes());
    }

    #[test]
    fn an_undersized_writer_still_reports_the_exact_requirement() {
        let mut bytes = [0; 3];
        let mut writer = Writer::new(&mut bytes);
        writer.push(b"four");
        writer.push(b" more");

        assert_eq!(writer.position(), 9);
        assert!(writer.finish().is_none());
    }

    #[test]
    fn an_empty_writer_reports_the_whole_requirement() {
        let mut writer = Writer::new(&mut []);
        writer.push(b"measured");

        assert_eq!(writer.position(), 8);
        assert!(writer.finish().is_none());
    }

    #[test]
    fn formats_the_full_u64_range() {
        assert_eq!(U64Decimal::new(0).as_bytes(), b"0");
        assert_eq!(
            U64Decimal::new(u64::MAX).as_bytes(),
            b"18446744073709551615"
        );
    }
}
