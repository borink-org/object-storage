// Writes the parts of a URL that come from text: the object key in the path,
// and the query.
//
// The caller's bytes are percent-encoded as they are written. There are two
// escape sets and one encoder. The encoder yields the runs of bytes that need
// no escaping as they are, and a three-byte escape for every byte that does.
// So a key or a marker is written into the request buffer without being
// copied first.
//
// A query is optional parameters, each a name and a value, written as
// `name=value` pairs joined by `&`, in the order given. A name is a constant
// of this crate, such as `prefix`, and needs no percent-encoding. A value is
// one of the forms of `QueryValue`. An S3 request signs its query, and the
// URL carries it in the canonical form that SigV4 signs. That holds only when
// the caller lists the parameters in the order of their names and every value
// reads the same whether or not it is percent-encoded again.

use crate::request::{ByteSink, U64Decimal};

// The bytes that have a structural meaning in a URL path or could be read as
// one. `%` is included so caller text cannot contain a pre-encoded separator.
// A flat account may list with another delimiter, but that delimiter is
// ordinary blob-name text here. Slash is not escaped because HNS paths use it
// between directory segments.
static OBJECT_KEY_ESCAPE: [bool; 256] = escaped(b":?#[]@!$&'()*+,;=\" <>%{}|\\^`");

// Everything but the bytes RFC 3986 calls unreserved: the letters, the digits,
// `-`, `.`, `_` and `~`. Nothing requires exactly this set. A query would
// accept `/` and `:` unescaped too. But escaping a byte that did not need it
// changes nothing. Leaving one unescaped that did lets an `&` or an `=` end
// the value early, and start a parameter the caller never wrote. These values
// are a caller's prefix and the service's own marker, so this takes the safe
// set rather than the smallest one. The live paging test shows that Azure
// reads it back: a real marker holds `!`, which this writes as `%21`.
static QUERY_VALUE_ESCAPE: [bool; 256] = unreserved_only();

// Builds an escape table from the bytes given, plus the bytes that are never
// written as themselves. Those are the control characters, which a URL may
// not hold, and all non-ASCII bytes, which a URL holds percent-encoded.
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

// Yields one value as the pieces it is written in: a run of bytes that need no
// escaping, or one escape.
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

// One query value, in the form that the writer needs it.
#[derive(Clone, Copy)]
pub(crate) enum QueryValue<'q> {
    // A constant of this crate, such as `url` in `encoding-type=url`. It
    // holds only bytes that a URL carries as they are, so it is written
    // unencoded.
    Literal(&'q str),
    // Text from the caller or the service, such as a prefix or a marker,
    // which is percent-encoded as it is written.
    Encoded(&'q [u8]),
    Number(u32),
    // Constants of this crate joined by commas, such as `metadata` in
    // Azure's `include=metadata`. The commas are written unencoded, and SigV4
    // would encode them, so a signed query does not use this form.
    Words(&'q [&'q str]),
}

// One parameter, or `None` for one that the request leaves out.
pub(crate) type Parameter<'q> = Option<(&'q str, QueryValue<'q>)>;

// Writes the parameters that `query` holds, without the `?` that begins a
// query in a URL.
pub(crate) fn write_query(out: &mut dyn ByteSink, query: &[Parameter<'_>]) {
    for (index, (name, value)) in query.iter().flatten().enumerate() {
        if index != 0 {
            out.push(b"&");
        }
        out.push(name.as_bytes());
        out.push(b"=");
        match *value {
            QueryValue::Literal(value) => out.push(value.as_bytes()),
            QueryValue::Encoded(value) => {
                for part in encode_query_value(value) {
                    out.push(part);
                }
            }
            QueryValue::Number(value) => out.push(U64Decimal::new(value.into()).as_bytes()),
            QueryValue::Words(words) => {
                for (index, word) in words.iter().enumerate() {
                    if index != 0 {
                        out.push(b",");
                    }
                    out.push(word.as_bytes());
                }
            }
        }
    }
}

// Writes the query as a URL ends with it: nothing if `query` holds no
// parameter, and otherwise a `?` and the parameters.
pub(crate) fn write_query_in_url(out: &mut dyn ByteSink, query: &[Parameter<'_>]) {
    if query.iter().any(Option::is_some) {
        out.push(b"?");
        write_query(out, query);
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
