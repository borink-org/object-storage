use core::str;

use crate::Payload;

/// The storage required to encode a request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestSize {
    /// Bytes for the URL, headers and any generated body.
    pub bytes: usize,
    /// Header descriptor slots.
    pub headers: usize,
}

/// One header's name and value in the request buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct HeaderSpan {
    /// The header name.
    pub name: Span,
    /// The header value.
    pub value: Span,
}

/// A range of bytes, as an offset from the start of a buffer.
///
/// [`WireRequest::url_span`] and [`WireRequest::header_spans`] return these,
/// for a host that addresses the request head by range instead of by slice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Span {
    /// The offset of the first byte.
    pub start: usize,
    /// The number of bytes.
    pub len: usize,
}

impl Span {
    // The text of this part of a head. Only the part is checked as UTF-8, so
    // a host that reads the head by range pays for no check.
    fn of(self, bytes: &[u8]) -> &str {
        // HeadWriter bounds start + len by the finished buffer's length.
        text(&bytes[self.start..self.start + self.len])
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
    /// `POST`. S3 creates and commits an upload with it.
    Post = 5,
}

impl Method {
    /// Returns the method as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Post => "POST",
        }
    }
}

impl core::fmt::Display for Method {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A request borrowing caller-owned byte storage and header descriptors.
///
/// Send this with the HTTP client of your choice. Read the method, the URL,
/// the headers and the body, and give them to the client.
///
/// The encoding methods copy every byte of the head into your buffer,
/// including each header name. The head therefore borrows nothing that you
/// passed to them, and each of those arguments can be a temporary. The content
/// stays where you put it for a stage or PUT. A commit writes its body after
/// the head in the same buffer.
///
/// # Lifetime
///
/// The request borrows both storage regions until transport consumption ends.
/// Drop the request before reusing either region.
#[derive(Debug, Clone, Copy)]
pub struct WireRequest<'r> {
    bytes: &'r [u8],
    method: Method,
    url: Span,
    headers: &'r [HeaderSpan],
    payload: Payload<'r>,
    body: Option<Span>,
}

impl<'r> WireRequest<'r> {
    /// Returns the caller-owned descriptor array used by this request.
    pub fn header_descriptors(&self) -> &'r [HeaderSpan] {
        self.headers
    }

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

    /// Returns the range of the request buffer that holds the body that this
    /// crate wrote. A commit writes its block list there. Returns [`None`] for
    /// a payload that you supplied, as a slice or as streamed content.
    pub fn body_span(&self) -> Option<Span> {
        self.body
    }

    /// Returns the URL as a range of the buffer that holds the head.
    pub fn url_span(&self) -> Span {
        self.url
    }

    /// Returns each header name and value as a range of that same buffer.
    pub fn header_spans(&self) -> impl ExactSizeIterator<Item = (Span, Span)> {
        self.headers.iter().copied().map(|header| {
            let HeaderSpan { name, value } = header;
            (name, value)
        })
    }
}

// The writer keeps counting after capacity is exhausted, so one pass produces
// either the request or its exact requirement. Partial bytes are never returned.
// Receives the text of a request as it is written: the request buffer, a
// count of its length, or a checksum or SHA-256 of it. One writer serves them
// all, so the signed bytes are the sent bytes.
pub(crate) trait ByteSink {
    // Takes the next piece of the text.
    fn push(&mut self, bytes: &[u8]);
}

impl ByteSink for Writer<'_> {
    fn push(&mut self, bytes: &[u8]) {
        Writer::push(self, bytes);
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

    #[inline]
    pub(crate) fn push(&mut self, value: &[u8]) {
        // Counting continues past capacity; aliased inputs can exceed usize
        // in aggregate. Saturation preserves monotonicity and cannot fit a slice.
        let end = self.position.saturating_add(value.len());
        if end <= self.bytes.len() {
            copy_short(&mut self.bytes[self.position..end], value);
        }
        self.position = end;
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    // Writes with `write` and returns where its bytes are.
    pub(crate) fn part(&mut self, write: impl FnOnce(&mut Self)) -> Span {
        let start = self.position;
        write(self);
        // push only increases or saturates position, so subtraction cannot underflow.
        Span {
            start,
            len: self.position - start,
        }
    }

    // Whether every byte pushed so far is in the buffer.
    fn fits(&self) -> bool {
        self.position <= self.bytes.len()
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
    headers: &'a mut [HeaderSpan],
    count: usize,
}

impl<'a> HeadWriter<'a> {
    pub(crate) fn new(bytes: &'a mut [u8], headers: &'a mut [HeaderSpan]) -> Self {
        Self {
            out: Writer::new(bytes),
            url: Span::default(),
            headers,
            count: 0,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.out.position()
    }

    // Whether every byte and every header so far is in the buffers, so that
    // `Self::written` and `Self::header_spans` can read them.
    pub(crate) fn fits(&self) -> bool {
        self.out.fits() && self.count <= self.headers.len()
    }

    // The bytes of a part already written, in a head that fits.
    pub(crate) fn written(&self, span: Span) -> &[u8] {
        &self.out.bytes[span.start..span.start + span.len]
    }

    // Writes `bytes` over a part already written, as long, in a head that
    // fits.
    pub(crate) fn overwrite(&mut self, span: Span, bytes: &[u8]) {
        self.out.bytes[span.start..span.start + span.len].copy_from_slice(bytes);
    }

    // The number of headers written so far.
    pub(crate) fn header_count(&self) -> usize {
        self.count
    }

    // The headers written so far, from the one at `first`, in a head that
    // fits.
    pub(crate) fn header_spans(&self, first: usize) -> &[HeaderSpan] {
        &self.headers[first..self.count]
    }

    pub(crate) fn capacity(&self) -> crate::CapacityError {
        crate::CapacityError {
            required: self.position(),
            required_headers: self.count,
        }
    }

    pub(crate) fn url(&mut self, write: impl FnOnce(&mut Writer<'a>)) {
        self.url = self.part(write);
    }

    pub(crate) fn header(&mut self, name: &str, value: impl HeaderValue) {
        self.header_parts(|out| out.push(name.as_bytes()), |out| value.write_to(out));
    }

    // A header whose value `write` writes from state of its own, such as
    // the signer's.
    pub(crate) fn header_with(&mut self, name: &str, write: impl FnOnce(&mut Writer<'a>)) {
        self.header_parts(|out| out.push(name.as_bytes()), write);
    }

    pub(crate) fn header_parts(
        &mut self,
        name: impl FnOnce(&mut Writer<'a>),
        value: impl FnOnce(&mut Writer<'a>),
    ) {
        let name = self.part(name);
        let value = self.part(value);
        self.record(name, value);
    }

    // A field written as a line of a SigV4 canonical request: `name:value`,
    // after a `\n` that ends the field before, which the spans leave out. A
    // run of such fields is the signed headers' lines but for the last `\n`,
    // so a signer hashes the run in one piece. It costs two bytes a field, so
    // only a client that signs writes its fields so.
    pub(crate) fn line_with(&mut self, name: &str, value: impl FnOnce(&mut Writer<'a>)) {
        self.line_parts(|out| out.push(name.as_bytes()), value);
    }

    pub(crate) fn line_parts(
        &mut self,
        name: impl FnOnce(&mut Writer<'a>),
        value: impl FnOnce(&mut Writer<'a>),
    ) {
        self.out.push(b"\n");
        let name = self.part(name);
        self.out.push(b":");
        let value = self.part(value);
        self.record(name, value);
    }

    fn record(&mut self, name: Span, value: Span) {
        if let Some(slot) = self.headers.get_mut(self.count) {
            *slot = HeaderSpan { name, value };
        }
        // Option iterators are not bounded by descriptor capacity; keep counting
        // without wrapping, just as Writer does for bytes.
        self.count = self.count.saturating_add(1);
    }

    pub(crate) fn finish(self, method: Method, payload: Payload<'a>) -> Option<WireRequest<'a>> {
        self.finish_with_body(method, payload, None)
    }

    pub(crate) fn body(&mut self, write: impl FnOnce(&mut Writer<'a>)) -> Span {
        self.part(write)
    }

    pub(crate) fn finish_with_body(
        self,
        method: Method,
        payload: Payload<'a>,
        body: Option<Span>,
    ) -> Option<WireRequest<'a>> {
        if self.count > self.headers.len() {
            return None;
        }
        let (url, headers) = (self.url, &self.headers[..self.count]);
        let bytes = self.out.finish()?;
        let head_end = body.map_or(bytes.len(), |span| span.start);
        // body() records writer positions; finish() proved the entire body fits.
        let payload = body.map_or(payload, |span| {
            Payload::Slice(&bytes[span.start..span.start + span.len])
        });
        Some(WireRequest {
            bytes: &bytes[..head_end],
            method,
            url,
            headers,
            payload,
            body,
        })
    }

    fn part(&mut self, write: impl FnOnce(&mut Writer<'a>)) -> Span {
        self.out.part(write)
    }
}

// Copies `from` into `to`, which is as long. A copy of up to 32 bytes is two
// overlapping copies of a fixed length, which compile to a few moves, where
// `copy_from_slice` of a length known only at run time calls the C library's
// `memcpy`, which costs more than the copy for so few bytes. Slicing `to` to
// that length first lets the compiler drop the bounds checks of each copy.
#[inline]
pub(crate) fn copy_short(to: &mut [u8], from: &[u8]) {
    let len = from.len();
    let to = &mut to[..len];
    match len {
        0 => {}
        1..=3 => {
            to[0] = from[0];
            to[len / 2] = from[len / 2];
            to[len - 1] = from[len - 1];
        }
        4..=7 => {
            to[..4].copy_from_slice(&from[..4]);
            to[len - 4..].copy_from_slice(&from[len - 4..]);
        }
        8..=16 => {
            to[..8].copy_from_slice(&from[..8]);
            to[len - 8..].copy_from_slice(&from[len - 8..]);
        }
        17..=32 => {
            to[..16].copy_from_slice(&from[..16]);
            to[len - 16..].copy_from_slice(&from[len - 16..]);
        }
        _ => to.copy_from_slice(from),
    }
}

pub(crate) fn text(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).expect("request construction writes UTF-8")
}

// Whether an encoder writes a request, or only measures it. A
// `*_requirements` function encodes into an empty buffer to learn the room
// that a request needs, so an empty buffer is a measuring pass, which skips
// the work that only the bytes need, such as a signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    Measure,
    Write,
}

impl Pass {
    pub(crate) fn of(buf: &[u8]) -> Self {
        if buf.is_empty() {
            Self::Measure
        } else {
            Self::Write
        }
    }
}

// The value of one header, which writes itself into the request head: bytes
// as they are, a number, or a value that a type of its own assembles in
// place, so that no value is built anywhere else first.
pub(crate) trait HeaderValue {
    fn write_to(self, out: &mut dyn ByteSink);
}

impl HeaderValue for &[u8] {
    fn write_to(self, out: &mut dyn ByteSink) {
        out.push(self);
    }
}

impl<const N: usize> HeaderValue for &[u8; N] {
    fn write_to(self, out: &mut dyn ByteSink) {
        out.push(self);
    }
}

impl HeaderValue for &str {
    fn write_to(self, out: &mut dyn ByteSink) {
        out.push(self.as_bytes());
    }
}

impl HeaderValue for U64Decimal {
    fn write_to(self, out: &mut dyn ByteSink) {
        out.push(self.as_bytes());
    }
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
            // A u64 has at most 20 decimal digits; each division consumes one.
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
    use super::{HeadWriter, HeaderSpan, Method, U64Decimal, Writer};

    #[test]
    fn header_capacity_is_independent_of_byte_capacity() {
        for (byte_capacity, header_capacity) in [(0, 1), (9, 0), (9, 1)] {
            let mut bytes = [0; 9];
            let mut headers = [HeaderSpan::default(); 1];
            let mut writer =
                HeadWriter::new(&mut bytes[..byte_capacity], &mut headers[..header_capacity]);
            writer.header("name", b"value");
            let capacity = writer.capacity();
            assert_eq!(capacity.required, 9);
            assert_eq!(capacity.required_headers, 1);
            assert_eq!(
                writer
                    .finish(Method::Get, crate::Payload::Slice(b""))
                    .is_some(),
                byte_capacity == 9 && header_capacity == 1
            );
        }
    }

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
    fn an_unrepresentable_requirement_saturates_without_wrapping() {
        let mut writer = Writer::new(&mut []);
        writer.position = usize::MAX - 1;
        writer.push(b"over");
        assert_eq!(writer.position(), usize::MAX);
        assert!(writer.finish().is_none());

        let mut head = HeadWriter::new(&mut [], &mut []);
        head.count = usize::MAX;
        head.header("name", b"value");
        assert_eq!(head.capacity().required_headers, usize::MAX);
        assert!(
            head.finish(Method::Get, crate::Payload::Slice(b""))
                .is_none()
        );
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
