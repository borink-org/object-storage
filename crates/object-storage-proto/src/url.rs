// Writes the parts of a URL that come from text, the object key and the
// query, and undoes percent-encoding in text that a service returns.
//
// The encoding and the decoding follow `percent-encoding` 2.3.2 and
// `form_urlencoded` 1.2.2 of the rust-url project, which implement the WHATWG
// URL Standard: the same sets, names and rules. They differ in two ways. They
// yield bytes rather than `&str`, because this crate has no `unsafe`. And they
// decode in place, because this crate allocates nothing.
//
// A query is `name=value` pairs joined by `&`, in the order given. An S3
// request signs its query, and the URL carries it in SigV4's canonical form.
// For that, the caller lists the parameters in the order of their names.

use crate::encoding::hex_digit;
use crate::request::{ByteSink, U64Decimal};

// A set of ASCII bytes, as `percent-encoding`'s `AsciiSet`. `percent_encode`
// escapes the bytes in the set and every byte outside ASCII.
pub(crate) struct AsciiSet {
    mask: [u32; 4],
}

impl AsciiSet {
    const fn contains(&self, byte: u8) -> bool {
        self.mask[byte as usize / 32] & (1 << (byte % 32)) != 0
    }

    fn should_percent_encode(&self, byte: u8) -> bool {
        !byte.is_ascii() || self.contains(byte)
    }

    const fn add(&self, byte: u8) -> Self {
        let mut mask = self.mask;
        mask[byte as usize / 32] |= 1 << (byte % 32);
        Self { mask }
    }

    const fn remove(&self, byte: u8) -> Self {
        let mut mask = self.mask;
        mask[byte as usize / 32] &= !(1 << (byte % 32));
        Self { mask }
    }
}

// The C0 controls, 0x00 to 0x1F, and DEL, 0x7F.
const CONTROLS: &AsciiSet = &AsciiSet {
    mask: [!0, 0, 0, 1 << (0x7F % 32)],
};

// Every ASCII byte that is not a letter or a digit.
const NON_ALPHANUMERIC: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'-')
    .add(b'.')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'_')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'~');

// The bytes that have a structural meaning in a URL path or could be read as
// one. `%` is included so caller text cannot contain a pre-encoded separator.
// A flat account may list with another delimiter, but that delimiter is
// ordinary blob-name text here. Slash is not escaped because HNS paths use it
// between directory segments.
const OBJECT_KEY_ESCAPE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

// Everything but the bytes RFC 3986 calls unreserved: the letters, the digits,
// `-`, `.`, `_` and `~`. A query would accept `/` and `:` unescaped too. But
// an `&` or an `=` left unescaped would end the value early, so this takes
// the safe set rather than the smallest one. SigV4 signs a query escaped with
// exactly this set.
const QUERY_VALUE_ESCAPE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

// `%` and the two upper-case hexadecimal digits of every byte, as
// `percent-encoding`'s `percent_encode_byte` writes them.
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

pub(crate) fn encode_object_key(value: &str) -> PercentEncode<'_> {
    percent_encode(value.as_bytes(), OBJECT_KEY_ESCAPE)
}

pub(crate) fn encode_query_value(value: &[u8]) -> PercentEncode<'_> {
    percent_encode(value, QUERY_VALUE_ESCAPE)
}

fn percent_encode<'v>(input: &'v [u8], ascii_set: &'static AsciiSet) -> PercentEncode<'v> {
    PercentEncode {
        bytes: input,
        ascii_set,
    }
}

// Yields the encoding of one value as the pieces it is written in: a run of
// bytes that need no escaping, or one escape. So text is written into the
// request buffer without a copy.
pub(crate) struct PercentEncode<'v> {
    bytes: &'v [u8],
    ascii_set: &'static AsciiSet,
}

impl<'v> Iterator for PercentEncode<'v> {
    type Item = &'v [u8];

    fn next(&mut self) -> Option<&'v [u8]> {
        let (&first, remaining) = self.bytes.split_first()?;
        if self.ascii_set.should_percent_encode(first) {
            self.bytes = remaining;
            return Some(&ESCAPES[first as usize]);
        }
        let end = remaining
            .iter()
            .position(|&byte| self.ascii_set.should_percent_encode(byte))
            .map_or(self.bytes.len(), |at| at + 1);
        let (unchanged, remaining) = self.bytes.split_at(end);
        self.bytes = remaining;
        Some(unchanged)
    }
}

// Percent-decodes `bytes` in place, as `percent-encoding`'s `percent_decode`
// does: `%` and two hexadecimal digits of either case become the byte they
// name, and any other `%` stays as it is. Returns the decoded length.
pub(crate) fn percent_decode_in_place(bytes: &mut [u8]) -> usize {
    decode_in_place(bytes, false).0
}

// Decodes `bytes` in place as `application/x-www-form-urlencoded`, as
// `form_urlencoded`'s `parse` decodes a value: a `+` becomes a space, and the
// rest is percent-decoded. Returns the decoded length, and whether decoding
// changed the text.
pub(crate) fn form_decode_in_place(bytes: &mut [u8]) -> (usize, bool) {
    decode_in_place(bytes, true)
}

// `form_urlencoded` replaces every `+` before it percent-decodes. One pass
// does the same, because a `+` is never a hexadecimal digit of an escape.
fn decode_in_place(b: &mut [u8], plus_is_space: bool) -> (usize, bool) {
    // `r` reads and `w` writes. The two are equal up to the first change, so
    // nothing moves until something has shrunk. `w` never passes `r`.
    let (mut r, mut w) = (0, 0);
    let mut changed = false;
    while r < b.len() {
        let run = b[r..]
            .iter()
            .position(|&byte| byte == b'%' || (plus_is_space && byte == b'+'))
            .unwrap_or(b.len() - r);
        if w != r {
            b.copy_within(r..r + run, w);
        }
        r += run;
        w += run;
        if r == b.len() {
            break;
        }
        let (decoded, read) = match b[r] {
            b'+' => (b' ', 1),
            _ => match after_percent_sign(&b[r + 1..]) {
                Some(byte) => (byte, 3),
                None => (b'%', 1),
            },
        };
        changed |= read != 1 || b[r] == b'+';
        b[w] = decoded;
        r += read;
        w += 1;
    }
    (w, changed)
}

// The byte that the two hexadecimal digits at the start of `rest` name, as
// `percent-encoding`'s `after_percent_sign` reads them.
fn after_percent_sign(rest: &[u8]) -> Option<u8> {
    let high = hex_digit(*rest.first()?)?;
    let low = hex_digit(*rest.get(1)?)?;
    Some(high << 4 | low)
}

// One query value, in the form that the writer needs it.
#[derive(Clone, Copy)]
pub(crate) enum QueryValue<'q> {
    // A constant of this crate, such as `url` in `encoding-type=url`,
    // written unencoded. It holds only unreserved bytes.
    Literal(&'q str),
    // Text from the caller or the service, such as a prefix, written
    // percent-encoded.
    Encoded(&'q [u8]),
    Number(u32),
    // Constants of this crate joined by commas, such as Azure's
    // `include=metadata`. SigV4 would encode the commas, so a signed query
    // does not use this form.
    Words(&'q [&'q str]),
}

// One parameter, or `None` for one that the request leaves out.
pub(crate) type Parameter<'q> = Option<(&'q str, QueryValue<'q>)>;

// Text from the caller or the service, percent-encoded, or no parameter
// without a value.
pub(crate) fn encoded<'q>(name: &'q str, value: impl Into<Option<&'q str>>) -> Parameter<'q> {
    value
        .into()
        .map(|value| (name, QueryValue::Encoded(value.as_bytes())))
}

// A constant of this crate, written as it is.
pub(crate) fn literal<'q>(name: &'q str, value: &'q str) -> Parameter<'q> {
    Some((name, QueryValue::Literal(value)))
}

// A number, or no parameter without one.
pub(crate) fn number<'q>(name: &'q str, value: impl Into<Option<u32>>) -> Parameter<'q> {
    value.into().map(|value| (name, QueryValue::Number(value)))
}

// Constants of this crate joined by commas, or no parameter without any.
pub(crate) fn words<'q>(name: &'q str, words: &'q [&'q str]) -> Parameter<'q> {
    (!words.is_empty()).then_some((name, QueryValue::Words(words)))
}

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

    use super::{
        OBJECT_KEY_ESCAPE, QUERY_VALUE_ESCAPE, encode_object_key, encode_query_value,
        form_decode_in_place, percent_decode_in_place,
    };

    fn key(value: &str) -> String {
        written(encode_object_key(value).collect())
    }

    fn query(value: &[u8]) -> String {
        written(encode_query_value(value).collect())
    }

    fn written(parts: Vec<&[u8]>) -> String {
        String::from_utf8(parts.concat()).unwrap()
    }

    fn percent(text: &str) -> String {
        let mut bytes = Vec::from(text.as_bytes());
        let len = percent_decode_in_place(&mut bytes);
        String::from_utf8(Vec::from(&bytes[..len])).unwrap()
    }

    fn form(text: &str) -> (String, bool) {
        let mut bytes = Vec::from(text.as_bytes());
        let (len, changed) = form_decode_in_place(&mut bytes);
        (
            String::from_utf8(Vec::from(&bytes[..len])).unwrap(),
            changed,
        )
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

    // The sets were tables of 256 entries before they followed
    // `percent-encoding`. This checks every byte against those tables.
    #[test]
    fn the_sets_escape_what_the_former_tables_escaped() {
        for byte in 0..=255u8 {
            let always = byte < 0x20 || byte == 0x7F || byte >= 0x80;
            let key = always || b":?#[]@!$&'()*+,;=\" <>%{}|\\^`".contains(&byte);
            let unreserved = byte.is_ascii_alphanumeric() || b"-._~".contains(&byte);
            assert_eq!(OBJECT_KEY_ESCAPE.should_percent_encode(byte), key, "{byte}");
            assert_eq!(
                QUERY_VALUE_ESCAPE.should_percent_encode(byte),
                !unreserved,
                "{byte}"
            );
        }
    }

    // The cases of `percent-encoding`'s own tests, and the WHATWG rule that a
    // `%` that begins no escape stays as it is.
    #[test]
    fn percent_decodes_as_the_url_standard_does() {
        assert_eq!(percent("foo%20bar%3f"), "foo bar?");
        assert_eq!(percent("%F0%9F%92%96"), "\u{1F496}");
        assert_eq!(percent("a+b"), "a+b");
        assert_eq!(percent("100%"), "100%");
        assert_eq!(percent("%2"), "%2");
        assert_eq!(percent("%zz%41"), "%zzA");
        assert_eq!(percent("%%41"), "%A");
    }

    #[test]
    fn form_decodes_a_plus_as_a_space_and_reports_a_change() {
        assert_eq!(form("a+b%2Bc"), ("a b+c".into(), true));
        assert_eq!(form("caf%C3%A9%2F"), ("caf\u{e9}/".into(), true));
        assert_eq!(form("plain/key"), ("plain/key".into(), false));
        assert_eq!(form("100%-name"), ("100%-name".into(), false));
        assert_eq!(form("%+2"), ("% 2".into(), true));
    }
}
