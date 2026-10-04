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
/// [`Blobs::accept_get_head`](crate::Blobs::accept_get_head) returns from
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
        let is = |known: &Known| known.is(name);
        Some(match name.len() {
            4 if is(const { &Known::new("etag") }) => &mut self.e_tag,
            10 if is(const { &Known::new("x-amz-id-2") }) => &mut self.extended_request_id,
            11 if is(const { &Known::new("content-md5") }) => &mut self.content_md5,
            12 if is(const { &Known::new("content-type") }) => &mut self.content_type,
            12 if is(const { &Known::new("x-ms-copy-id") }) => &mut self.copy_id,
            13 if is(const { &Known::new("content-range") }) => &mut self.content_range,
            13 if is(const { &Known::new("cache-control") }) => &mut self.cache_control,
            13 if is(const { &Known::new("last-modified") }) => &mut self.last_modified,
            13 if is(const { &Known::new("x-ms-snapshot") }) => &mut self.snapshot,
            13 if is(const { &Known::new("x-amz-restore") }) => &mut self.restore_status,
            14 if is(const { &Known::new("content-length") }) => &mut self.content_length,
            15 if is(const { &Known::new("x-ms-version-id") }) => &mut self.version,
            15 if is(const { &Known::new("x-ms-error-code") }) => &mut self.error_code,
            15 if is(const { &Known::new("x-ms-request-id") }) => &mut self.request_id,
            16 if is(const { &Known::new("content-encoding") }) => &mut self.content_encoding,
            16 if is(const { &Known::new("content-language") }) => &mut self.content_language,
            16 if is(const { &Known::new("x-ms-access-tier") }) => &mut self.storage_class,
            16 if is(const { &Known::new("x-amz-version-id") }) => &mut self.version,
            16 if is(const { &Known::new("x-ms-copy-status") }) => &mut self.copy_status,
            16 if is(const { &Known::new("x-amz-request-id") }) => &mut self.request_id,
            19 if is(const { &Known::new("content-disposition") }) => &mut self.content_disposition,
            19 if is(const { &Known::new("x-amz-storage-class") }) => &mut self.storage_class,
            19 if is(const { &Known::new("x-ms-archive-status") }) => &mut self.restore_status,
            _ => return None,
        })
    }
}

// A header name that a head keeps, ready to compare: its bytes as words, and
// the case bit `0x20` of each byte that is a letter. A byte matches if it
// equals the known byte once that bit is set, which is exact for each byte,
// and a name of 4 to 24 bytes is two or three overlapping words, so a
// comparison is a few loads whatever the length.
struct Known {
    len: usize,
    // The words at the start, in the middle (only past 16 bytes) and at the
    // end, each as `(bytes, case bits)`. Below 8 bytes they are 4 wide.
    words: [(u64, u64); 3],
}

impl Known {
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
