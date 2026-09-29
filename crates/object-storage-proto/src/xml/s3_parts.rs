// Reads the answers of an S3 upload in parts: the upload ID that
// CreateMultipartUpload returns, the entity tag of a committed object, and a
// page of ListParts. `azure_blocks.rs` reads Azure's block list the same way.

use super::page::{
    check_body, check_room, decode_value_in_place, open_root_element, read_size, set_once, text,
};
use super::scan::{Child, Scan, Span, fault};
use crate::s3::Part;
use crate::{Listing, Result};

// Reads an `InitiateMultipartUploadResult`, the answer to a
// CreateMultipartUpload, and returns the ID of the upload.
pub(crate) fn read_upload_id(body: &mut [u8]) -> Result<&str> {
    let [id] = read_values(body, b"InitiateMultipartUploadResult", [b"UploadId"])?;
    // S3 hands out no empty ID, and a request cannot name one.
    id.filter(|id| !id.is_empty()).map_or_else(fault, Ok)
}

// Reads a `CompleteMultipartUploadResult`, the answer to a commit that
// succeeded, and returns the entity tag of the object.
pub(crate) fn read_committed(body: &mut [u8]) -> Result<&str> {
    let [e_tag] = read_values(body, b"CompleteMultipartUploadResult", [b"ETag"])?;
    e_tag.map_or_else(fault, Ok)
}

// Reads the values of the children of the root `root` that `names` names,
// each decoded in place, and skips every other child. A child named twice is
// a fault. As in `read_session`, nothing is taken off the body until the root
// is closed, so every span indexes the whole document.
fn read_values<'b, const N: usize>(
    body: &'b mut [u8],
    root: &[u8],
    names: [&[u8]; N],
) -> Result<[Option<&'b str>; N]> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    open_root_element(&mut scan, root)?;
    let mut fields: [Option<(Span, u8)>; N] = [None; N];
    loop {
        match scan.child(root)? {
            Child::Close => break,
            Child::Open(tag) => match names.iter().position(|name| *name == scan.text(tag.name)) {
                Some(slot) => set_once(&mut fields[slot], scan.value(tag)?)?,
                None => scan.skip(tag)?,
            },
        }
    }
    let chunk = scan.take();
    let mut spans = [None; N];
    for (span, field) in spans.iter_mut().zip(fields) {
        *span = decode_value_in_place(chunk, field)?;
    }
    let chunk: &'b [u8] = chunk;
    let mut values = [None; N];
    for (value, span) in values.iter_mut().zip(spans) {
        *value = span
            .map(|(start, end)| text(&chunk[start..end]))
            .transpose()?;
    }
    Ok(values)
}

const PARTS_ROOT: &[u8] = b"ListPartsResult";

// Reads a `ListPartsResult`, one page of the parts of an upload, into a
// caller's array, the way `fill_listing` reads a page of objects. Each `Part`
// child is taken off the body whole once it is read, and its fields are
// decoded in place.
pub(crate) fn fill_parts<'b, E: From<Part<'b>>>(
    body: &'b mut [u8],
    into: &mut [E],
) -> Result<Listing<'b>> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    open_root_element(&mut scan, PARTS_ROOT)?;
    let mut held = 0;
    let mut truncated = None;
    let mut marker = None;
    loop {
        // Drop what was read before this child, so that the spans recorded
        // while reading it index the bytes that the next `take` returns.
        scan.take();
        let tag = match scan.child(PARTS_ROOT)? {
            Child::Close => break,
            Child::Open(tag) => tag,
        };
        match scan.text(tag.name) {
            // A part with nothing in it has no number.
            b"Part" if tag.empty => return fault(),
            b"Part" => {
                let fields = read_part(&mut scan)?;
                let chunk = scan.take();
                if let Some(slot) = into.get_mut(held) {
                    *slot = build_part(chunk, fields)?.into();
                }
                held += 1;
            }
            b"IsTruncated" => {
                let (span, _) = scan.value(tag)?;
                let value = match scan.text(span).trim_ascii() {
                    b"true" => true,
                    b"false" => false,
                    _ => return fault(),
                };
                set_once(&mut truncated, value)?;
            }
            b"NextPartNumberMarker" => {
                let field = scan.value(tag)?;
                let chunk = scan.take();
                let decoded = decode_value_in_place(chunk, Some(field))?;
                let chunk: &'b [u8] = chunk;
                let value = decoded.map(|(start, end)| &chunk[start..end]);
                set_once(&mut marker, value.map(text).transpose()?)?;
            }
            _ => scan.skip(tag)?,
        }
    }
    check_room(held, into.len())?;
    // AWS writes the marker on every page, so only `IsTruncated` says whether
    // another page follows. A page that does not say cannot be continued
    // safely.
    let next_marker = match truncated {
        Some(true) => match marker.flatten() {
            Some(marker) if !marker.is_empty() => Some(marker),
            _ => return fault(),
        },
        Some(false) => None,
        None => return fault(),
    };
    Ok(Listing {
        filled: held,
        next_marker,
    })
}

// The fields of one part, as ranges into the part's own bytes.
#[derive(Default)]
struct PartFields {
    number: Option<Span>,
    e_tag: Option<(Span, u8)>,
    size: Option<Span>,
    last_modified: Option<(Span, u8)>,
}

// Reads a part. `<Part>` has been consumed. The checksums that S3 writes
// beside the fields are skipped.
fn read_part(scan: &mut Scan<'_>) -> Result<PartFields> {
    let mut fields = PartFields::default();
    loop {
        match scan.child(b"Part")? {
            Child::Close => break,
            Child::Open(tag) => match scan.text(tag.name) {
                b"PartNumber" => set_once(&mut fields.number, scan.value(tag)?.0)?,
                b"ETag" => set_once(&mut fields.e_tag, scan.value(tag)?)?,
                b"Size" => set_once(&mut fields.size, scan.value(tag)?.0)?,
                b"LastModified" => set_once(&mut fields.last_modified, scan.value(tag)?)?,
                _ => scan.skip(tag)?,
            },
        }
    }
    Ok(fields)
}

// Builds one part from its bytes, decoding its entity tag and date in place.
// A part always has a number, an entity tag and a length.
fn build_part(chunk: &mut [u8], fields: PartFields) -> Result<Part<'_>> {
    let (Some(number), Some(size), Some(_)) = (fields.number, fields.size, fields.e_tag) else {
        return fault();
    };
    let number = u32::try_from(read_size(chunk, number)?).or_else(|_| fault())?;
    let size = read_size(chunk, size)?;
    let e_tag = decode_value_in_place(chunk, fields.e_tag)?;
    let last_modified = decode_value_in_place(chunk, fields.last_modified)?;
    let chunk: &[u8] = chunk;
    let Some((start, end)) = e_tag else {
        return fault();
    };
    Ok(Part {
        number,
        e_tag: text(&chunk[start..end])?,
        size,
        last_modified: last_modified
            .map(|(start, end)| text(&chunk[start..end]))
            .transpose()?,
    })
}
