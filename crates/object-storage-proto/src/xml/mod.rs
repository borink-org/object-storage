// Reads the XML that a blob service sends back: a listing page, and the error
// document that names the error code in the body.
//
// The reader walks the document structure directly; it does not tokenise.
// `scan.rs` walks the bytes once, `decode.rs` undoes the escaping in place in
// the caller's buffer, and `azure.rs` knows the shape of a listing page. This
// file reads the error document, which is only three elements. It also walks
// the properties of an entry, so the caller can read the ones this crate
// skips.
//
// A page is read in this order. `azure::check_body` checks the body is UTF-8
// and holds no zero byte. `azure::open_root_element` skips the prolog and
// reads through the root's opening tag. `azure::read_root_children_into` then
// loops over the root's children, and on the one that holds the entries calls
// `azure::read_entries_into`, which reads each entry's fields as spans, takes
// the entry off the body as its own slice, and has `azure::build_entry`
// decode those spans in place and build the `ListEntry` from them. An entry
// the array has no room for is walked but not built, so that the error can
// say how many entries the page holds.

pub(crate) mod azure;
pub(crate) mod decode;
pub(crate) mod scan;

pub(crate) use azure::fill_listing;
pub(crate) use decode::decode_text;

use scan::{find_byte, trim};

/// Returns the error code that a service wrote into a response body.
///
/// This is the text of the first non-empty `<Code>` element, at any depth. A
/// body that was cut short yields whatever part of the code was written before
/// the cut. [`classify_error`](crate::classify_error) knows the body was cut
/// short and reports such a code as incomplete rather than unknown.
pub(crate) fn error_code(body: &[u8]) -> Option<&str> {
    // The code is returned as `&str`, so a body that is not valid UTF-8 has
    // no code.
    core::str::from_utf8(body).ok()?;
    let mut at = 0;
    while at < body.len() {
        let open = find_byte(body, at, b'<');
        // A tag that the body was cut off in the middle of has no name.
        let close = find_byte(body, open, b'>');
        if close == body.len() {
            return None;
        }
        let tag = &body[open + 1..close];
        let name = tag
            .split(|byte| byte.is_ascii_whitespace() || *byte == b'/')
            .next()
            .unwrap_or_default();
        if name == b"Code" && !tag.ends_with(b"/") {
            let end = find_byte(body, close + 1, b'<');
            let (start, end) = trim(body, (close + 1, end));
            // An empty element is not the code. A later one may still be.
            if start < end {
                return core::str::from_utf8(&body[start..end]).ok();
            }
        }
        at = close + 1;
    }
    None
}

// The element that holds the properties of one entry. The walk below reports
// the elements inside it and beside it in one sequence. Azure puts a property
// under this element, and S3 puts it next to the entry.
const PROPERTIES: &[u8] = b"Properties";

// Returns the bytes after the entry's own opening tag.
pub(crate) fn after_opening_tag(raw: &[u8]) -> Option<&[u8]> {
    let end = find_byte(raw, 0, b'>');
    (end < raw.len()).then(|| &raw[end + 1..])
}

// Reads the next element of an entry and returns its name and its text.
//
// `rest` starts after the entry's own opening tag. Each call advances it past
// what it read, so a whole walk reads each byte once. `within` records
// whether the walk is inside the properties element. The walk steps into that
// element instead of reporting it.
//
// A byte that is not the start of an element ends the walk. The document was
// checked when the page was read, before any entry was handed out. So this is
// the end of the entry, not a fault.
pub(crate) fn next_property<'b>(
    rest: &mut &'b [u8],
    within: &mut bool,
) -> Option<(&'b [u8], &'b [u8])> {
    loop {
        let space = rest
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let Some(after) = rest[space..].strip_prefix(b"<") else {
            *rest = &[];
            return None;
        };
        // A close tag ends the properties element, or the entry itself.
        if let Some(closing) = after.strip_prefix(b"/") {
            let end = opening_tag_end(closing)? + 1;
            if *within && closing.starts_with(PROPERTIES) {
                *within = false;
                *rest = &closing[end..];
                continue;
            }
            *rest = &[];
            return None;
        }
        let tag = opening_tag_end(after)?;
        // The name is everything up to the first space, where an attribute
        // would begin, or the `/` of an empty element.
        let name_end = after[..tag]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || *byte == b'/')
            .unwrap_or(tag);
        let name = &after[..name_end];
        let body = &after[tag + 1..];
        let empty = after[..tag].ends_with(b"/");
        // Step into the properties element instead of reporting it. An empty
        // one has nothing to step into.
        if !*within && name == PROPERTIES {
            *within = !empty;
            *rest = body;
            continue;
        }
        // An empty element has no value and no close tag.
        if empty {
            *rest = body;
            return Some((name, &[]));
        }
        let (value, after_close) =
            split_at_decoded_value(body, name).or_else(|| split_at_close_tag(body, name))?;
        *rest = after_close;
        return Some((name, value));
    }
}

// The elements whose text reading the page decodes in place.
const DECODED: [&[u8]; 3] = [b"Name", b"Etag", b"Last-Modified"];

// Returns the decoded text of an element that reading the page decoded in
// place, and the bytes after its close tag. Returns `None` if this element
// was not decoded.
//
// A decoded value is shorter than the text it replaced, and the bytes it no
// longer needs were set to zero. The document held no zero byte, so the first
// zero byte in the entry ends a decoded value, and the close tag after that
// run says which element the value belongs to. This is checked before the
// close tag is searched for, because a decoded key can hold `<` and `>`: a key
// spelled `</Name><X>y</X>` would otherwise be read as those elements.
fn split_at_decoded_value<'b>(body: &'b [u8], name: &[u8]) -> Option<(&'b [u8], &'b [u8])> {
    if !DECODED.contains(&name) {
        return None;
    }
    let zero = find_byte(body, 0, 0);
    if zero == body.len() {
        return None;
    }
    let mut at = zero;
    while body.get(at) == Some(&0) {
        at += 1;
    }
    while body.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    let tail = body[at..].strip_prefix(b"</")?.strip_prefix(name)?;
    let close = tail
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(tail.len());
    (tail.get(close) == Some(&b'>')).then(|| (&body[..zero], &tail[close + 1..]))
}

// Returns the value of an element and the bytes after its close tag. The
// close tag is found by name, so an element that holds other elements is
// returned whole.
fn split_at_close_tag<'b>(body: &'b [u8], name: &[u8]) -> Option<(&'b [u8], &'b [u8])> {
    let mut at = 0;
    loop {
        let found = find_byte(body, at, b'<');
        if found == body.len() {
            return None;
        }
        let after = body.get(found + 2..)?;
        if body[found + 1] == b'/' && after.starts_with(name) {
            // XML allows whitespace before the `>` of a close tag. Azure
            // never writes it; this is for consistency with the page reader.
            let tail = &after[name.len()..];
            let close = tail
                .iter()
                .position(|byte| !byte.is_ascii_whitespace())
                .unwrap_or(tail.len());
            if tail.get(close) == Some(&b'>') {
                return Some((&body[..found], &tail[close + 1..]));
            }
        }
        at = found + 1;
    }
}

// Returns the offset of the `>` that ends a tag, counted from just after its
// `<`.
fn opening_tag_end(after: &[u8]) -> Option<usize> {
    let end = find_byte(after, 0, b'>');
    (end < after.len()).then_some(end)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::error_code;

    #[test]
    fn reads_the_code_from_an_azure_error_body() {
        assert_eq!(
            error_code(b"<?xml version=\"1.0\"?><Error><Code>BlobNotFound</Code><Message>The specified blob does not exist.</Message></Error>"),
            Some("BlobNotFound")
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            error_code(b"<Error>\n  <Code>\n    ServerBusy\n  </Code>\n</Error>"),
            Some("ServerBusy")
        );
    }

    #[test]
    fn finds_the_code_at_any_depth() {
        assert_eq!(
            error_code(b"<Error><Detail><Code>InternalError</Code></Detail></Error>"),
            Some("InternalError")
        );
    }

    #[test]
    fn returns_nothing_without_a_code_element() {
        assert_eq!(
            error_code(b"<Error><Message>no code here</Message></Error>"),
            None
        );
        assert_eq!(error_code(b""), None);
    }

    #[test]
    fn returns_nothing_for_a_code_element_with_no_text() {
        assert_eq!(error_code(b"<Error><Code /></Error>"), None);
        assert_eq!(
            error_code(b"<Error><Code></Code><Code>Late</Code></Error>"),
            Some("Late")
        );
    }

    #[test]
    fn ignores_text_outside_the_code_element() {
        assert_eq!(
            error_code(b"<Error><Message>BlobNotFound</Message><Code>ServerBusy</Code></Error>"),
            Some("ServerBusy")
        );
    }

    #[test]
    fn returns_nothing_for_a_body_that_is_not_utf_8() {
        assert_eq!(error_code(b"<Error><Code>\xff</Code></Error>"), None);
    }

    #[test]
    fn a_body_cut_short_yields_at_most_a_partial_code() {
        // `classify_error` is told separately that the body was truncated, so a
        // partial code that matches nothing becomes `Incomplete`, not `Unknown`.
        assert_eq!(error_code(b"<Error><Code>BlobNot"), Some("BlobNot"));
        assert_eq!(error_code(b"<Error><Cod"), None);
    }
}
