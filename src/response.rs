/// HTTP response metadata supplied by the host before it reads the body.
#[derive(Debug, Clone, Copy)]
pub struct Response<'a> {
    status: u16,
    content_length: Option<&'a str>,
    content_range: Option<&'a str>,
    e_tag: Option<&'a str>,
    version: Option<&'a str>,
}

impl<'a> Response<'a> {
    /// Reads the relevant response headers from borrowed name-value pairs.
    ///
    /// The iterator is consumed immediately. Relevant header values are
    /// borrowed without copying, so a host can map its native iterator directly.
    pub fn new(status: u16, headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut response = Self {
            status,
            content_length: None,
            content_range: None,
            e_tag: None,
            version: None,
        };
        for (name, value) in headers {
            let slot = if name.eq_ignore_ascii_case("content-length") {
                &mut response.content_length
            } else if name.eq_ignore_ascii_case("content-range") {
                &mut response.content_range
            } else if name.eq_ignore_ascii_case("etag") {
                &mut response.e_tag
            } else if name.eq_ignore_ascii_case("x-ms-version-id") {
                &mut response.version
            } else {
                continue;
            };
            if slot.is_none() {
                *slot = Some(value);
            }
        }
        response
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    pub(crate) fn header(&self, name: &str) -> Option<&'a str> {
        if name.eq_ignore_ascii_case("content-length") {
            self.content_length
        } else if name.eq_ignore_ascii_case("content-range") {
            self.content_range
        } else if name.eq_ignore_ascii_case("etag") {
            self.e_tag
        } else if name.eq_ignore_ascii_case("x-ms-version-id") {
            self.version
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Response;

    #[test]
    fn retains_the_first_relevant_header_case_insensitively() {
        let response = Response::new(
            206,
            [
                ("ignored", "value"),
                ("Content-Range", "bytes 2-5/10"),
                ("content-range", "bytes 0-1/10"),
                ("ETAG", "\"etag\""),
            ],
        );

        assert_eq!(response.status(), 206);
        assert_eq!(response.header("content-range"), Some("bytes 2-5/10"));
        assert_eq!(response.header("etag"), Some("\"etag\""));
        assert_eq!(response.header("ignored"), None);
    }
}
