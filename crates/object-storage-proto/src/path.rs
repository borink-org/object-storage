use percent_encoding::{
    AsciiSet, CONTROLS, NON_ALPHANUMERIC, PercentEncode, percent_encode, utf8_percent_encode,
};

// Encode bytes that are structural or ambiguous inside a URL path, including
// `%` so caller text cannot smuggle in a pre-encoded separator. Flat accounts
// may list with another delimiter, but that is ordinary blob-name text here.
// Slash remains literal because HNS paths use it between directory segments.
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

// review: where is this from; as in what requires us to define precisely this? i find the explanation very vague
// "so no byte of them may be structural"; use plainer language please
// Everything but the unreserved bytes of RFC 3986. That is stricter than a
// query needs, and it is what an opaque marker or a delimiter requires: those
// values are the service's own bytes, so no byte of them may be structural.
const QUERY_VALUE_ESCAPE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub(crate) fn encode_query_value(value: &[u8]) -> PercentEncode<'_> {
    percent_encode(value, QUERY_VALUE_ESCAPE)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{encode_object_key, encode_query_value};
    use std::string::ToString;

    #[test]
    fn preserves_path_segments_and_encodes_structure() {
        assert_eq!(
            encode_object_key("directory/a key+é%?x").to_string(),
            "directory/a%20key%2B%C3%A9%25%3Fx"
        );
    }

    #[test]
    fn a_query_value_keeps_only_unreserved_bytes() {
        // Base64 and an opaque marker both carry bytes that are structural in
        // a query, so every one of them is encoded.
        assert_eq!(
            encode_query_value(b"AAAAAAE+/=").to_string(),
            "AAAAAAE%2B%2F%3D"
        );
        assert_eq!(encode_query_value(b"/").to_string(), "%2F");
        assert_eq!(
            encode_query_value(b"letters-._~0123").to_string(),
            "letters-._~0123"
        );
        assert_eq!(encode_query_value(b"a b&c=d").to_string(), "a%20b%26c%3Dd");
        assert_eq!(encode_query_value(b"\xff").to_string(), "%FF");
    }

    #[test]
    fn preserves_unreserved_bytes() {
        assert_eq!(
            encode_object_key("letters-._~0123/path").to_string(),
            "letters-._~0123/path"
        );
    }
}
