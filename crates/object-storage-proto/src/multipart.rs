// The `multipart/mixed` bodies of RFC 2046 §5.1, which an Azure Blob Batch
// sends and answers with: the boundary that the `Content-Type` names, the
// parts between its delimiters, and the delimiters that a request writes.
//
// The reader is strict. It takes the bodies that the RFC allows and that
// Azure writes, and refuses everything else:
//
// - The boundary is 1 to 70 of the RFC's `bchars`, and does not end in a
//   space. The `Content-Type` is `multipart/mixed` and names it once, as a
//   token or as a quoted string without escapes.
// - The body begins with the first delimiter: there is no preamble.
// - A delimiter is CRLF, `--` and the boundary, then CRLF before the next
//   part or `--` after the last. The CRLF before it belongs to it, not to
//   the part. No transport padding follows the boundary.
// - After the close delimiter comes nothing, or one CRLF.
// - CRLF, `--` and the boundary appear nowhere but in a delimiter. The RFC
//   requires that, so a part that holds them is a fault rather than two
//   parts.
// - Each part is a field section, the empty line, and the content. Every line
//   of the field section ends in CRLF.

use crate::http_message::{field_line, field_lines, next_line, token_byte, trim_whitespace};
use crate::request::ByteSink;
use crate::{ResponseFault, Result};

// The longest boundary that RFC 2046 allows.
const MAX_BOUNDARY_LEN: usize = 70;

// Returns the boundary that a `multipart/mixed` `Content-Type` names. A value
// that is not one is a fault of the head that carries it.
pub(crate) fn boundary(content_type: &[u8]) -> Result<&[u8]> {
    let head_fault = || Err(ResponseFault::Head.into());
    let mut pieces = content_type.split(|byte| *byte == b';');
    let media_type = trim_whitespace(pieces.next().unwrap_or_default());
    if !media_type.eq_ignore_ascii_case(b"multipart/mixed") {
        return head_fault();
    }
    let mut boundary = None;
    for parameter in pieces {
        let parameter = trim_whitespace(parameter);
        let Some(equals) = parameter.iter().position(|byte| *byte == b'=') else {
            return head_fault();
        };
        let (name, value) = (&parameter[..equals], &parameter[equals + 1..]);
        if name.is_empty() || !name.iter().all(|byte| token_byte(*byte)) {
            return head_fault();
        }
        let value = match value {
            [b'"', inner @ .., b'"'] if !inner.contains(&b'\\') && !inner.contains(&b'"') => inner,
            value if !value.is_empty() && value.iter().all(|byte| token_byte(*byte)) => value,
            _ => return head_fault(),
        };
        if name.eq_ignore_ascii_case(b"boundary") {
            if boundary.is_some() {
                return head_fault();
            }
            boundary = Some(value);
        }
    }
    match boundary {
        Some(boundary) if valid_boundary(boundary) => Ok(boundary),
        _ => head_fault(),
    }
}

// RFC 2046's `boundary`: 1 to 70 `bchars`, the last of which is not a space.
pub(crate) fn valid_boundary(boundary: &[u8]) -> bool {
    let bchar = |byte: &u8| byte.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(byte);
    (1..=MAX_BOUNDARY_LEN).contains(&boundary.len())
        && boundary.iter().all(bchar)
        && boundary.last() != Some(&b' ')
}

// One part of a body: its field section and its content.
#[derive(Clone, Copy)]
pub(crate) struct Part<'b> {
    // The field lines, each ending in CRLF, with no empty line after them.
    fields: &'b [u8],
    pub(crate) content: &'b [u8],
}

impl<'b> Part<'b> {
    // The fields of the part, in order. `parts` checked each line.
    pub(crate) fn fields(&self) -> impl Iterator<Item = (&'b str, &'b [u8])> + use<'b> {
        field_lines(self.fields)
    }
}

// The parts of `body`, in order. The first fault ends them.
pub(crate) fn parts<'b, 'n>(body: &'b [u8], boundary: &'n [u8]) -> Parts<'b, 'n> {
    Parts {
        rest: body,
        boundary,
        state: State::Start,
    }
}

pub(crate) struct Parts<'b, 'n> {
    rest: &'b [u8],
    boundary: &'n [u8],
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    // Nothing is read yet.
    Start,
    // A delimiter was read, so a part follows.
    Part,
    // The close delimiter was read, or a fault.
    Done,
}

impl<'b> Iterator for Parts<'b, '_> {
    type Item = Result<Part<'b>>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = match self.state {
            State::Done => return None,
            State::Start => self.open().and_then(|()| self.part()),
            State::Part => self.part(),
        };
        if item.is_err() {
            self.state = State::Done;
        }
        Some(item)
    }
}

impl<'b> Parts<'b, '_> {
    // Reads the first delimiter, which no CRLF precedes.
    fn open(&mut self) -> Result<()> {
        let rest = self
            .rest
            .strip_prefix(b"--")
            .and_then(|rest| rest.strip_prefix(self.boundary))
            .and_then(|rest| rest.strip_prefix(b"\r\n"));
        self.rest = rest.ok_or(ResponseFault::Body)?;
        self.state = State::Part;
        Ok(())
    }

    // Reads one part and the delimiter after it.
    fn part(&mut self) -> Result<Part<'b>> {
        let end = find_delimiter(self.rest, self.boundary).ok_or(ResponseFault::Body)?;
        let part = &self.rest[..end];
        let after = &self.rest[end + 4 + self.boundary.len()..];
        if let Some(rest) = after.strip_prefix(b"\r\n") {
            self.rest = rest;
        } else if let Some(epilogue) = after.strip_prefix(b"--") {
            if !matches!(epilogue, b"" | b"\r\n") {
                return Err(ResponseFault::Body.into());
            }
            self.rest = &[];
            self.state = State::Done;
        } else {
            // The boundary appears where no delimiter is.
            return Err(ResponseFault::Body.into());
        }
        read_part(part)
    }
}

// Where the next `CRLF--boundary` begins.
fn find_delimiter(bytes: &[u8], boundary: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = bytes
        .get(from..)?
        .windows(4)
        .position(|window| window == b"\r\n--")
    {
        let at = from + at;
        if bytes[at + 4..].starts_with(boundary) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

// Splits a part into its field section and its content, and checks each
// field line.
fn read_part(part: &[u8]) -> Result<Part<'_>> {
    let mut rest = part;
    loop {
        let at = part.len() - rest.len();
        let (line, after) = next_line(rest)?;
        if line.is_empty() {
            return Ok(Part {
                fields: &part[..at],
                content: after,
            });
        }
        field_line(line)?;
        rest = after;
    }
}

// Writes the delimiter before the first part.
pub(crate) fn write_first_delimiter(out: &mut dyn ByteSink, boundary: &str) {
    out.push(b"--");
    out.push(boundary.as_bytes());
    out.push(b"\r\n");
}

// Writes the delimiter between two parts.
pub(crate) fn write_delimiter(out: &mut dyn ByteSink, boundary: &str) {
    out.push(b"\r\n--");
    out.push(boundary.as_bytes());
    out.push(b"\r\n");
}

// Writes the delimiter after the last part.
pub(crate) fn write_close_delimiter(out: &mut dyn ByteSink, boundary: &str) {
    out.push(b"\r\n--");
    out.push(boundary.as_bytes());
    out.push(b"--\r\n");
}
