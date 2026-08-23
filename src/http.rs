// Endpoints are copied directly into the request target, and this crate has no
// URL or IDNA dependency to normalize them. This check rejects bytes that could
// change request structure; the host's HTTP client still validates the address.
pub(crate) fn valid_http_origin(value: &str) -> bool {
    let Some((scheme, authority)) = value.split_once("://") else {
        return false;
    };
    // `/`, `?`, and `#` end the authority; `@` introduces userinfo. Space and
    // controls delimit HTTP syntax, while clients disagree on normalizing `\\`.
    // Reject them before appending the container and key to this origin.
    matches!(scheme, "http" | "https")
        && !authority.is_empty()
        && value.is_ascii()
        && !authority.bytes().any(|byte| {
            byte.is_ascii_control() || matches!(byte, b' ' | b'/' | b'?' | b'#' | b'@' | b'\\')
        })
}

#[cfg(test)]
mod tests {
    use super::valid_http_origin;

    #[test]
    fn accepts_ascii_http_origins() {
        for value in [
            "https://account.blob.core.windows.net",
            "http://127.0.0.1:10000",
            "http://[::1]:10000",
        ] {
            assert!(valid_http_origin(value), "{value}");
        }
    }

    #[test]
    fn rejects_values_that_are_not_ascii_origins() {
        for value in [
            "account.example",
            "ftp://account.example",
            "https://",
            "https://user@account.example",
            "https://account.example/path",
            "https://account.example?query",
            "https://account.example#fragment",
            "https://tést.example",
            "https://account.example\\path",
            "https://account.example\r\nheader",
        ] {
            assert!(!valid_http_origin(value), "{value:?}");
        }
    }
}
