// What both page readers share: checking the body, reading the root element,
// keeping the values of wanted properties, and building an entry taken off
// the body.

use super::decode::decode;
use super::scan::{Scan, Span, Tag, fault, trim};
use crate::{CapacityError, Error, Result};

// A property whose value a page reader can keep: `BlobProperty` or
// `s3::ObjectProperty`.
pub(super) trait ListProperty: Copy {
    fn name(self) -> &'static str;

    // Whether the element holds other elements rather than one text.
    fn holds_elements(self) -> bool;

    // The property that an element name stands for, if any.
    fn identify(name: &[u8]) -> Option<Self>;
}

// The properties that one read of a page keeps the values of: `PropertySet`
// or `s3::PropertySet`. It is a projection, like Parquet's `ProjectionMask`:
// the reader walks every element and keeps only these.
pub(super) trait ListPropertySet: Copy {
    type Property: ListProperty;

    fn contains(self, property: Self::Property) -> bool;

    fn is_empty(self) -> bool;

    // The property's rank among the set's members, which is its slot.
    fn slot(self, property: Self::Property) -> usize;
}

// Keeps the span of a property's value if `wanted` holds the property. A
// property written twice keeps the first.
#[inline(always)]
pub(super) fn capture<S: ListPropertySet>(
    property: S::Property,
    span: Span,
    wanted: S,
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
pub(super) fn read_known<S: ListPropertySet>(
    scan: &mut Scan<'_>,
    property: S::Property,
    wanted: S,
    captured: &mut [Option<Span>],
) -> Result<()> {
    let name = property.name().as_bytes();
    let span = if property.holds_elements() {
        scan.nested(name)?
    } else {
        scan.value_of(name)?.0
    };
    capture(property, span, wanted, captured);
    Ok(())
}

// Reads past an element that no whole-tag match took: one this crate does
// not know, or a known one in another spelling, such as `<AccessTier >`.
// Keeps the value of a wanted one.
pub(super) fn read_other<S: ListPropertySet>(
    scan: &mut Scan<'_>,
    tag: Tag,
    wanted: S,
    captured: &mut [Option<Span>],
) -> Result<()> {
    if wanted.is_empty() {
        return scan.skip(tag);
    }
    match S::Property::identify(scan.text(tag.name)) {
        Some(property) if wanted.contains(property) => {
            let span = if property.holds_elements() && !tag.empty {
                scan.nested(property.name().as_bytes())?
            } else {
                scan.value(tag)?.0
            };
            capture(property, span, wanted, captured);
            Ok(())
        }
        _ => scan.skip(tag),
    }
}

// Writes into `values` the part of an entry's bytes that each captured span
// names. The spans were recorded on those bytes.
pub(super) fn values_of<'b>(
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
        return Err(Error::Capacity(CapacityError {
            required: held,
            ..CapacityError::default()
        }));
    }
    Ok(())
}

// Reads the size of an object.
#[inline]
pub(super) fn read_size(chunk: &[u8], span: Span) -> Result<u64> {
    let (start, end) = trim(chunk, span);
    match crate::common::decimal(&chunk[start..end]) {
        Some(size) => Ok(size),
        None => fault(),
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
            return fault();
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
    if core::str::from_utf8(body).is_err() {
        return fault();
    }
    // XML forbids a zero byte in a document, and the reader writes zero over
    // the bytes a decoded value no longer needs, so that the walk over an
    // entry can find where the decoded text ends. A document that held a zero
    // byte of its own would defeat that. Written as a minimum so that it
    // compiles to one vector instruction per sixteen bytes; a search that
    // stops at the first hit does not, and costs ten times as much.
    if body.iter().fold(u8::MAX, |lowest, &byte| lowest.min(byte)) == 0 {
        return fault();
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
            return fault();
        }
        if !scan.skip_misc()? {
            break;
        }
    }
    let tag = scan.open()?;
    // A service can answer a listing with an error document under a success
    // status. That is not a page.
    if scan.text(tag.name) != root || tag.empty {
        return fault();
    }
    Ok(())
}

// A value written twice is a fault. Choosing one of them would be a rule this
// crate made up.
pub(super) fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<()> {
    if slot.is_some() {
        return fault();
    }
    *slot = Some(value);
    Ok(())
}

// Returns a decoded value as text. The body was UTF-8 and a reference decodes
// to a character, so only a percent-decoded key can fail this.
pub(super) fn text(bytes: &[u8]) -> Result<&str> {
    core::str::from_utf8(bytes).or_else(|_| fault())
}

// Trims one value, decodes it in place and returns the range of the decoded
// text.
pub(super) fn decode_value_in_place(
    chunk: &mut [u8],
    field: Option<(Span, u8)>,
) -> Result<Option<Span>> {
    let Some((span, flags)) = field else {
        return Ok(None);
    };
    let (start, end) = trim(chunk, span);
    let len = decode(&mut chunk[start..end], flags, false)?;
    // trim preserves start <= end <= chunk.len(); decode returns len <= end - start.
    if len < end - start {
        chunk[start + len..end].fill(0);
    }
    Ok(Some((start, start + len)))
}
