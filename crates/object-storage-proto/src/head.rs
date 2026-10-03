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
        let slot = if name.eq_ignore_ascii_case("content-length") {
            &mut self.content_length
        } else if name.eq_ignore_ascii_case("content-range") {
            &mut self.content_range
        } else if name.eq_ignore_ascii_case("content-encoding") {
            &mut self.content_encoding
        } else if name.eq_ignore_ascii_case("content-type") {
            &mut self.content_type
        } else if name.eq_ignore_ascii_case("content-md5") {
            &mut self.content_md5
        } else if name.eq_ignore_ascii_case("content-language") {
            &mut self.content_language
        } else if name.eq_ignore_ascii_case("content-disposition") {
            &mut self.content_disposition
        } else if name.eq_ignore_ascii_case("cache-control") {
            &mut self.cache_control
        } else if name.eq_ignore_ascii_case("x-amz-storage-class")
            || name.eq_ignore_ascii_case("x-ms-access-tier")
        {
            &mut self.storage_class
        } else if name.eq_ignore_ascii_case("etag") {
            &mut self.e_tag
        } else if name.eq_ignore_ascii_case("last-modified") {
            &mut self.last_modified
        } else if name.eq_ignore_ascii_case("x-ms-version-id")
            || name.eq_ignore_ascii_case("x-amz-version-id")
        {
            &mut self.version
        } else if name.eq_ignore_ascii_case("x-ms-snapshot") {
            &mut self.snapshot
        } else if name.eq_ignore_ascii_case("x-ms-copy-id") {
            &mut self.copy_id
        } else if name.eq_ignore_ascii_case("x-ms-copy-status") {
            &mut self.copy_status
        } else if name.eq_ignore_ascii_case("x-amz-restore")
            || name.eq_ignore_ascii_case("x-ms-archive-status")
        {
            &mut self.restore_status
        } else if name.eq_ignore_ascii_case("x-ms-error-code") {
            &mut self.error_code
        } else if name.eq_ignore_ascii_case("x-ms-request-id")
            || name.eq_ignore_ascii_case("x-amz-request-id")
        {
            &mut self.request_id
        } else if name.eq_ignore_ascii_case("x-amz-id-2") {
            &mut self.extended_request_id
        } else {
            return;
        };
        if slot.is_none() {
            *slot = Some(value);
        }
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
