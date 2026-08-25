/// The response header values that this crate reads.
///
/// Each field holds the value of one header. Fill the fields with
/// [`GetHead::from_headers`], or set them directly from a streaming parser.
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
pub struct GetHead<'h> {
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
    /// The value of the `ETag` header.
    pub e_tag: Option<&'h [u8]>,
    /// The value of the `Last-Modified` header.
    pub last_modified: Option<&'h [u8]>,
    /// The value of the `x-ms-version-id` header.
    pub version: Option<&'h [u8]>,
    /// The value of the `x-ms-error-code` header.
    ///
    /// Azure names the error here. See
    /// [`classify_error`](crate::classify_error).
    pub error_code: Option<&'h [u8]>,
    /// The value of the `x-ms-request-id` header.
    ///
    /// Azure assigns one identifier to each request. Record it: Azure support
    /// uses it to find the request in the service logs.
    pub request_id: Option<&'h [u8]>,
}

impl<'h> GetHead<'h> {
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
            let slot = if name.eq_ignore_ascii_case("content-length") {
                &mut head.content_length
            } else if name.eq_ignore_ascii_case("content-range") {
                &mut head.content_range
            } else if name.eq_ignore_ascii_case("content-encoding") {
                &mut head.content_encoding
            } else if name.eq_ignore_ascii_case("etag") {
                &mut head.e_tag
            } else if name.eq_ignore_ascii_case("last-modified") {
                &mut head.last_modified
            } else if name.eq_ignore_ascii_case("x-ms-version-id") {
                &mut head.version
            } else if name.eq_ignore_ascii_case("x-ms-error-code") {
                &mut head.error_code
            } else if name.eq_ignore_ascii_case("x-ms-request-id") {
                &mut head.request_id
            } else {
                continue;
            };
            if slot.is_none() {
                *slot = Some(value);
            }
        }
        head
    }
}

#[cfg(test)]
mod tests {
    use super::GetHead;

    #[test]
    fn retains_the_first_relevant_header_case_insensitively() {
        let head = GetHead::from_headers(
            206,
            [
                ("ignored", b"value".as_slice()),
                ("Content-Range", b"bytes 2-5/10"),
                ("content-range", b"bytes 0-1/10"),
                ("ETAG", b"\"etag\""),
            ],
        );

        assert_eq!(head.status, 206);
        assert_eq!(head.content_range, Some(b"bytes 2-5/10".as_slice()));
        assert_eq!(head.e_tag, Some(b"\"etag\"".as_slice()));
        assert_eq!(head.content_length, None);
    }

    #[test]
    fn keeps_header_values_that_are_not_utf_8() {
        let head = GetHead::from_headers(200, [("etag", b"\"\xff\"".as_slice())]);
        assert_eq!(head.e_tag, Some(b"\"\xff\"".as_slice()));
    }
}
