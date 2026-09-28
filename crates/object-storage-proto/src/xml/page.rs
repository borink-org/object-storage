// What both page readers share: checking the body, reading the root element,
// and decoding a value in an entry taken off the body.

use super::decode::decode;
use super::scan::{Scan, Span, fault, trim};
use crate::Result;

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
