// Undoes the escaping that XML applies to text, and the percent-encoding that
// the service applies to a name that XML cannot hold.
//
// Both only ever shorten the text, so both decode in place from left to
// right. The bytes freed at the end keep whatever they held. Each function
// returns the length of the decoded text. The page reader then sets the
// leftover bytes to zero, so that the walk over an entry can find where the
// decoded text ends.

use super::scan::{AMP, PCT, fault, find_byte};
use crate::Result;

/// Undoes both encodings, in the order a document applies them.
pub(crate) fn decode_text(bytes: &mut [u8], percent: bool) -> Result<usize> {
    let len = decode_references(bytes)?;
    if percent {
        decode_percent(&mut bytes[..len])
    } else {
        Ok(len)
    }
}

// The same, for a value the scanner has already seen. The scanner's flags say
// whether there is anything to undo. In an ordinary listing no text holds an
// `&` or a `%`, so nothing is undone.
pub(crate) fn decode(bytes: &mut [u8], flags: u8, percent: bool) -> Result<usize> {
    let mut len = bytes.len();
    if flags & AMP != 0 {
        len = decode_references(bytes)?;
    }
    // A reference can encode a `%`, so after the references are undone the
    // flag no longer says whether there is one.
    if percent && flags & (PCT | AMP) != 0 {
        len = decode_percent(&mut bytes[..len])?;
    }
    Ok(len)
}

fn decode_references(b: &mut [u8]) -> Result<usize> {
    // `r` reads and `w` writes. The two are equal up to the first escape, so
    // nothing moves until something has shrunk.
    let (mut r, mut w) = (0, 0);
    while r < b.len() {
        let run = find_byte(b, r, b'&') - r;
        if w != r {
            b.copy_within(r..r + run, w);
        }
        r += run;
        w += run;
        if r == b.len() {
            break;
        }
        // In XML an `&` always begins a reference. This document declares no
        // entities and may not declare any, so the references below are the
        // only ones it can hold. Anything else is a fault.
        //
        // The five named references each decode to one ASCII byte. Nearly
        // every reference in a listing is one of these.
        let rest = &b[r + 1..];
        let named = if rest.starts_with(b"quot;") {
            Some((b'"', 6))
        } else if rest.starts_with(b"amp;") {
            Some((b'&', 5))
        } else if rest.starts_with(b"lt;") {
            Some((b'<', 4))
        } else if rest.starts_with(b"gt;") {
            Some((b'>', 4))
        } else if rest.starts_with(b"apos;") {
            Some((b'\'', 6))
        } else {
            None
        };
        if let Some((c, len)) = named {
            b[w] = c;
            w += 1;
            r += len;
            continue;
        }
        // The numeric form. A longer reference cannot name a character.
        let Some(len) = rest.iter().take(10).position(|c| *c == b';') else {
            return fault();
        };
        let name = &b[r + 1..r + 1 + len];
        let code = match name {
            [b'#', b'x', hex @ ..] if !hex.is_empty() => digits(hex, 16)?,
            [b'#', decimal @ ..] if !decimal.is_empty() => digits(decimal, 10)?,
            _ => return fault(),
        };
        // A reference to a character that XML forbids in a document is a
        // fault, whatever it would decode to. Accepting it would put a byte
        // in a key that no key may hold.
        //
        // This is a rule about the document, not about UTF-8: a character XML
        // forbids is still valid UTF-8. Azure never writes such a reference.
        // It refuses a control character in a key when the key is written,
        // and percent-encodes the non-characters it does store, marked with
        // `<Name Encoded="true">`. S3 can write one: it stores `U+0001` and,
        // unless the listing asks for `encoding-type=url`, writes it as
        // `&#x1;`. This crate always asks, so the reference never arrives. If
        // that changes, this rule refuses a key that AWS stores, and the
        // decision then belongs with S3 LIST rather than here.
        let Some(ch) = char::from_u32(code).filter(|c| xml_char(*c as u32)) else {
            return fault();
        };
        let mut buffer = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buffer).as_bytes();
        // The reference is at least four bytes and its character at most
        // four, so the write position never passes the read position.
        b[w..w + encoded.len()].copy_from_slice(encoded);
        w += encoded.len();
        r += len + 2;
    }
    Ok(w)
}

fn digits(bytes: &[u8], radix: u32) -> Result<u32> {
    bytes.iter().try_fold(0u32, |value, byte| {
        let Some(digit) = (*byte as char).to_digit(radix) else {
            return fault();
        };
        match value.checked_mul(radix).and_then(|v| v.checked_add(digit)) {
            Some(value) => Ok(value),
            None => fault(),
        }
    })
}

// Returns whether XML 1.0 allows this character in a document.
fn xml_char(code: u32) -> bool {
    matches!(
        code,
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF
    )
}

fn decode_percent(b: &mut [u8]) -> Result<usize> {
    let (mut r, mut w) = (0, 0);
    while r < b.len() {
        let run = find_byte(b, r, b'%') - r;
        if w != r {
            b.copy_within(r..r + run, w);
        }
        r += run;
        w += run;
        if r == b.len() {
            break;
        }
        // A name marked as encoded is encoded whole, including the separators
        // between its segments. So every `%` in it begins an escape, and a
        // `%` that does not is a fault. Measured: a listed name reads
        // `...azure-list-scratch%2F100%25-%EF%BF%BE...`.
        let (Some(high), Some(low)) = (
            b.get(r + 1).copied().and_then(hex),
            b.get(r + 2).copied().and_then(hex),
        ) else {
            return fault();
        };
        b[w] = high << 4 | low;
        r += 3;
        w += 1;
    }
    Ok(w)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::String;
    use std::vec::Vec;

    use super::decode_text;

    fn decoded(text: &str, percent: bool) -> String {
        let mut bytes = Vec::from(text.as_bytes());
        let len = decode_text(&mut bytes, percent).expect(text);
        String::from_utf8(Vec::from(&bytes[..len])).unwrap()
    }

    fn refused(text: &str, percent: bool) -> bool {
        let mut bytes = Vec::from(text.as_bytes());
        decode_text(&mut bytes, percent).is_err()
    }

    #[test]
    fn undoes_the_five_references_that_xml_defines() {
        assert_eq!(decoded("a&amp;b", false), "a&b");
        assert_eq!(decoded("&lt;tag&gt;", false), "<tag>");
        assert_eq!(decoded("&quot;q&apos;", false), "\"q'");
        assert_eq!(decoded("nothing to undo", false), "nothing to undo");
    }

    #[test]
    fn undoes_a_reference_written_as_a_number() {
        assert_eq!(decoded("caf&#233;", false), "caf\u{e9}");
        assert_eq!(decoded("caf&#xe9;", false), "caf\u{e9}");
        // Four bytes out of nine is still shorter, so even this reference
        // can be decoded in place.
        assert_eq!(decoded("&#x1F600;", false), "\u{1f600}");
    }

    #[test]
    fn refuses_what_is_not_a_reference() {
        // A listing declares no entities and may not declare any, so `&` can
        // only begin one of the references above. Passing anything else
        // through would return a key that is not the object's.
        for text in ["a&nbsp;b", "a&#;b", "100% &", "a&b", "a&amp"] {
            assert!(refused(text, false), "{text}");
        }
    }

    #[test]
    fn refuses_a_number_that_names_no_character_of_a_document() {
        // A surrogate is not a character. The control codes and the two
        // non-characters are characters that XML 1.0 forbids in a document.
        for text in [
            "a&#xD800;b",
            "a&#0;b",
            "a&#x8;b",
            "a&#xB;b",
            "a&#x1F;b",
            "a&#xFFFE;b",
            "a&#xFFFF;b",
            "a&#x110000;b",
        ] {
            assert!(refused(text, false), "{text}");
        }
        // The three control characters that XML does allow.
        assert_eq!(decoded("a&#x9;&#xA;&#xD;b", false), "a\t\n\rb");
    }

    #[test]
    fn undoes_percent_encoding_only_when_asked() {
        assert_eq!(decoded("a%20b%2Fc", true), "a b/c");
        assert_eq!(decoded("a%20b%2Fc", false), "a%20b%2Fc");
        // The references are undone first, so a name may hold both.
        assert_eq!(decoded("a&amp;b%20c", true), "a&b c");
        assert_eq!(decoded("100%25", true), "100%");
        // An encoded name is encoded whole, so a `%` that does not begin an
        // escape is a fault.
        for text in ["100%25 %zz", "a%", "a%2", "a%2Gb"] {
            assert!(refused(text, true), "{text}");
        }
    }
}
