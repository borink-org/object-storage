use core::str;

/// A request head that borrows its URL and header values from your buffer.
///
/// Send this with the HTTP client of your choice. Read the method, the URL and
/// the headers, and give them to the client.
///
/// [`Blobs::encode_get`](crate::Blobs::encode_get) copies every byte of the
/// head into your buffer. The head therefore borrows nothing that you passed
/// to that method, and each of those arguments can be a temporary.
///
/// # Lifetime
///
/// The head borrows the buffer, so the buffer stays locked until you drop the
/// head. Encode, send, then drop the head to use the buffer again. The
/// compiler enforces this order.
#[derive(Debug, Clone, Copy)]
pub struct WireRequest<'r> {
    method: &'static str,
    url: &'r str,
    headers: [(&'static str, &'r str); 5],
    header_count: usize,
}

impl<'r> WireRequest<'r> {
    pub(crate) fn new(
        method: &'static str,
        url: &'r str,
        authorization: &'r str,
        date: &'r str,
        version: &'static str,
        range: Option<&'r str>,
        condition: Option<(&'static str, &'r str)>,
    ) -> Self {
        let mut headers = [("", ""); 5];
        headers[..3].copy_from_slice(&[
            ("authorization", authorization),
            ("x-ms-date", date),
            ("x-ms-version", version),
        ]);
        let mut header_count = 3;
        if let Some(value) = range {
            headers[header_count] = ("range", value);
            header_count += 1;
        }
        if let Some(value) = condition {
            headers[header_count] = value;
            header_count += 1;
        }
        Self {
            method,
            url,
            headers,
            header_count,
        }
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
