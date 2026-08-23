use percent_encoding::{AsciiSet, CONTROLS, PercentEncode, utf8_percent_encode};

// Encode bytes that are structural or ambiguous inside a URL path, including
// `%` so caller text cannot smuggle in a pre-encoded separator. Slash remains
// literal because Azure blob names use it for virtual directory segments.
const OBJECT_KEY_ESCAPE: &AsciiSet = &CONTROLS
    .add(b':')
    .add(b'?')
    .add(b'#')
    .add(b'[')
    .add(b']')
    .add(b'@')
    .add(b'!')
    .add(b'$')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b';')
    .add(b'=')
    .add(b'"')
    .add(b' ')
    .add(b'<')
    .add(b'>')
    .add(b'%')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\')
    .add(b'^')
    .add(b'`');

pub(crate) fn encode_object_key(value: &str) -> PercentEncode<'_> {
    utf8_percent_encode(value, OBJECT_KEY_ESCAPE)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::encode_object_key;
    use std::string::ToString;

    #[test]
    fn preserves_path_segments_and_encodes_structure() {
        assert_eq!(
            encode_object_key("directory/a key+é%?x").to_string(),
            "directory/a%20key%2B%C3%A9%25%3Fx"
        );
    }

    #[test]
    fn preserves_unreserved_bytes() {
        assert_eq!(
            encode_object_key("letters-._~0123/path").to_string(),
            "letters-._~0123/path"
        );
    }
}
