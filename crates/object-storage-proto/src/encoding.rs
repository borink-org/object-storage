// The byte-to-text encodings that requests and responses carry: hexadecimal,
// base64, the RFC 2047 encoded words of S3 metadata, and the text of an XML
// element. None allocates. Each writes into a buffer or a callback that the
// caller passes.

use crate::request::ByteSink;

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// Writes 32 bytes as lowercase hexadecimal, as SigV4 writes a digest.
pub(crate) fn hex(bytes: &[u8; 32]) -> [u8; 64] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0; 64];
    for (index, byte) in bytes.iter().enumerate() {
        out[2 * index] = DIGITS[usize::from(byte >> 4)];
        out[2 * index + 1] = DIGITS[usize::from(byte & 0xF)];
    }
    out
}

// Reads one hexadecimal digit of either case.
pub(crate) fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// Writes the standard base64 of `bytes` into `into`, which holds exactly the
// four characters per three bytes that it takes, and returns it as text.
pub(crate) fn base64_into<'a>(bytes: &[u8], into: &'a mut [u8]) -> &'a str {
    for (group, out) in bytes.chunks(3).zip(into.chunks_mut(4)) {
        // A group is 1 to 3 bytes; the missing ones read as zero and are
        // written as padding below. Each sextet index is at most 63.
        let bits = (u32::from(group[0]) << 16)
            | (u32::from(*group.get(1).unwrap_or(&0)) << 8)
            | u32::from(*group.get(2).unwrap_or(&0));
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = if i <= group.len() {
                BASE64[((bits >> (18 - 6 * i)) & 63) as usize]
            } else {
                b'='
            };
        }
    }
    // Every byte written is from the alphabet or padding, so this is ASCII.
    crate::request::text(into)
}

// Writes the bytes of padded standard base64 `text` into `into`, and returns
// how many it wrote. Returns `None` for text that is not base64, or if
// `into` is too small; the bytes never outnumber the characters.
pub(crate) fn decode_base64(text: &[u8], into: &mut [u8]) -> Option<usize> {
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let sextet = |byte: u8| -> Option<u32> {
        let index = BASE64.iter().position(|&digit| digit == byte)?;
        Some(index as u32)
    };
    let mut len = 0;
    let groups = text.len() / 4;
    for (index, group) in text.chunks(4).enumerate() {
        // Only the last group may end in padding.
        let padding = group.iter().rev().take_while(|&&byte| byte == b'=').count();
        if padding > 2 || (padding > 0 && index + 1 != groups) {
            return None;
        }
        let mut bits = 0;
        for &byte in &group[..4 - padding] {
            bits = (bits << 6) | sextet(byte)?;
        }
        bits <<= 6 * padding as u32;
        let count = 3 - padding;
        into.get_mut(len..len + count)?
            .copy_from_slice(&bits.to_be_bytes()[1..1 + count]);
        len += count;
    }
    Some(len)
}

// RFC 2047 encoded words, which carry text that one header value cannot
// hold. S3 decodes one in a metadata value when it stores the value, and
// returns a stored value outside ASCII as one. This module writes and reads
// the UTF-8 form alone.
pub(crate) mod rfc2047 {
    use super::{base64_into, decode_base64, hex_digit};
    use crate::request::ByteSink;

    // Writes `text` as one encoded word of its UTF-8, in base64:
    // `=?UTF-8?B?...?=`. The word holds no space.
    pub(crate) fn write(out: &mut dyn ByteSink, text: &str) {
        out.push(b"=?UTF-8?B?");
        for group in text.as_bytes().chunks(3) {
            let mut encoded = [0; 4];
            out.push(base64_into(group, &mut encoded).as_bytes());
        }
        out.push(b"?=");
    }

    // Returns whether a space-separated token of `text` starts with `=?` and
    // ends with `?=`. S3 reads each such token as an encoded word, and
    // decodes it or refuses the write.
    pub(crate) fn looks_encoded(text: &str) -> bool {
        text.split(' ')
            .any(|token| token.starts_with("=?") && token.ends_with("?="))
    }

    // Writes the text of `value`, one or more UTF-8 encoded words separated by
    // whitespace, into `into`, and returns its length. `into` holds at least as
    // many bytes as `value`. Returns `None` for any other value, and for words
    // whose bytes are not UTF-8.
    pub(crate) fn decode(value: &[u8], into: &mut [u8]) -> Option<usize> {
        let mut len = 0;
        let mut words = value
            .split(|byte| matches!(byte, b' ' | b'\t'))
            .filter(|word| !word.is_empty())
            .peekable();
        words.peek()?;
        for word in words {
            len += decode_word(word, &mut into[len..])?;
        }
        core::str::from_utf8(&into[..len]).ok()?;
        Some(len)
    }

    // Decodes one `=?UTF-8?B?...?=` or `=?UTF-8?Q?...?=`.
    fn decode_word(word: &[u8], into: &mut [u8]) -> Option<usize> {
        let inner = word.strip_prefix(b"=?")?.strip_suffix(b"?=")?;
        let mut parts = inner.split(|&byte| byte == b'?');
        let (charset, encoding, text) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || !charset.eq_ignore_ascii_case(b"UTF-8") {
            return None;
        }
        match encoding {
            b"B" | b"b" => decode_base64(text, into),
            b"Q" | b"q" => decode_q(text, into),
            _ => None,
        }
    }

    // The Q encoding: `_` is a space, `=XX` is the byte XX, and any other
    // printable character is itself.
    fn decode_q(text: &[u8], into: &mut [u8]) -> Option<usize> {
        let mut len = 0;
        let mut at = 0;
        while at < text.len() {
            let byte = match text[at] {
                b'_' => b' ',
                b'=' => {
                    let high = hex_digit(*text.get(at + 1)?)?;
                    let low = hex_digit(*text.get(at + 2)?)?;
                    at += 2;
                    high << 4 | low
                }
                byte if byte.is_ascii_graphic() => byte,
                _ => return None,
            };
            *into.get_mut(len)? = byte;
            len += 1;
            at += 1;
        }
        Some(len)
    }
}

// Writes `text` as the text of an XML element, with the bytes that XML
// text cannot hold as they are written as references.
pub(crate) fn write_xml_text(out: &mut dyn ByteSink, text: &[u8]) {
    let mut start = 0;
    for (at, byte) in text.iter().enumerate() {
        let reference: &[u8] = match byte {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            _ => continue,
        };
        out.push(&text[start..at]);
        out.push(reference);
        start = at + 1;
    }
    out.push(&text[start..]);
}
