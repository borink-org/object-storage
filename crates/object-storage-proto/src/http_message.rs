// Reads an HTTP/1.1 response that another response carries whole, as text in
// its body: the status line and the field lines of RFC 9112, then the
// content. Each part of an Azure Blob Batch answer holds one. The host's
// HTTP client reads every response that arrives on a connection; this reads
// only one that is already in a buffer.
//
// The reader is strict. It refuses anything that RFC 9112 does not allow,
// and anything that a response inside a body has no use for:
//
// - The status line is `HTTP/1.1`, a space, a status code of three digits
//   from 100 to 599, a space, and a reason phrase that may be empty.
// - A field line is a token, a colon right after it, and a value of visible
//   characters with spaces and tabs inside it. A line folded onto the next,
//   a space before the colon, and a control character are refused.
// - Every line ends in CRLF. A bare CR or LF is refused.
// - `Transfer-Encoding` is refused: the body that carries the response
//   frames it.
// - `Content-Length` is required exactly when the response has content, and
//   must equal the length of the content. It may appear once.
//
// A response with no content may leave out the empty line that ends its
// head. A multipart body ends each part with the CRLF that begins the next
// delimiter, and that CRLF stands for the empty line: the answer of an Azure
// batch is written so, as its documentation shows.

use crate::common::decimal;
use crate::{ResponseFault, ResponseHead, Result};

// One response, read and checked.
#[derive(Clone, Copy)]
pub(crate) struct Response<'b> {
    pub(crate) status: u16,
    // The field lines, each ending in CRLF, with no empty line after them.
    fields: &'b [u8],
    pub(crate) content: &'b [u8],
}

impl<'b> Response<'b> {
    // Reads and checks the response that `message` holds, all of it.
    pub(crate) fn parse(message: &'b [u8]) -> Result<Self> {
        let (status_line, mut rest) = next_line(message)?;
        let status = status(status_line)?;
        let fields_start = message.len() - rest.len();
        let (fields_end, content) = loop {
            if rest.is_empty() {
                // The empty line was left out, so the response has no
                // content.
                break (message.len(), rest);
            }
            let at = message.len() - rest.len();
            let (line, after) = next_line(rest)?;
            if line.is_empty() {
                break (at, after);
            }
            field_line(line)?;
            rest = after;
        };
        let response = Self {
            status,
            fields: &message[fields_start..fields_end],
            content,
        };
        response.check_framing()?;
        Ok(response)
    }

    // The fields of the head, in order. `parse` checked each line.
    pub(crate) fn fields(&self) -> impl Iterator<Item = (&'b str, &'b [u8])> + use<'b> {
        field_lines(self.fields)
    }

    // The head, as the methods that read a response head take it.
    pub(crate) fn head(&self) -> ResponseHead<'b> {
        ResponseHead::from_headers(self.status, self.fields())
    }

    // `Content-Length` appears at most once, is required exactly when there
    // is content, and states its length. `Transfer-Encoding` does not
    // appear.
    fn check_framing(&self) -> Result<()> {
        let mut length = None;
        for (name, value) in self.fields() {
            if name.eq_ignore_ascii_case("transfer-encoding") {
                return fault();
            }
            if name.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return fault();
                }
                length = Some(decimal(value).ok_or(ResponseFault::Body)?);
            }
        }
        match length {
            None if self.content.is_empty() => Ok(()),
            Some(length) if length == self.content.len() as u64 => Ok(()),
            _ => fault(),
        }
    }
}

// Splits off the first line, which ends in CRLF, and returns it without the
// CRLF. A line holds no other CR or LF.
pub(crate) fn next_line(bytes: &[u8]) -> Result<(&[u8], &[u8])> {
    let Some(end) = bytes.iter().position(|byte| matches!(byte, b'\r' | b'\n')) else {
        return fault();
    };
    if bytes.get(end..end + 2) != Some(b"\r\n") {
        return fault();
    }
    Ok((&bytes[..end], &bytes[end + 2..]))
}

// Reads the status code of `HTTP/1.1 NNN reason`.
fn status(line: &[u8]) -> Result<u16> {
    let Some(rest) = line.strip_prefix(b"HTTP/1.1 ") else {
        return fault();
    };
    let (code, reason) = match rest {
        [a, b, c, b' ', reason @ ..] if [a, b, c].iter().all(|digit| digit.is_ascii_digit()) => (
            u16::from(a - b'0') * 100 + u16::from(b - b'0') * 10 + u16::from(c - b'0'),
            reason,
        ),
        _ => return fault(),
    };
    if !(100..=599).contains(&code) || !reason.iter().all(|byte| text_byte(*byte)) {
        return fault();
    }
    Ok(code)
}

// Checks one field line and returns its name and its value, without the
// whitespace around the value.
pub(crate) fn field_line(line: &[u8]) -> Result<(&str, &[u8])> {
    let Some(colon) = line.iter().position(|byte| *byte == b':') else {
        return fault();
    };
    let (name, value) = (&line[..colon], &line[colon + 1..]);
    if name.is_empty() || !name.iter().all(|byte| token_byte(*byte)) {
        return fault();
    }
    if !value.iter().all(|byte| text_byte(*byte)) {
        return fault();
    }
    // A token is ASCII.
    let name = core::str::from_utf8(name).or(Err(ResponseFault::Body))?;
    Ok((name, trim_whitespace(value)))
}

// The name and the value of each line of a field section that was checked.
pub(crate) fn field_lines(fields: &[u8]) -> impl Iterator<Item = (&str, &[u8])> {
    fields
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| field_line(line.strip_suffix(b"\r").unwrap_or(line)).ok())
}

// The characters of a token, RFC 9110's `tchar`.
pub(crate) fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

// What a field value or a reason phrase may hold: visible characters, bytes
// above ASCII, spaces and tabs.
fn text_byte(byte: u8) -> bool {
    matches!(byte, b'\t' | b' '..=b'~' | 0x80..=0xff)
}

// Removes RFC 9110's optional whitespace, spaces and tabs, from both ends.
pub(crate) fn trim_whitespace(mut value: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = value {
        value = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = value {
        value = rest;
    }
    value
}

fn fault<T>() -> Result<T> {
    Err(ResponseFault::Body.into())
}
