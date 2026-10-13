// What both page readers share: checking the body, reading the root element,
// keeping the values of wanted properties, and building an entry taken off
// the body.

use crate::encoding::parse_decimal;
use crate::properties::{Property, PropertySet};
use crate::xml::decode::decode_scanned;
use crate::xml::scan::{Scan, Span, StartTag, body_fault, trim_span};
use crate::{CapacityError, Error, Result};

// A property whose value a page reader can keep: `azure::BlobProperty` or
// `s3::ObjectProperty`.
pub(super) trait ListProperty: Property {
    fn name(self) -> &'static str;

    // Whether the element holds other elements rather than one text.
    fn holds_elements(self) -> bool;

    // The property that an element name stands for, if any.
    fn identify(name: &[u8]) -> Option<Self>;
}

// Keeps the span of a property's value if `wanted` holds the property. A
// property written twice keeps the first.
#[inline(always)]
pub(super) fn keep_value<P: ListProperty>(
    property: P,
    span: Span,
    wanted: PropertySet<P>,
    captured: &mut [Option<Span>],
) {
    // The slot is in range: `captured` has one slot per member of `wanted`.
    // `get_mut` compiles in no panic path.
    if wanted.contains(property)
        && let Some(slot) = captured.get_mut(wanted.slot(property))
        && slot.is_none()
    {
        *slot = Some(span);
    }
}

// Reads past a property whose start tag was matched whole, and keeps its
// value if it is wanted. An element that holds other elements is read to its
// close tag, and its value is everything between its tags.
#[inline(always)]
pub(super) fn read_matched<P: ListProperty>(
    scan: &mut Scan<'_>,
    property: P,
    wanted: PropertySet<P>,
    captured: &mut [Option<Span>],
) -> Result<()> {
    let name = property.name().as_bytes();
    let span = if property.holds_elements() {
        scan.nested(name)?
    } else {
        scan.value_of(name)?.0
    };
    keep_value(property, span, wanted, captured);
    Ok(())
}

// Reads past an element that no whole-tag match took: one this crate does
// not know, or a known one in another spelling, such as `<AccessTier >`.
// Keeps the value of a wanted one.
pub(super) fn read_unmatched<P: ListProperty>(
    scan: &mut Scan<'_>,
    tag: StartTag,
    wanted: PropertySet<P>,
    captured: &mut [Option<Span>],
) -> Result<()> {
    if wanted.is_empty() {
        return scan.skip(tag);
    }
    match P::identify(scan.text(tag.name)) {
        Some(property) if wanted.contains(property) => {
            let span = if property.holds_elements() && !tag.empty {
                scan.nested(property.name().as_bytes())?
            } else {
                scan.value(tag)?.0
            };
            keep_value(property, span, wanted, captured);
            Ok(())
        }
        _ => scan.skip(tag),
    }
}

// Writes into `values` the part of an entry's bytes that each captured span
// names. The spans were recorded on those bytes.
pub(super) fn captured_values<'b>(
    raw: &'b [u8],
    spans: &[Option<Span>],
    values: &mut [Option<&'b [u8]>],
) {
    for (value, span) in values.iter_mut().zip(spans) {
        *value = span.map(|(start, end)| &raw[start..end]);
    }
}

// Refuses a page that holds more entries than the array has room for, with
// the number it holds.
pub(super) fn check_room(held: usize, room: usize) -> Result<()> {
    if held > room {
        return Err(Error::Capacity(CapacityError { required: held }));
    }
    Ok(())
}

// Reads the size of an object.
#[inline]
pub(super) fn read_size(chunk: &[u8], span: Span) -> Result<u64> {
    let (start, end) = trim_span(chunk, span);
    match parse_decimal(&chunk[start..end]) {
        Some(size) => Ok(size),
        None => body_fault(),
    }
}

// Zeroes the bytes that a decoded key no longer needs. The walk over the
// entry finds the end of the key by them, because a decoded key can hold `<`
// and `>`. See `next_property`. The scanner refused a zero byte in the
// document, so only this writes one.
//
// A percent escape can name a zero byte, which would look like that filler.
// If `escaped`, the key may hold one, and it is refused.
#[inline]
pub(super) fn end_decoded_key(
    chunk: &mut [u8],
    key: Span,
    len: usize,
    escaped: bool,
) -> Result<()> {
    if len < key.1 - key.0 {
        chunk[key.0 + len..key.1].fill(0);
        if escaped && chunk[key.0..key.0 + len].contains(&0) {
            return body_fault();
        }
    }
    Ok(())
}

// Checks the two properties of the whole body that the read relies on, once,
// before anything is read from it.
//
// The body must be valid UTF-8. A document that is not valid UTF-8 is not
// XML. The check runs at 45 to 50 GB/s and the read at 1.7 GB/s, so it costs
// a few percent. It does not replace the check each key gets after decoding,
// because a percent escape can produce any byte.
//
// Refusing the body is safe because Azure never sends invalid UTF-8. This was
// measured, not assumed. A key with an invalid byte is refused with
// `400 InvalidUri`. A query value with one comes back with `U+FFFD` in its
// place. So invalid UTF-8 here is a protocol violation, not a key the caller
// might hold. The measurement is `a_listing_body_is_always_utf_8` in the live
// suite.
// An S3 listing asks for URL-encoded keys, so its keys arrive as ASCII.
pub(super) fn check_body(body: &[u8]) -> Result<()> {
    // The body is valid UTF-8 from here on, but the reader keeps working on
    // bytes rather than turning it into a `str`. Values are decoded in place,
    // and a percent escape writes whatever byte it names, which may not be
    // UTF-8. The key is checked again after it is decoded and refused then,
    // but a `str` could not hold the bytes in between. The decoded values are
    // handed out as `str` once each is known to be text.
    //
    // XML forbids a zero byte in a document, and the reader writes zero over
    // the bytes a decoded value no longer needs, so that the walk over an
    // entry can find where the decoded text ends. A document that held a zero
    // byte of its own would defeat that.
    //
    // One pass finds the lowest byte and every bit that any byte sets. It is
    // written as a fold so that it compiles to two vector instructions per
    // sixteen bytes. A search that stops at the first hit does not, and costs
    // ten times as much. A body without a byte above 0x7F is ASCII, which is
    // UTF-8, so only a body with one is validated as UTF-8 as well.
    // Azure begins a body with the byte order mark, which is UTF-8 and the
    // only byte above 0x7F of an ASCII page; it is left out of the fold.
    let body = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body);
    let (lowest, bits) = body.iter().fold((u8::MAX, 0), |(lowest, bits), &byte| {
        (lowest.min(byte), bits | byte)
    });
    if lowest == 0 || (bits >= 0x80 && core::str::from_utf8(body).is_err()) {
        return body_fault();
    }
    Ok(())
}

// Reads the prolog and the opening tag of the root, which must be named
// `root`, and leaves the scan where the root's first child begins.
pub(super) fn open_root_element(scan: &mut Scan<'_>, root: &[u8]) -> Result<()> {
    // Azure begins a listing with the UTF-8 byte order mark, U+FEFF encoded
    // as these three bytes, before the XML declaration. It is not part of the
    // document.
    scan.lit(&[0xEF, 0xBB, 0xBF]);
    loop {
        scan.skip_space();
        if scan.cur() != b'<' {
            return body_fault();
        }
        if !scan.skip_misc()? {
            break;
        }
    }
    let tag = scan.open()?;
    // A service can answer a listing with an error document under a success
    // status. That is not a page.
    if scan.text(tag.name) != root || tag.empty {
        return body_fault();
    }
    Ok(())
}

// A value written twice is a fault. Choosing one of them would be a rule this
// crate made up.
pub(super) fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<()> {
    if slot.is_some() {
        return body_fault();
    }
    *slot = Some(value);
    Ok(())
}

// Returns a decoded value as text. The body was UTF-8 and a reference decodes
// to a character, so only a percent-decoded key can fail this.
pub(super) fn decoded_text(bytes: &[u8]) -> Result<&str> {
    core::str::from_utf8(bytes).or_else(|_| body_fault())
}

// The fields of an entry as text, after their values were decoded: the
// bytes of `raw` from the first field to the end of the last, checked as
// UTF-8 once, and where they begin. Take each field with `field_text`.
// Checked one by one, a field of a few dozen bytes is too short for
// `from_utf8` to read a word at a time, and costs a byte at a time and a
// misread branch or two. Checked whole, the entry would include every
// property as well.
pub(super) fn fields_text<'r>(raw: &'r [u8], spans: &[Option<Span>]) -> Result<(&'r str, usize)> {
    let (start, end) = spans
        .iter()
        .flatten()
        .fold((usize::MAX, 0), |(start, end), span| {
            (start.min(span.0), end.max(span.1))
        });
    if start > end {
        return Ok(("", 0));
    }
    // `from_utf8` reads two words a step only from an address that is a
    // multiple of eight, and only while sixteen bytes are left. A key of
    // sixteen bytes that begins anywhere else is read a byte at a time, at
    // nine instructions each. So the bytes around the fields are checked
    // too, out to such an address before them and to a whole number of steps
    // after. They are the tags around the fields, which are ASCII. Should the
    // wider range fail, the fields alone decide.
    let (wide_start, wide_end) = utf8_check_range(raw, start, end);
    if let Some(text) = raw
        .get(wide_start..wide_end)
        .and_then(|wide| core::str::from_utf8(wide).ok())
    {
        return Ok((text, wide_start));
    }
    let text = raw.get(start..end).map_or_else(body_fault, decoded_text)?;
    Ok((text, start))
}

// The range `fields_text` checks for `start..end` of `raw`. It begins at the
// last multiple of eight at or before `start`, and ends a whole number of
// sixteen-byte steps after that, both within `raw`.
#[inline(always)]
fn utf8_check_range(raw: &[u8], start: usize, end: usize) -> (usize, usize) {
    let misaligned = (raw.as_ptr() as usize).wrapping_add(start) % 8;
    let wide_start = start.saturating_sub(misaligned);
    let wide_end = (wide_start + (end - wide_start).next_multiple_of(16)).min(raw.len());
    (wide_start, wide_end)
}

// The text of the field `span` of an entry, from what `fields_text` gave.
pub(super) fn field_text((text, start): (&str, usize), (from, to): Span) -> Result<&str> {
    text.get(from - start..to - start)
        .map_or_else(body_fault, Ok)
}

// Trims one value, decodes it in place and returns the range of the decoded
// text. An absent value is told apart here, inline, rather than by a call: a
// group of keys has none of the values an object has.
#[inline(always)]
pub(super) fn decode_value_in_place(
    chunk: &mut [u8],
    field: Option<(Span, u8)>,
) -> Result<Option<Span>> {
    match field {
        Some((span, flags)) => decode_span_in_place(chunk, span, flags).map(Some),
        None => Ok(None),
    }
}

fn decode_span_in_place(chunk: &mut [u8], span: Span, flags: u8) -> Result<Span> {
    let (start, end) = trim_span(chunk, span);
    let len = decode_scanned(&mut chunk[start..end], flags, false)?;
    // trim preserves start <= end <= chunk.len(); decode returns len <= end - start.
    if len < end - start {
        chunk[start + len..end].fill(0);
    }
    Ok((start, start + len))
}

// The value of a child element, if the entry wrote one: its span and the
// scanner's flags for what it holds to decode.
pub(super) type ElementValue = Option<(Span, u8)>;
