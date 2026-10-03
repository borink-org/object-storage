// Reads the tags of an object: an Azure `Tags` or an S3 `Tagging` document,
// whose `TagSet` holds one `Tag` with a `Key` and a `Value` for each tag.
// Each `Tag` is taken off the body whole once it is read, and its two fields
// are decoded in place, as `s3/parts.rs` reads the parts of an upload.

use super::page::{
    check_body, check_room, decode_value_in_place, open_root_element, set_once, text,
};
use super::scan::{Child, Scan, Span, fault};
use crate::{Result, Tag};

const SET: &[u8] = b"TagSet";

pub(crate) fn fill_tags<'b>(
    body: &'b mut [u8],
    root: &[u8],
    into: &mut [Tag<'b>],
) -> Result<usize> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    open_root_element(&mut scan, root)?;
    let mut held = 0;
    let mut seen_set = false;
    loop {
        scan.take();
        let tag = match scan.child(root)? {
            Child::Close => break,
            Child::Open(tag) => tag,
        };
        if scan.text(tag.name) != SET {
            scan.skip(tag)?;
            continue;
        }
        if seen_set {
            return fault();
        }
        seen_set = true;
        if tag.empty {
            continue;
        }
        loop {
            // Drop what was read before this tag, so that the spans recorded
            // while reading it index the bytes that the next `take` returns.
            scan.take();
            match scan.child(SET)? {
                Child::Close => break,
                Child::Open(tag) if scan.text(tag.name) == b"Tag" && !tag.empty => {
                    let (key, value) = read_tag(&mut scan)?;
                    let chunk = scan.take();
                    let tag = build_tag(chunk, key, value)?;
                    if let Some(slot) = into.get_mut(held) {
                        *slot = tag;
                    }
                    held += 1;
                }
                Child::Open(_) => return fault(),
            }
        }
    }
    check_room(held, into.len())?;
    Ok(held)
}

type Field = Option<(Span, u8)>;

// Reads a tag. `<Tag>` has been consumed.
fn read_tag(scan: &mut Scan<'_>) -> Result<(Field, Field)> {
    let (mut key, mut value) = (None, None);
    loop {
        match scan.child(b"Tag")? {
            Child::Close => break,
            Child::Open(tag) => match scan.text(tag.name) {
                b"Key" => set_once(&mut key, scan.value(tag)?)?,
                b"Value" => set_once(&mut value, scan.value(tag)?)?,
                _ => scan.skip(tag)?,
            },
        }
    }
    Ok((key, value))
}

// Builds one tag from its bytes. A tag always has a key and a value, which
// may be empty.
fn build_tag(chunk: &mut [u8], key: Field, value: Field) -> Result<Tag<'_>> {
    if key.is_none() || value.is_none() {
        return fault();
    }
    let key = decode_value_in_place(chunk, key)?;
    let value = decode_value_in_place(chunk, value)?;
    let chunk: &[u8] = chunk;
    let (Some(key), Some(value)) = (key, value) else {
        return fault();
    };
    Ok(Tag {
        key: text(&chunk[key.0..key.1])?,
        value: text(&chunk[value.0..value.1])?,
    })
}
