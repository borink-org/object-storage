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
