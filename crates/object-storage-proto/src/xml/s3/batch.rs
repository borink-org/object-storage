// Reads a `DeleteResult`, the answer to an S3 DeleteObjects: a `Deleted` for
// each object that S3 removed, and an `Error` for each that it did not. Each
// is taken off the body whole once it is read, and its fields are decoded in
// place, as `parts.rs` reads the parts of an upload.

use crate::Result;
use crate::s3::DeleteResult;
use crate::xml::page::{
    check_body, check_room, decode_value_in_place, open_root_element, set_once, text,
};
use crate::xml::scan::{Child, Scan, Span, fault};

const ROOT: &[u8] = b"DeleteResult";

pub(crate) fn fill_delete_results<'b>(
    body: &'b mut [u8],
    into: &mut [DeleteResult<'b>],
) -> Result<usize> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    open_root_element(&mut scan, ROOT)?;
    let mut held = 0;
    loop {
        // Drop what was read before this child, so that the spans recorded
        // while reading it index the bytes that the next `take` returns.
        scan.take();
        let tag = match scan.child(ROOT)? {
            Child::Close => break,
            Child::Open(tag) => tag,
        };
        let deleted = match scan.text(tag.name) {
            b"Deleted" => true,
            b"Error" => false,
            _ => {
                scan.skip(tag)?;
                continue;
            }
        };
        if tag.empty {
            return fault();
        }
        let parent: &[u8] = if deleted { b"Deleted" } else { b"Error" };
        let (key, code) = read_result(&mut scan, parent)?;
        let chunk = scan.take();
        let result = build_result(chunk, key, code, deleted)?;
        if let Some(slot) = into.get_mut(held) {
            *slot = result;
        }
        held += 1;
    }
    check_room(held, into.len())?;
    Ok(held)
}

type Field = Option<(Span, u8)>;

// Reads a result. Its opening tag has been consumed.
fn read_result(scan: &mut Scan<'_>, parent: &[u8]) -> Result<(Field, Field)> {
    let (mut key, mut code) = (None, None);
    loop {
        match scan.child(parent)? {
            Child::Close => break,
            Child::Open(tag) => match scan.text(tag.name) {
                b"Key" => set_once(&mut key, scan.value(tag)?)?,
                b"Code" => set_once(&mut code, scan.value(tag)?)?,
                _ => scan.skip(tag)?,
            },
        }
    }
    Ok((key, code))
}

// Builds one result. Each names its key, and an error names its code.
fn build_result(
    chunk: &mut [u8],
    key: Field,
    code: Field,
    deleted: bool,
) -> Result<DeleteResult<'_>> {
    if key.is_none() || deleted == code.is_some() {
        return fault();
    }
    let key = decode_value_in_place(chunk, key)?;
    let code = decode_value_in_place(chunk, code)?;
    let chunk: &[u8] = chunk;
    let Some(key) = key else {
        return fault();
    };
    Ok(DeleteResult {
        key: text(&chunk[key.0..key.1])?,
        code: code
            .map(|(start, end)| text(&chunk[start..end]))
            .transpose()?,
    })
}
