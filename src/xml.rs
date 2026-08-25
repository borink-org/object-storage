use xmlparser::{ElementEnd, Token, Tokenizer};

pub(crate) fn error_code(body: &[u8]) -> Option<&str> {
    let body = core::str::from_utf8(body).ok()?;
    let mut depth = 0usize;
    let mut code_depth = None;
    for token in Tokenizer::from(body) {
        match token.ok()? {
            Token::ElementStart { local, .. } => {
                depth += 1;
                if local.as_str() == "Code" {
                    code_depth = Some(depth);
                }
            }
            Token::Text { text } if code_depth == Some(depth) => {
                return Some(text.as_str().trim());
            }
            Token::ElementEnd { end, .. } => match end {
                ElementEnd::Open => {}
                ElementEnd::Empty | ElementEnd::Close(..) => {
                    if code_depth == Some(depth) {
                        code_depth = None;
                    }
                    depth = depth.checked_sub(1)?;
                }
            },
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::error_code;

    #[test]
    fn reads_the_code_from_an_azure_error_body() {
        assert_eq!(
            error_code(b"<?xml version=\"1.0\"?><Error><Code>BlobNotFound</Code><Message>The specified blob does not exist.</Message></Error>"),
            Some("BlobNotFound")
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            error_code(b"<Error>\n  <Code>\n    ServerBusy\n  </Code>\n</Error>"),
            Some("ServerBusy")
        );
    }

    #[test]
    fn finds_the_code_at_any_depth() {
        assert_eq!(
            error_code(b"<Error><Detail><Code>InternalError</Code></Detail></Error>"),
            Some("InternalError")
        );
    }

    #[test]
    fn returns_nothing_without_a_code_element() {
        assert_eq!(
            error_code(b"<Error><Message>no code here</Message></Error>"),
            None
        );
        assert_eq!(error_code(b""), None);
    }

    #[test]
    fn returns_nothing_for_a_code_element_with_no_text() {
        assert_eq!(error_code(b"<Error><Code /></Error>"), None);
        assert_eq!(
            error_code(b"<Error><Code></Code><Code>Late</Code></Error>"),
            Some("Late")
        );
    }

    #[test]
    fn ignores_text_outside_the_code_element() {
        assert_eq!(
            error_code(b"<Error><Message>BlobNotFound</Message><Code>ServerBusy</Code></Error>"),
            Some("ServerBusy")
        );
    }

    #[test]
    fn returns_nothing_for_a_body_that_is_not_utf_8() {
        assert_eq!(error_code(b"<Error><Code>\xff</Code></Error>"), None);
    }

    #[test]
    fn a_body_cut_short_yields_at_most_a_partial_code() {
        // `classify_error` is told separately that the body was truncated, so a
        // partial code that matches nothing becomes `Incomplete`, not `Unknown`.
        assert_eq!(error_code(b"<Error><Code>BlobNot"), Some("BlobNot"));
        assert_eq!(error_code(b"<Error><Cod"), None);
    }
}
