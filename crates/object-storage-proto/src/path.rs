// Writing the caller's bytes into a URL.
//
// Two sets, one encoder. Both hand the writer the runs that need nothing done
// to them as they stand and one three-byte escape for every byte that does, so
// a key or a marker is written into the request buffer without a copy of it
// being made first.

// Encode bytes that are structural or ambiguous inside a URL path, including
// `%` so caller text cannot smuggle in a pre-encoded separator. Flat accounts
// may list with another delimiter, but that is ordinary blob-name text here.
// Slash remains literal because HNS paths use it between directory segments.
static OBJECT_KEY_ESCAPE: [bool; 256] = escaped(b":?#[]@!$&'()*+,;=\" <>%{}|\\^`");

// Everything but the bytes RFC 3986 calls unreserved: the letters, the digits,
// `-`, `.`, `_` and `~`. Nothing requires exactly this set. A query would
// accept `/` and `:` unescaped too, but escaping a byte that did not need it
// changes nothing, while leaving one that did lets an `&` or an `=` end the
// value early and start a parameter the caller never wrote. These values are a
// caller's prefix and the service's own marker, so this takes the set that is
// safe rather than the set that is smallest. The live paging test is the
// evidence that Azure reads it back: a real marker carries `!`, which this
// writes as `%21`.
static QUERY_VALUE_ESCAPE: [bool; 256] = unreserved_only();

// The bytes given, plus the ones that are never written as themselves: the
// control characters, which a URL may not carry, and everything outside ASCII,
// whose meaning in a URL is the percent-encoded UTF-8 it stands for.
const fn escaped(structural: &[u8]) -> [bool; 256] {
    let mut table = [false; 256];
    let mut byte = 0usize;
    while byte < 256 {
        table[byte] = byte < 0x20 || byte == 0x7F || byte >= 0x80;
        byte += 1;
    }
    let mut at = 0;
    while at < structural.len() {
        table[structural[at] as usize] = true;
        at += 1;
    }
    table
}

const fn unreserved_only() -> [bool; 256] {
    let mut table = [true; 256];
    let mut byte = 0usize;
    while byte < 256 {
        let c = byte as u8;
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'.' || c == b'_' || c == b'~' {
            table[byte] = false;
        }
        byte += 1;
    }
    table
}

// `%` and the two upper-case hexadecimal digits of every byte.
static ESCAPES: [[u8; 3]; 256] = escapes();

const fn escapes() -> [[u8; 3]; 256] {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut table = [*b"%00"; 256];
    let mut byte = 0usize;
    while byte < 256 {
        table[byte][1] = DIGITS[byte >> 4];
        table[byte][2] = DIGITS[byte & 0xF];
        byte += 1;
    }
    table
}

pub(crate) fn encode_object_key(value: &str) -> Encode<'_> {
    Encode {
        rest: value.as_bytes(),
        escape: &OBJECT_KEY_ESCAPE,
    }
}

pub(crate) fn encode_query_value(value: &[u8]) -> Encode<'_> {
    Encode {
        rest: value,
        escape: &QUERY_VALUE_ESCAPE,
    }
}

// One value as the pieces it is written in: a run of bytes that stand for
// themselves, or one escape.
pub(crate) struct Encode<'v> {
    rest: &'v [u8],
    escape: &'static [bool; 256],
}

impl<'v> Iterator for Encode<'v> {
    type Item = &'v [u8];

    fn next(&mut self) -> Option<&'v [u8]> {
        let (first, tail) = self.rest.split_first()?;
        if self.escape[*first as usize] {
            self.rest = tail;
            return Some(&ESCAPES[*first as usize]);
        }
        let end = self
            .rest
            .iter()
            .position(|byte| self.escape[*byte as usize])
            .unwrap_or(self.rest.len());
        let (run, tail) = self.rest.split_at(end);
        self.rest = tail;
        Some(run)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::String;
    use std::vec::Vec;

    use super::{encode_object_key, encode_query_value};

    fn key(value: &str) -> String {
        written(encode_object_key(value).collect())
    }

    fn query(value: &[u8]) -> String {
        written(encode_query_value(value).collect())
    }

    fn written(parts: Vec<&[u8]>) -> String {
        String::from_utf8(parts.concat()).unwrap()
    }

    #[test]
    fn preserves_path_segments_and_encodes_structure() {
        assert_eq!(
            key("directory/a key+é%?x"),
            "directory/a%20key%2B%C3%A9%25%3Fx"
        );
    }

    #[test]
    fn a_query_value_keeps_only_unreserved_bytes() {
        // Base64 and an opaque marker both carry bytes that are structural in
        // a query, so every one of them is encoded.
        assert_eq!(query(b"AAAAAAE+/="), "AAAAAAE%2B%2F%3D");
        assert_eq!(query(b"/"), "%2F");
        assert_eq!(query(b"letters-._~0123"), "letters-._~0123");
        assert_eq!(query(b"a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(query(b"\xff"), "%FF");
        assert_eq!(query(b""), "");
    }

    #[test]
    fn preserves_unreserved_bytes() {
        assert_eq!(key("letters-._~0123/path"), "letters-._~0123/path");
    }

    #[test]
    fn a_control_character_is_never_written_as_itself() {
        assert_eq!(key("a\u{1}\u{7f}b"), "a%01%7Fb");
    }
}
