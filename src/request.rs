use core::str;

use crate::Payload;

/// The most headers that this crate writes into one request head.
///
/// `authorization`, `x-ms-date`, `x-ms-version`, `content-length`,
/// `x-ms-blob-type` and one condition. [`WireRequest::headers`] returns at
/// most this many.
///
/// This is not a limit on your request. Headers that you add yourself, such as
/// one a proxy needs, go to your HTTP client and are not counted here.
pub const MAX_HEADERS: usize = 6;

/// A range of bytes, as an offset from the start of a buffer.
///
/// [`WireRequest::url_span`] and [`WireRequest::header_spans`] return these,
/// for a host that addresses the request head by range instead of by slice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Span {
    /// The offset of the first byte.
    pub start: usize,
    /// The number of bytes.
    pub len: usize,
}

impl Span {
    fn of(self, bytes: &str) -> &str {
        &bytes[self.start..self.start + self.len]
    }
}

/// The HTTP method of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u8)]
pub enum Method {
    /// `GET`.
    Get = 1,
    /// `HEAD`.
    Head = 2,
    /// `PUT`.
    Put = 3,
    /// `DELETE`.
    Delete = 4,
}

impl Method {
    /// Returns the method as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

impl core::fmt::Display for Method {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A request that borrows its head from your buffer.
///
/// Send this with the HTTP client of your choice. Read the method, the URL,
/// the headers and the body, and give them to the client.
///
/// The encoding methods copy every byte of the head into your buffer,
/// including each header name. The head therefore borrows nothing that you
/// passed to them, and each of those arguments can be a temporary. The content
/// is the one exception: it stays where you put it, and this request borrows
/// it or leaves it to you.
///
/// # Lifetime
///
/// The request borrows the buffer, so the buffer stays locked until you drop
/// the request. Encode, send, then drop the request to use the buffer again.
/// The compiler enforces this order.
#[derive(Debug, Clone, Copy)]
pub struct WireRequest<'r> {
    bytes: &'r str,
    method: Method,
    url: Span,
    headers: [(Span, Span); MAX_HEADERS],
    header_count: usize,
    payload: Payload<'r>,
}

impl<'r> WireRequest<'r> {
    /// Returns the HTTP method.
    pub fn method(&self) -> Method {
        self.method
    }

    /// Returns the complete object URL.
    pub fn url(&self) -> &'r str {
        self.url.of(self.bytes)
    }

    /// Returns an iterator over the request headers.
    ///
    /// The order of the headers does not matter to Azure.
    pub fn headers(&self) -> impl ExactSizeIterator<Item = (&'r str, &'r str)> {
        let bytes = self.bytes;
        self.header_spans()
            .map(move |(name, value)| (name.of(bytes), value.of(bytes)))
    }

    /// Returns the content of the request.
    ///
    /// A read has no content, so this is an empty [`Payload::Slice`] for a
    /// read. For a write of streamed content this states the length that you
    /// must send, and carries no bytes.
    pub fn payload(&self) -> Payload<'r> {
        self.payload
    }

    /// Returns the URL as a range of the buffer that holds the head.
    pub fn url_span(&self) -> Span {
        self.url
    }

    /// Returns each header name and value as a range of that same buffer.
    pub fn header_spans(&self) -> impl ExactSizeIterator<Item = (Span, Span)> {
        let headers = self.headers;
        (0..self.header_count).map(move |index| headers[index])
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

// The head as it is written: every byte of it goes into the caller's buffer,
// and every part of it is recorded as a range of that buffer.
pub(crate) struct HeadWriter<'a> {
    out: Writer<'a>,
    url: Span,
    headers: [(Span, Span); MAX_HEADERS],
    count: usize,
}

impl<'a> HeadWriter<'a> {
    pub(crate) fn new(bytes: &'a mut [u8]) -> Self {
        Self {
            out: Writer::new(bytes),
            url: Span::default(),
            headers: [(Span::default(), Span::default()); MAX_HEADERS],
            count: 0,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.out.position()
    }

    pub(crate) fn url(&mut self, write: impl FnOnce(&mut Writer<'a>)) {
        self.url = self.part(write);
    }

    pub(crate) fn header(&mut self, name: &str, write: impl FnOnce(&mut Writer<'a>)) {
        let name = self.part(|out| out.push(name.as_bytes()));
        let value = self.part(write);
        self.headers[self.count] = (name, value);
        self.count += 1;
    }

    pub(crate) fn finish(self, method: Method, payload: Payload<'a>) -> Option<WireRequest<'a>> {
        let (url, headers, header_count) = (self.url, self.headers, self.count);
        Some(WireRequest {
            bytes: text(self.out.finish()?),
            method,
            url,
            headers,
            header_count,
            payload,
        })
    }

    fn part(&mut self, write: impl FnOnce(&mut Writer<'a>)) -> Span {
        let start = self.out.position();
        write(&mut self.out);
        Span {
            start,
            len: self.out.position() - start,
        }
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
