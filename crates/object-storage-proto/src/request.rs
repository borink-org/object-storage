use core::str;

/// A range of bytes, as an offset from the start of a buffer.
///
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Span {
    /// The offset of the first byte.
    pub start: usize,
    /// The number of bytes.
    pub len: usize,
}

impl Span {}

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

    #[inline(always)]
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
    #[inline(always)]
    pub(crate) fn part(&mut self, write: impl FnOnce(&mut Self)) -> Span {
        let start = self.position;
        write(self);
        // push only increases or saturates position, so subtraction cannot underflow.
        Span {
            start,
            len: self.position - start,
        }
    }

    // Writes again the bytes of `span`, which this writer wrote before. If
    // they did not fit, it only counts them.
    #[inline(always)]
    pub(crate) fn repeat(&mut self, span: Span) {
        let end = self.position.saturating_add(span.len);
        if end <= self.bytes.len() {
            self.bytes
                .copy_within(span.start..span.start + span.len, self.position);
        }
        self.position = end;
    }

    // Whether every byte pushed so far is in the buffer.
    pub(crate) fn fits(&self) -> bool {
        self.position <= self.bytes.len()
    }

    // The bytes written so far that are in the buffer.
    pub(crate) fn written(&self) -> &[u8] {
        &self.bytes[..self.position.min(self.bytes.len())]
    }
}

// Copies `from` into `to`, which is as long. A copy of up to 32 bytes is two
// overlapping copies of a fixed length, which compile to a few moves, where
// `copy_from_slice` of a length known only at run time calls the C library's
// `memcpy`, which costs more than the copy for so few bytes. Slicing `to` to
// that length first lets the compiler drop the bounds checks of each copy.
#[inline(always)]
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

// The text of bytes that this crate wrote. A request written from bytes that
// are not its template's can cut a character in two; its text then ends
// before that character, rather than the program.
pub(crate) fn text(bytes: &[u8]) -> &str {
    match str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default(),
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

// The two decimal digits of each number below 100.
static DIGIT_PAIRS: [[u8; 2]; 100] = {
    let mut pairs = [[0; 2]; 100];
    let mut value = 0;
    while value < 100 {
        pairs[value] = [b'0' + (value / 10) as u8, b'0' + (value % 10) as u8];
        value += 1;
    }
    pairs
};

// Unlike the fixed-width date fields in `time`, range offsets need the shortest
// decimal representation. This buffer owns that representation without allocating.
pub(crate) struct U64Decimal {
    bytes: [u8; 20],
    start: usize,
}

impl U64Decimal {
    // Kept out of line: the compiler unrolls the digit loop, and inlined at
    // each of the 21 places that write a number it made one range header
    // alone 2,400 instructions long.
    #[inline(never)]
    pub(crate) fn new(mut value: u64) -> Self {
        let mut bytes = [0; 20];
        let mut start = bytes.len();
        // Two digits a division, from a table of them. A u64 has at most 20
        // decimal digits, so `start` stays in the array.
        while value >= 100 {
            let pair = DIGIT_PAIRS[(value % 100) as usize];
            value /= 100;
            start -= 2;
            bytes[start..start + 2].copy_from_slice(&pair);
        }
        if value >= 10 {
            start -= 2;
            bytes[start..start + 2].copy_from_slice(&DIGIT_PAIRS[value as usize]);
        } else {
            start -= 1;
            bytes[start] = b'0' + value as u8;
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
    fn an_exactly_sized_writer_holds_the_written_bytes() {
        let mut bytes = [0; 5];
        let mut writer = Writer::new(&mut bytes);
        writer.push(b"one");
        writer.push("\u{e9}".as_bytes());

        assert_eq!(writer.position(), 5);
        assert!(writer.fits());
        assert_eq!(bytes, "one\u{e9}".as_bytes());
    }

    #[test]
    fn an_undersized_writer_still_reports_the_exact_requirement() {
        let mut bytes = [0; 3];
        let mut writer = Writer::new(&mut bytes);
        writer.push(b"four");
        writer.push(b" more");

        assert_eq!(writer.position(), 9);
        assert!(!writer.fits());
    }

    #[test]
    fn an_empty_writer_reports_the_whole_requirement() {
        let mut writer = Writer::new(&mut []);
        writer.push(b"measured");

        assert_eq!(writer.position(), 8);
        assert!(!writer.fits());
    }

    #[test]
    fn an_unrepresentable_requirement_saturates_without_wrapping() {
        let mut writer = Writer::new(&mut []);
        writer.position = usize::MAX - 1;
        writer.push(b"over");
        assert_eq!(writer.position(), usize::MAX);
        assert!(!writer.fits());
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
