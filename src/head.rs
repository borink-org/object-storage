/// The response header values this library needs, filled by name.
///
/// Values are bytes: a header a provider sends is not guaranteed to be UTF-8,
/// and dropping such a header silently would lose exactly the metadata a
/// scheduler needs. `Copy` is normative — classification may want the head
/// again after the body has been read.
///
/// Header names and values must remain valid for as long as the host uses the
/// [`GetHeadOutcome`](crate::GetHeadOutcome) they produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct GetHead<'h> {
    /// The HTTP status code.
    pub status: u16,
    /// `Content-Length`.
    pub content_length: Option<&'h [u8]>,
    /// `Content-Range`.
    pub content_range: Option<&'h [u8]>,
    /// `Content-Encoding`, surfaced so the host knows what the bytes are.
    pub content_encoding: Option<&'h [u8]>,
    /// `ETag`.
    pub etag: Option<&'h [u8]>,
    /// `Last-Modified`.
    pub last_modified: Option<&'h [u8]>,
    /// `x-ms-version-id`.
    pub version: Option<&'h [u8]>,
    /// `x-ms-request-id`, the identifier Azure support asks for.
    pub request_id: Option<&'h [u8]>,
}

impl<'h> GetHead<'h> {
    /// An otherwise empty head with this status.
    pub fn new(status: u16) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }

    /// Fills the slots from borrowed name-value pairs.
    ///
    /// The iterator is consumed immediately and only the relevant values are
    /// retained, so a host that keeps all of its headers can map its native
    /// iterator directly. A host with a streaming parser fills the fields
    /// itself and drops everything else.
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
                &mut head.etag
            } else if name.eq_ignore_ascii_case("last-modified") {
                &mut head.last_modified
            } else if name.eq_ignore_ascii_case("x-ms-version-id") {
                &mut head.version
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
        assert_eq!(head.etag, Some(b"\"etag\"".as_slice()));
        assert_eq!(head.content_length, None);
    }

    #[test]
    fn keeps_header_values_that_are_not_utf_8() {
        let head = GetHead::from_headers(200, [("etag", b"\"\xff\"".as_slice())]);
        assert_eq!(head.etag, Some(b"\"\xff\"".as_slice()));
    }
}
