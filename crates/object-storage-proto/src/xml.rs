use core::mem;
use core::str;

use xmlparser::{ElementEnd, Token, Tokenizer};

use crate::{EntryKind, Error, Fill, ListEntry, Listing, ResponseFault, Result, Resume};

pub(crate) fn error_code(body: &[u8]) -> Option<&str> {
    let body = core::str::from_utf8(body).ok()?;
    let mut depth = 0usize;
    let mut code_depth = None;
    for token in Tokenizer::from(body) {
        match token.ok()? {
            Token::ElementStart { local, .. } => {
                depth += 1;
                if local.as_str() == "Code" {
                    code_depth = Some(depth);
                }
            }
            Token::Text { text } if code_depth == Some(depth) => {
                return Some(text.as_str().trim());
            }
            Token::ElementEnd { end, .. } => match end {
                ElementEnd::Open => {}
                ElementEnd::Empty | ElementEnd::Close(..) => {
                    if code_depth == Some(depth) {
                        code_depth = None;
                    }
                    depth = depth.checked_sub(1)?;
                }
            },
            _ => {}
        }
    }
    None
}

// The listing document, read in two passes over one buffer.
//
// The first pass is a tokeniser run over the whole body as `&str`: it checks
// that this is a listing, finds where the entries start, and records where the
// next marker's text stands. The second walks the entries as bytes, splitting
// each one off as its own `&mut [u8]` so that the text inside it can be
// decoded where it stands and then lent out. Splitting is what keeps the walk
// linear without one borrow of the whole body being held across it.
//
// The walk matches the tags around an entry byte for byte. That is sound only
// because the first pass has already refused everything that could put those
// bytes anywhere else: a comment, a character-data section, and any tag that
// does not close the element it claims to. A tag the walk cannot match is a
// fault, so a document written some other way is refused rather than misread.

const ROOT: &str = "EnumerationResults";
const ENTRIES: &str = "Blobs";
const MARKER: &str = "NextMarker";
const ENTRIES_CLOSE: &[u8] = b"</Blobs>";
const OBJECT_OPEN: &[u8] = b"<Blob>";
const OBJECT_CLOSE: &[u8] = b"</Blob>";
const PREFIX_OPEN: &[u8] = b"<BlobPrefix>";
const PREFIX_CLOSE: &[u8] = b"</BlobPrefix>";

pub(crate) fn fill_listing<'b>(body: &'b mut [u8], into: &mut [ListEntry<'b>]) -> Result<Fill<'b>> {
    // Both tokeniser passes read `&str`, so the bytes are checked here and
    // again per entry, and neither check can be dropped without `unsafe`.
    // Measured over a 5000-entry page, the two checks together are 0.6% of
    // the read: validating runs at some 50 GB/s and tokenising at 0.55 GB/s,
    // so what this costs is lost in what it feeds.
    let page = locate_page(str::from_utf8(body).map_err(|_| fault())?)?;
    read_page(body, page, into)
}

pub(crate) fn resume_listing<'b>(
    body: &'b mut [u8],
    resume: Resume,
    into: &mut [ListEntry<'b>],
) -> Result<Fill<'b>> {
    read_page(body, resume, into)
}

// review: "where the entries do,"? that seems like a language error?
// The second pass. It starts where `locate_page` said the entries do, or where
// a previous call stopped, and reads until the array is full or the page ends.
fn read_page<'b>(body: &'b mut [u8], page: Resume, into: &mut [ListEntry<'b>]) -> Result<Fill<'b>> {
    let total = body.len();
    if page.at > total {
        return Err(fault());
    }
    // The marker stands past the entries, so the walk and the marker are two
    // pieces of the body that are never the same bytes.
    let (before, mut rest) = body.split_at_mut(page.at);
    let mut filled = 0;

    if page.within {
        loop {
            skip_whitespace(&mut rest);
            if rest.starts_with(ENTRIES_CLOSE) {
                advance(&mut rest, ENTRIES_CLOSE.len());
                break;
            }
            // The array is the budget. An entry that does not fit is not read
            // at all, so the bytes it stands in are still a document and the
            // next call reads it from there.
            if filled == into.len() {
                return Ok(Fill::Partial {
                    filled,
                    resume: Resume {
                        at: total - rest.len(),
                        within: true,
                        marker: page.marker,
                    },
                });
            }
            let (prefix, close) = if rest.starts_with(OBJECT_OPEN) {
                (false, OBJECT_CLOSE)
            } else if rest.starts_with(PREFIX_OPEN) {
                (true, PREFIX_CLOSE)
            } else {
                return Err(fault());
            };
            let end = find(rest, close).ok_or_else(fault)? + close.len();
            into[filled] = read_entry(split_off(&mut rest, end), prefix)?;
            filled += 1;
        }
    }

    let next_marker = match page.marker {
        None => None,
        Some(span) => {
            let text = if span.1 <= page.at {
                slice(before, span)?
            } else {
                let at = total - rest.len();
                slice(mem::take(&mut rest), shift(span, at)?)?
            };
            let len = decode_text(text, false);
            let text: &'b [u8] = text;
            // An empty marker is how the service says that the listing is
            // complete, so it names no next page rather than an empty one.
            (len > 0).then(|| &text[..len])
        }
    };
    Ok(Fill::Page(Listing {
        filled,
        next_marker,
    }))
}

// One tokeniser pass over the whole document. It refuses anything that is not
// a listing, including the error document that a service can put under a
// success status, reports where the entries begin, and records the marker's
// text rather than leaving the walk to recognise the tag that carries it: the
// service writes that tag as `<NextMarker/>`, as `<NextMarker />`, and with
// text between two tags, and all three name the same thing.
fn locate_page(text: &str) -> Result<Resume> {
    let mut open = Open::new();
    // The element that holds the entries: the depth it sits at while it is
    // open, then where its content begins and whether it has any.
    let mut entries_at = None;
    let mut at = None;
    let mut within = false;
    // The element that holds the marker, and its text.
    let mut marker_at = None;
    let mut marker = None;

    for token in Tokenizer::from(text) {
        let token = token.map_err(|_| fault())?;
        // Everything the walk reads byte for byte stands here.
        let entries = entries_at.is_some();
        match token {
            Token::ElementStart { local, .. } => {
                let depth = open.start(local.as_str())?;
                match local.as_str() {
                    name if depth == 1 && name != ROOT => return Err(fault()),
                    // A service can answer a listing with an error document
                    // under a success status. That is not a page.
                    "Code" => return Err(fault()),
                    ENTRIES if depth == 2 && at.is_none() => entries_at = Some(depth),
                    MARKER if depth == 2 && marker.is_none() => marker_at = Some(depth),
                    _ => {}
                }
            }
            Token::Text { text: value } if marker_at == Some(open.depth()) => {
                // Text in more than one piece would mean that something stood
                // between the pieces, and taking one of them would name a page
                // that the service did not.
                if marker.is_some() {
                    return Err(fault());
                }
                marker = Some(trim(text.as_bytes(), (value.start(), value.end())));
            }
            Token::ElementEnd { end, span } => {
                if entries_at == Some(open.depth()) && at.is_none() {
                    at = Some(span.end());
                    within = matches!(end, ElementEnd::Open);
                }
                if !matches!(end, ElementEnd::Open) {
                    if entries_at == Some(open.depth()) {
                        entries_at = None;
                    }
                    if marker_at == Some(open.depth()) {
                        marker_at = None;
                    }
                    open.end(&end)?;
                }
            }
            // A comment, a character-data section and a processing
            // instruction may hold any bytes at all, including the tags that
            // the walk looks for. None of the three belongs in a listing, so
            // all three are refused where the walk would trip over them.
            Token::Comment { .. } | Token::Cdata { .. } | Token::ProcessingInstruction { .. }
                if entries =>
            {
                return Err(fault());
            }
            // A listing carries no document type, and this crate expands no
            // entity that one could declare.
            Token::DtdStart { .. } | Token::EmptyDtd { .. } => return Err(fault()),
            _ => {}
        }
    }
    // A body that was cut short leaves elements open. The walk would fault on
    // it anyway; saying so here is what makes the fault the same either way.
    if open.depth() != 0 {
        return Err(fault());
    }
    Ok(Resume {
        at: at.ok_or_else(fault)?,
        within,
        marker,
    })
}

// A listing nests the root, the entries, one entry and its properties, so an
// element inside a property is already deeper than a listing goes.
const MAX_DEPTH: usize = 16;

// The names of the elements that are open. A close tag names the element that
// it closes, and the tokeniser hands that name over without checking it, so a
// document whose tags cross would otherwise be read as though it nested. This
// is what checks it.
struct Open<'t> {
    names: [&'t str; MAX_DEPTH],
    depth: usize,
}

impl<'t> Open<'t> {
    const fn new() -> Self {
        Self {
            names: [""; MAX_DEPTH],
            depth: 0,
        }
    }

    fn depth(&self) -> usize {
        self.depth
    }

    // Opens one element and returns the depth that it sits at.
    fn start(&mut self, local: &'t str) -> Result<usize> {
        // The array is what lets this pass check the nesting without a heap.
        // A document deeper than it is not a listing.
        *self.names.get_mut(self.depth).ok_or_else(fault)? = local;
        self.depth += 1;
        Ok(self.depth)
    }

    // Closes the innermost element, refusing a tag that names another one.
    fn end(&mut self, end: &ElementEnd<'t>) -> Result<()> {
        let closed = match end {
            ElementEnd::Open => return Ok(()),
            ElementEnd::Empty => None,
            ElementEnd::Close(_, local) => Some(local.as_str()),
        };
        self.depth = self.depth.checked_sub(1).ok_or_else(fault)?;
        match closed {
            Some(name) if self.names[self.depth] != name => Err(fault()),
            _ => Ok(()),
        }
    }
}

// The fields of one entry, as ranges of the entry's own bytes. Recording
// ranges rather than slices is what lets the text be decoded afterwards, when
// the tokeniser no longer holds the entry.
#[derive(Default)]
struct Fields {
    name: Option<(usize, usize)>,
    name_encoded: bool,
    size: Option<(usize, usize)>,
    e_tag: Option<(usize, usize)>,
    last_modified: Option<(usize, usize)>,
    resource_type: Option<(usize, usize)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Name,
    Size,
    ETag,
    LastModified,
    ResourceType,
}

fn read_entry(raw: &mut [u8], prefix: bool) -> Result<ListEntry<'_>> {
    let fields = scan_entry(str::from_utf8(raw).map_err(|_| fault())?, prefix)?;
    // A key may begin or end with a space, so only the values that the service
    // writes for itself are trimmed.
    let name = fields.name.ok_or_else(fault)?;
    // A directory is told from an object before any decoding, because the
    // value is the service's own word and carries nothing to decode.
    let directory = fields
        .resource_type
        .is_some_and(|span| at(raw, trim(raw, span)) == b"directory");
    let size = match fields.size {
        Some(span) => Some(crate::azure::decimal(at(raw, trim(raw, span))).ok_or_else(fault)?),
        // An object states its length. A group of keys and a directory are not
        // objects and have none to state.
        None if !prefix && !directory => return Err(fault()),
        None => None,
    };
    let kind = match (prefix, directory) {
        (true, _) => EntryKind::Prefix,
        (false, true) => EntryKind::Directory,
        (false, false) => EntryKind::Object,
    };

    let name = decode_span(raw, name, fields.name_encoded);
    let e_tag = fields
        .e_tag
        .map(|span| decode_span(raw, trim(raw, span), false));
    let last_modified = fields
        .last_modified
        .map(|span| decode_span(raw, trim(raw, span), false));

    let raw: &[u8] = raw;
    Ok(ListEntry {
        kind,
        key: str::from_utf8(&raw[name.0..name.1]).map_err(|_| fault())?,
        size: if directory { None } else { size },
        e_tag: e_tag
            .filter(|_| !directory)
            .map(|(start, end)| &raw[start..end]),
        last_modified: last_modified.map(|(start, end)| &raw[start..end]),
    })
}

// Decodes one span where it stands and returns what is left of it.
fn decode_span(raw: &mut [u8], (start, end): (usize, usize), percent: bool) -> (usize, usize) {
    (start, start + decode_text(&mut raw[start..end], percent))
}

// The tokeniser pass over one entry. The properties are read only under
// `<Properties>`, so an element that a later service version adds beside it
// cannot be mistaken for one of them.
fn scan_entry(text: &str, prefix: bool) -> Result<Fields> {
    let mut fields = Fields::default();
    let mut open = Open::new();
    let mut properties = false;
    let mut slot = None;
    for token in Tokenizer::from_fragment(text, 0..text.len()) {
        match token.map_err(|_| fault())? {
            Token::ElementStart { local, .. } => {
                // An element that this pass reads holds text and nothing else.
                if slot.is_some() {
                    return Err(fault());
                }
                let depth = open.start(local.as_str())?;
                let name = local.as_str();
                properties |= depth == 2 && name == "Properties";
                slot = match (depth, name) {
                    (2, "Name") => Some(Slot::Name),
                    // A hierarchical-namespace account gives a group of keys a
                    // `<Properties>` block too. Nothing consumes it: a group
                    // has no size and no entity tag to use.
                    (3, _) if prefix || !properties => None,
                    (3, "Content-Length") => Some(Slot::Size),
                    (3, "Etag") => Some(Slot::ETag),
                    (3, "Last-Modified") => Some(Slot::LastModified),
                    (3, "ResourceType") => Some(Slot::ResourceType),
                    _ => None,
                };
            }
            Token::Attribute { local, value, .. } => {
                if slot == Some(Slot::Name) && local.as_str() == "Encoded" {
                    fields.name_encoded = value.as_str() == "true";
                }
            }
            Token::Text { text } => {
                if let Some(slot) = slot {
                    let field = match slot {
                        Slot::Name => &mut fields.name,
                        Slot::Size => &mut fields.size,
                        Slot::ETag => &mut fields.e_tag,
                        Slot::LastModified => &mut fields.last_modified,
                        Slot::ResourceType => &mut fields.resource_type,
                    };
                    // Text in more than one piece would mean that something
                    // stood between the pieces, and taking one of them would
                    // quietly lose the rest of the value.
                    if field.is_some() {
                        return Err(fault());
                    }
                    *field = Some((text.start(), text.end()));
                }
            }
            // The text of an element follows the `>` that opens it, so only
            // the tags that close one end the field that it names.
            Token::ElementEnd { end, .. } if !matches!(end, ElementEnd::Open) => {
                slot = None;
                properties &= open.depth() != 2;
                open.end(&end)?;
            }
            Token::ElementEnd { .. } => {}
            // Nothing else belongs in an entry, and a comment between the
            // pieces of a value is exactly what must not be stepped over.
            _ => return Err(fault()),
        }
    }
    // An entry the walk cut short leaves elements open.
    if open.depth() != 0 {
        return Err(fault());
    }
    Ok(fields)
}

/// Undoes the escaping that XML applies to text, and the percent-encoding that
/// the service applies to a name that XML cannot carry.
///
/// Both only ever shorten the text, so both run left to right within the span,
/// and the bytes that they free at the end keep whatever they held. Returns
/// what is left of the text.
pub(crate) fn decode_text(bytes: &mut [u8], percent: bool) -> usize {
    let len = decode_references(bytes);
    if percent {
        decode_percent(&mut bytes[..len])
    } else {
        len
    }
}

fn decode_references(bytes: &mut [u8]) -> usize {
    let (mut read, mut write) = (0, 0);
    while read < bytes.len() {
        if bytes[read] == b'&'
            && let Some((decoded, consumed)) = reference(&bytes[read..])
        {
            let mut buffer = [0; 4];
            let decoded = decoded.encode_utf8(&mut buffer).as_bytes();
            bytes[write..write + decoded.len()].copy_from_slice(decoded);
            write += decoded.len();
            read += consumed;
            continue;
        }
        bytes[write] = bytes[read];
        write += 1;
        read += 1;
    }
    write
}

// The five references that XML defines, and the numeric form, with the number
// of bytes that each one occupies. Anything else is left as it stands: the
// service does not write it, and rewriting it would lose bytes that the caller
// may still want.
fn reference(bytes: &[u8]) -> Option<(char, usize)> {
    for (name, decoded) in [
        (b"&amp;".as_slice(), '&'),
        (b"&lt;", '<'),
        (b"&gt;", '>'),
        (b"&quot;", '"'),
        (b"&apos;", '\''),
    ] {
        if bytes.starts_with(name) {
            return Some((decoded, name.len()));
        }
    }
    let rest = bytes.strip_prefix(b"&#")?;
    let (digits, radix) = match rest.strip_prefix(b"x") {
        Some(rest) => (rest, 16),
        None => (rest, 10),
    };
    let end = digits.iter().position(|byte| *byte == b';')?;
    if end == 0 {
        return None;
    }
    let code = digits[..end].iter().try_fold(0u32, |value, byte| {
        let digit = (*byte as char).to_digit(radix)?;
        value.checked_mul(radix)?.checked_add(digit)
    })?;
    // `&#`, the `x` of the hexadecimal form, the digits and the `;`. Every
    // reference is longer than the character it stands for.
    Some((char::from_u32(code)?, bytes.len() - digits.len() + end + 1))
}

fn decode_percent(bytes: &mut [u8]) -> usize {
    let (mut read, mut write) = (0, 0);
    while read < bytes.len() {
        if bytes[read] == b'%'
            && let Some(decoded) = hex_pair(bytes, read + 1)
        {
            bytes[write] = decoded;
            write += 1;
            read += 3;
            continue;
        }
        bytes[write] = bytes[read];
        write += 1;
        read += 1;
    }
    write
}

fn hex_pair(bytes: &[u8], at: usize) -> Option<u8> {
    let pair = bytes.get(at..at + 2)?;
    let high = (pair[0] as char).to_digit(16)?;
    let low = (pair[1] as char).to_digit(16)?;
    Some((high * 16 + low) as u8)
}

fn fault() -> Error {
    Error::Response(ResponseFault::Body)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn skip_whitespace(rest: &mut &mut [u8]) {
    let count = rest
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(rest.len());
    advance(rest, count);
}

fn advance(rest: &mut &mut [u8], count: usize) {
    split_off(rest, count);
}

// Hands out the first `count` bytes as their own borrow of the body, which is
// what lets an entry be decoded while the walk goes on past it.
fn split_off<'b>(rest: &mut &'b mut [u8], count: usize) -> &'b mut [u8] {
    let (entry, tail) = mem::take(rest).split_at_mut(count);
    *rest = tail;
    entry
}

// One recorded range of a buffer, as its own borrow of it.
fn slice(bytes: &mut [u8], (start, end): (usize, usize)) -> Result<&mut [u8]> {
    if start > end || end > bytes.len() {
        return Err(fault());
    }
    let (_, tail) = bytes.split_at_mut(start);
    let (text, _) = tail.split_at_mut(end - start);
    Ok(text)
}

fn at(bytes: &[u8], (start, end): (usize, usize)) -> &[u8] {
    &bytes[start..end]
}

// The same range, counted from `at` rather than from the start of the body.
fn shift((start, end): (usize, usize), at: usize) -> Result<(usize, usize)> {
    Ok((
        start.checked_sub(at).ok_or_else(fault)?,
        end.checked_sub(at).ok_or_else(fault)?,
    ))
}

// A value that the service writes for itself, without the whitespace that the
// document may hold around it. A key is never trimmed: a key may begin or end
// with a space, and that space is part of it.
fn trim(bytes: &[u8], (mut start, mut end): (usize, usize)) -> (usize, usize) {
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::String;
    use std::vec::Vec;

    use super::{decode_text, error_code};

    fn decoded(text: &str, percent: bool) -> String {
        let mut bytes = Vec::from(text.as_bytes());
        let len = decode_text(&mut bytes, percent);
        String::from_utf8(Vec::from(&bytes[..len])).unwrap()
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
        // Four bytes out of nine is still shorter, which is what lets every
        // reference be undone where it stands.
        assert_eq!(decoded("&#x1F600;", false), "\u{1f600}");
    }

    #[test]
    fn leaves_alone_what_it_does_not_recognize() {
        // Not one of the five, no number, or no code point: the bytes are the
        // caller's, and rewriting them would lose what the service sent.
        assert_eq!(decoded("a&nbsp;b", false), "a&nbsp;b");
        assert_eq!(decoded("a&#;b", false), "a&#;b");
        assert_eq!(decoded("a&#xD800;b", false), "a&#xD800;b");
        assert_eq!(decoded("100% &", false), "100% &");
    }

    #[test]
    fn undoes_percent_encoding_only_when_asked() {
        assert_eq!(decoded("a%20b%2Fc", true), "a b/c");
        assert_eq!(decoded("a%20b%2Fc", false), "a%20b%2Fc");
        // The references are undone first, so a name may carry both.
        assert_eq!(decoded("a&amp;b%20c", true), "a&b c");
        assert_eq!(decoded("100%25 %zz %", true), "100% %zz %");
    }

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
