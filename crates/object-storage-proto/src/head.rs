use crate::encoding::parse_decimal;
use crate::{
    BodyWindow, Error, GetHeadOutcome, GetKind, GetShape, ObjectMeta, RequestedRange,
    ResponseFault, Result,
};

/// The response header values that this crate reads.
///
/// One head describes any response, from a read or from a write. Each field
/// holds the value of one header. Fill the fields with
/// [`ResponseHead::from_headers`], or set them directly from a streaming
/// parser.
///
/// The values are byte slices, not strings. A server can send a header value
/// that is not UTF-8, and this crate carries such a value instead of
/// discarding it.
///
/// # Lifetime
///
/// The header values must stay valid for as long as you use the
/// [`GetHeadOutcome`](crate::GetHeadOutcome) that
/// [`azure::accept_get_head`](crate::azure::accept_get_head) returns from
/// them. The type is [`Copy`], so you can keep the head after you read the
/// response body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResponseHead<'h> {
    /// The HTTP status code.
    pub status: u16,
    /// The value of the `Content-Length` header.
    pub content_length: Option<&'h [u8]>,
    /// The value of the `Content-Range` header.
    pub content_range: Option<&'h [u8]>,
    /// The value of the `Content-Encoding` header.
    ///
    /// This crate does not decode the body. It returns this value so that you
    /// know how the bytes are encoded.
    pub content_encoding: Option<&'h [u8]>,
    /// The value of the `Content-Type` header, without an inferred default.
    pub content_type: Option<&'h [u8]>,
    /// The value of the `Content-MD5` header.
    pub content_md5: Option<&'h [u8]>,
    /// The value of the `Content-Language` header.
    pub content_language: Option<&'h [u8]>,
    /// The value of the `Content-Disposition` header.
    pub content_disposition: Option<&'h [u8]>,
    /// The value of the `Cache-Control` header.
    pub cache_control: Option<&'h [u8]>,
    /// The value of the `x-amz-storage-class` or `x-ms-access-tier` header.
    pub storage_class: Option<&'h [u8]>,
    /// The value of the `ETag` header.
    pub e_tag: Option<&'h [u8]>,
    /// The value of the `Last-Modified` header.
    pub last_modified: Option<&'h [u8]>,
    /// The value of the `x-ms-version-id` or `x-amz-version-id` header.
    pub version: Option<&'h [u8]>,
    /// The value of the `x-ms-snapshot` header, which names the snapshot
    /// that an Azure Snapshot Blob took.
    pub snapshot: Option<&'h [u8]>,
    /// The value of the `x-ms-copy-id` header, which names the last Azure
    /// copy onto the object.
    pub copy_id: Option<&'h [u8]>,
    /// The value of the `x-ms-copy-status` header: the state of that copy.
    pub copy_status: Option<&'h [u8]>,
    /// The value of the `x-amz-restore` or `x-ms-archive-status` header:
    /// the state of a restore from an archive.
    pub restore_status: Option<&'h [u8]>,
    /// The value of the `x-ms-error-code` header.
    ///
    /// Azure names the error here. The methods that read a head return the
    /// named error with the outcome.
    pub error_code: Option<&'h [u8]>,
    /// The value of the `x-ms-request-id` or `x-amz-request-id` header.
    ///
    /// The service assigns one identifier to each request. Record it: the
    /// service's support uses it to find the request in the service logs.
    pub request_id: Option<&'h [u8]>,
    /// The value of the `x-amz-id-2` header.
    ///
    /// S3 sends this second identifier beside `request_id`. AWS support asks
    /// for both.
    pub extended_request_id: Option<&'h [u8]>,
}

impl<'h> ResponseHead<'h> {
    /// Creates a head with this status and no header values.
    pub fn new(status: u16) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }

    /// Creates a head from borrowed name-value pairs.
    ///
    /// This method reads `headers` immediately and keeps only the values that
    /// it needs. Header names are compared without case. If a name occurs more
    /// than once, the first value wins.
    ///
    /// Use this method if you already hold every response header. If you parse
    /// the response as a stream, set the fields directly instead.
    pub fn from_headers(
        status: u16,
        headers: impl IntoIterator<Item = (&'h str, &'h [u8])>,
    ) -> Self {
        let mut head = Self::new(status);
        for (name, value) in headers {
            head.insert(name, value);
        }
        head
    }

    /// Consumes one parsed header without copying its value. The first value wins.
    pub fn insert(&mut self, name: &str, value: &'h [u8]) {
        if let Some(slot) = self.slot(name)
            && slot.is_none()
        {
            *slot = Some(value);
        }
    }

    // The field that the header `name` fills, in any case, or `None` for a
    // header that a head does not keep. A name is compared only with the
    // known names of its length, so most headers that a service sends and a
    // head does not keep cost one comparison of lengths.
    fn slot(&mut self, name: &str) -> Option<&mut Option<&'h [u8]>> {
        let name = name.as_bytes();
        let is = |known: &KeptHeader| known.is(name);
        Some(match name.len() {
            4 if is(const { &KeptHeader::new("etag") }) => &mut self.e_tag,
            10 if is(const { &KeptHeader::new("x-amz-id-2") }) => &mut self.extended_request_id,
            11 if is(const { &KeptHeader::new("content-md5") }) => &mut self.content_md5,
            12 if is(const { &KeptHeader::new("content-type") }) => &mut self.content_type,
            12 if is(const { &KeptHeader::new("x-ms-copy-id") }) => &mut self.copy_id,
            13 if is(const { &KeptHeader::new("content-range") }) => &mut self.content_range,
            13 if is(const { &KeptHeader::new("cache-control") }) => &mut self.cache_control,
            13 if is(const { &KeptHeader::new("last-modified") }) => &mut self.last_modified,
            13 if is(const { &KeptHeader::new("x-ms-snapshot") }) => &mut self.snapshot,
            13 if is(const { &KeptHeader::new("x-amz-restore") }) => &mut self.restore_status,
            14 if is(const { &KeptHeader::new("content-length") }) => &mut self.content_length,
            15 if is(const { &KeptHeader::new("x-ms-version-id") }) => &mut self.version,
            15 if is(const { &KeptHeader::new("x-ms-error-code") }) => &mut self.error_code,
            15 if is(const { &KeptHeader::new("x-ms-request-id") }) => &mut self.request_id,
            16 if is(const { &KeptHeader::new("content-encoding") }) => &mut self.content_encoding,
            16 if is(const { &KeptHeader::new("content-language") }) => &mut self.content_language,
            16 if is(const { &KeptHeader::new("x-ms-access-tier") }) => &mut self.storage_class,
            16 if is(const { &KeptHeader::new("x-amz-version-id") }) => &mut self.version,
            16 if is(const { &KeptHeader::new("x-ms-copy-status") }) => &mut self.copy_status,
            16 if is(const { &KeptHeader::new("x-amz-request-id") }) => &mut self.request_id,
            19 if is(const { &KeptHeader::new("content-disposition") }) => {
                &mut self.content_disposition
            }
            19 if is(const { &KeptHeader::new("x-amz-storage-class") }) => &mut self.storage_class,
            19 if is(const { &KeptHeader::new("x-ms-archive-status") }) => &mut self.restore_status,
            _ => return None,
        })
    }
}

// A header name that a head keeps, ready to compare: its bytes as words, and
// the case bit `0x20` of each byte that is a letter. A byte matches if it
// equals the known byte once that bit is set, which is exact for each byte,
// and a name of 4 to 24 bytes is two or three overlapping words, so a
// comparison is a few loads whatever the length.
struct KeptHeader {
    len: usize,
    // The words at the start, in the middle (only past 16 bytes) and at the
    // end, each as `(bytes, case bits)`. Below 8 bytes they are 4 wide.
    words: [(u64, u64); 3],
}

impl KeptHeader {
    const fn new(name: &str) -> Self {
        let name = name.as_bytes();
        let len = name.len();
        assert!(4 <= len && len <= 24, "a known name is 4 to 24 bytes");
        let width = if len < 8 { 4 } else { 8 };
        let middle = if len > 16 { 8 } else { 0 };
        Self {
            len,
            words: [
                Self::word(name, 0, width),
                Self::word(name, middle, width),
                Self::word(name, len - width, width),
            ],
        }
    }

    const fn word(name: &[u8], at: usize, width: usize) -> (u64, u64) {
        let (mut bytes, mut fold, mut i) = (0u64, 0u64, 0);
        while i < width {
            let byte = name[at + i];
            bytes |= (byte as u64) << (8 * i);
            if byte.is_ascii_lowercase() {
                fold |= 0x20 << (8 * i);
            }
            i += 1;
        }
        (bytes, fold)
    }

    // Whether `name`, as long as this one, is this name in any case. Inlined
    // with a constant name, the lengths fold away and each word is one load.
    #[inline]
    fn is(&self, name: &[u8]) -> bool {
        let narrow = self.len < 8;
        let width = if narrow { 4 } else { 8 };
        let middle = if self.len > 16 { 8 } else { 0 };
        let word = |at: usize| -> u64 {
            if narrow {
                u32::from_le_bytes(name[at..at + 4].try_into().expect("4 bytes")).into()
            } else {
                u64::from_le_bytes(name[at..at + 8].try_into().expect("8 bytes"))
            }
        };
        let [first, mid, last] = self.words;
        word(0) | first.1 == first.0
            && word(middle) | mid.1 == mid.0
            && word(self.len - width) | last.1 == last.0
    }
}

impl<'h> ResponseHead<'h> {
    // The metadata that the head states, without its size and with
    // `Last-Modified` unread: `read_text_header` reads it.
    pub(crate) fn object_meta(self) -> ObjectMeta<'h> {
        ObjectMeta {
            size: None,
            e_tag: self.e_tag,
            last_modified: None,
            version: self.version,
            content_encoding: self.content_encoding,
            content_type: self.content_type,
            content_md5: self.content_md5,
            content_language: self.content_language,
            content_disposition: self.content_disposition,
            cache_control: self.cache_control,
            storage_class: self.storage_class,
            copy_id: self.copy_id,
            copy_status: self.copy_status,
            restore_status: self.restore_status,
        }
    }
}

// Reads the head of a read that succeeded: the metadata, and the window of
// the object that the body holds, which must be the range that was asked
// for.
pub(crate) fn accept_get_success<'h>(
    shape: GetShape,
    head: ResponseHead<'h>,
) -> Result<GetHeadOutcome<'h>> {
    let content_length = read_decimal_header(head.content_length)?;
    let last_modified = read_text_header(head.last_modified)?;
    let meta = |size| ObjectMeta {
        size,
        last_modified,
        ..head.object_meta()
    };
    if head.status == 200 {
        // An unranged read reads from byte zero, and the service states the whole
        // object length, so `Content-Length` is both the window and the size.
        return Ok(match shape.kind {
            GetKind::Head => GetHeadOutcome::Complete {
                meta: meta(content_length),
            },
            GetKind::Bytes => GetHeadOutcome::Body {
                meta: meta(content_length),
                body: BodyWindow {
                    object_offset: 0,
                    expected_len: content_length,
                    object_size: content_length,
                },
            },
        });
    }
    let value = head
        .content_range
        .ok_or(Error::Response(ResponseFault::Head))?;
    let ContentRange::Satisfied { start, end, total } =
        parse_content_range(value).ok_or(Error::Response(ResponseFault::Head))?
    else {
        return Err(ResponseFault::Head.into());
    };
    // parse_content_range establishes start <= end < u64::MAX.
    let served = end - start + 1;
    if content_length.is_some_and(|length| length != served) {
        return Err(ResponseFault::Head.into());
    }
    // Both services serve the whole satisfiable range, so a short serve is a
    // mismatch: silently accepting it would hand consumers a partial read.
    let requested_start = match shape.range {
        RequestedRange::Bounded { start, .. } | RequestedRange::Offset(start) => start,
        // Where a suffix starts depends on the size, so the head must state it.
        RequestedRange::Suffix(suffix) => match total {
            Some(total) => total.saturating_sub(suffix),
            None => return Err(ResponseFault::Range.into()),
        },
        RequestedRange::Whole => {
            // Public shapes need not have passed through an encoder.
            return Err(ResponseFault::Range.into());
        }
    };
    if start != requested_start {
        return Err(ResponseFault::Range.into());
    }
    if let Some(total) = total {
        let satisfiable = match shape.range {
            RequestedRange::Bounded { end, .. } => end.min(total),
            _ => total,
        };
        // The parser's end < total bound also makes the exclusive end fit.
        if end + 1 != satisfiable {
            return Err(ResponseFault::Range.into());
        }
    }
    Ok(GetHeadOutcome::Body {
        meta: meta(total),
        body: BodyWindow {
            object_offset: start,
            expected_len: Some(served),
            object_size: total,
        },
    })
}

pub(crate) enum ContentRange {
    Satisfied {
        start: u64,
        end: u64,
        total: Option<u64>,
    },
    Unsatisfied {
        total: Option<u64>,
    },
}

// Satisfied ranges establish S <= E < u64::MAX, and E < T when T is known.
// This keeps inclusive lengths and exclusive ends representable even for /*.
pub(crate) fn parse_content_range(value: &[u8]) -> Option<ContentRange> {
    let rest = value.trim_ascii().strip_prefix(b"bytes ")?;
    // `start-end/total` in digits, as both services write it, in one pass.
    // Anything else takes the general reading below, which agrees with this
    // one wherever this one reads a value.
    if let Some(range) = digits_range(rest) {
        return Some(range);
    }
    let slash = rest.iter().rposition(|byte| *byte == b'/')?;
    let (spec, total) = (rest[..slash].trim_ascii(), rest[slash + 1..].trim_ascii());
    let total = match total {
        b"*" => None,
        digits => Some(parse_decimal(digits)?),
    };
    if spec == b"*" {
        return Some(ContentRange::Unsatisfied { total });
    }
    let dash = spec.iter().position(|byte| *byte == b'-')?;
    let start = parse_decimal(&spec[..dash])?;
    let end = parse_decimal(&spec[dash + 1..])?;
    if start > end || end >= total.unwrap_or(u64::MAX) {
        return None;
    }
    Some(ContentRange::Satisfied { start, end, total })
}

// `start-end/total`, digits only, as a satisfied range.
fn digits_range(text: &[u8]) -> Option<ContentRange> {
    let (start, text) = leading_decimal(text)?;
    let (end, text) = leading_decimal(text.strip_prefix(b"-")?)?;
    let (total, text) = leading_decimal(text.strip_prefix(b"/")?)?;
    (text.is_empty() && start <= end && end < total).then_some(ContentRange::Satisfied {
        start,
        end,
        total: Some(total),
    })
}

// The number that `text` starts with, of 1 to 19 digits, which cannot
// overflow, and the rest of `text`. One loop reads the digits and their
// value; a 20th digit is refused, so the wrapping arithmetic never wraps in a
// value that is returned.
fn leading_decimal(text: &[u8]) -> Option<(u64, &[u8])> {
    let (mut value, mut len) = (0u64, 0);
    while let Some(&byte) = text.get(len) {
        let digit = byte.wrapping_sub(b'0');
        if digit > 9 || len == 19 {
            break;
        }
        value = value.wrapping_mul(10).wrapping_add(u64::from(digit));
        len += 1;
    }
    let refused = len == 0 || text.get(len).is_some_and(u8::is_ascii_digit);
    (!refused).then_some((value, &text[len..]))
}

// Reads a header value that carries text. Both services write
// `Last-Modified` in ASCII, so a value that is not UTF-8 is a fault in the
// head.
pub(crate) fn read_text_header(value: Option<&[u8]>) -> Result<Option<&str>> {
    value
        .map(|value| core::str::from_utf8(value).map_err(|_| Error::Response(ResponseFault::Head)))
        .transpose()
}

// Reads a header value that carries a decimal number, such as
// `Content-Length`. A value that is not one is a fault in the head.
pub(crate) fn read_decimal_header(value: Option<&[u8]>) -> Result<Option<u64>> {
    match value {
        None => Ok(None),
        Some(value) => parse_decimal(value.trim_ascii())
            .map(Some)
            .ok_or(Error::Response(ResponseFault::Head)),
    }
}

// The name of the metadata pair that `header` carries under `prefix`, which
// the header name starts with in any case, or `None` for another header.
pub(crate) fn strip_metadata_prefix<'h>(header: &'h str, prefix: &str) -> Option<&'h str> {
    let (start, name) = header.split_at_checked(prefix.len())?;
    (start.eq_ignore_ascii_case(prefix) && !name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::ResponseHead;

    #[test]
    fn retains_the_first_relevant_header_case_insensitively() {
        let head = ResponseHead::from_headers(
            206,
            [
                ("ignored", b"value".as_slice()),
                ("Content-Range", b"bytes 2-5/10"),
                ("content-range", b"bytes 0-1/10"),
                ("ETAG", b"\"etag\""),
                ("CONTENT-TYPE", b"text/plain; charset=utf-8"),
                ("content-type", b"application/octet-stream"),
            ],
        );

        assert_eq!(head.status, 206);
        assert_eq!(head.content_range, Some(b"bytes 2-5/10".as_slice()));
        assert_eq!(head.e_tag, Some(b"\"etag\"".as_slice()));
        assert_eq!(head.content_length, None);
        assert_eq!(
            head.content_type,
            Some(b"text/plain; charset=utf-8".as_slice())
        );
    }

    #[test]
    fn keeps_header_values_that_are_not_utf_8() {
        let head = ResponseHead::from_headers(200, [("etag", b"\"\xff\"".as_slice())]);
        assert_eq!(head.e_tag, Some(b"\"\xff\"".as_slice()));
    }
}
