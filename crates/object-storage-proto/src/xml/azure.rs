// Reads an `EnumerationResults` document, using the scanner in `scan.rs`.
//
// The read is one pass over the body. Only the entries are kept. Each entry
// is split off as its own borrow of the body. Its text is decoded in place and
// then handed to the caller. The array the caller passed must hold the whole
// page; one that does not is refused with the number of entries the page
// holds.

use super::decode::decode;
use super::scan::{Child, Scan, Span, Tag, fault, split_off, trim};
use crate::{CapacityError, EntryKind, Error, ListEntry, Listing, Result};

const ROOT: &[u8] = b"EnumerationResults";
const ENTRIES: &[u8] = b"Blobs";

pub(crate) fn fill_listing<'b, E: From<ListEntry<'b>>>(
    body: &'b mut [u8],
    into: &mut [E],
) -> Result<Listing<'b>> {
    let at = open_root_element(body)?;
    read_root_children_into(body, at, into)
}

// Reads the prolog and the root's opening tag, and returns the offset just
// after that tag, where the root's first child begins.
//
// The whole body is checked for valid UTF-8 here, once, before anything is
// read from it. A document that is not valid UTF-8 is not XML. The check runs
// at 45 to 50 GB/s and the read at 1.7 GB/s, so it costs a few percent. It
// does not replace the check each key gets after decoding, because a percent
// escape can produce any byte.
//
// Refusing the body is safe because Azure never sends invalid UTF-8. This was
// measured, not assumed. A key with an invalid byte is refused with
// `400 InvalidUri`. A query value with one comes back with `U+FFFD` in its
// place. So invalid UTF-8 here is a protocol violation, not a key the caller
// might hold. The measurement is `a_listing_body_is_always_utf_8` in the live
// suite.
fn open_root_element(body: &[u8]) -> Result<usize> {
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
    let mut scan = Scan::new(body);
    // Azure begins a listing with the UTF-8 byte order mark, U+FEFF encoded
    // as these three bytes, before the XML declaration. It is not part of the
    // document.
    if body.starts_with(&[0xEF, 0xBB, 0xBF]) {
        scan.cursor = 3;
    }
    loop {
        scan.skip_space();
        if scan.cur() != b'<' {
            return fault();
        }
        if !scan.skip_misc()? {
            break;
        }
    }
    let root = scan.open()?;
    // A service can answer a listing with an error document under a success
    // status. That is not a page.
    if scan.text(root.name) != ROOT || root.empty {
        return fault();
    }
    Ok(scan.cursor)
}

// One child of the root element or of the entries element, as the loop in
// `read_root_children_into` sees it.
enum Item {
    Entry(Fields),
    // The element that holds the entries, and whether it holds any.
    Entries { within: bool },
    // The entries element has ended.
    Leave,
    // The text that names the next page.
    Marker((Span, u8)),
    // An element this crate does not read.
    Skip,
    // The root element has ended.
    End,
}

// Reads one child from the bytes that begin with it, and returns how many
// bytes it took. The caller has already skipped the whitespace before it, so
// byte zero is the child's first byte.
fn read_next_child(bytes: &[u8], within: bool) -> Result<(Item, usize)> {
    let mut scan = Scan::new(bytes);
    if !within {
        let item = match scan.child(ROOT)? {
            Child::Close => Item::End,
            Child::Open(tag) => match scan.text(tag.name) {
                ENTRIES => Item::Entries { within: !tag.empty },
                // The marker names the next page, so it holds only text. The
                // service writes it as `<NextMarker/>`, as `<NextMarker />`,
                // or with text between two tags. All three are the same
                // element.
                b"NextMarker" => Item::Marker(scan.value(tag)?),
                _ => {
                    scan.skip(tag)?;
                    Item::Skip
                }
            },
        };
        return Ok((item, scan.cursor));
    }
    // Inside the entries element, where only entries belong. A comment, a
    // CDATA section and a processing instruction may each hold the tags that
    // entries are read by. All three are refused here rather than skipped.
    if scan.cur() != b'<' {
        return fault();
    }
    match bytes.get(1).copied().unwrap_or(0) {
        b'/' => {
            scan.close(ENTRIES)?;
            return Ok((Item::Leave, scan.cursor));
        }
        b'!' | b'?' => return fault(),
        _ => {}
    }
    // Nearly every entry of a page starts with `<Blob>`, which is one six-byte
    // compare. Anything else takes the general path below.
    if scan.lit(b"<Blob>") {
        let tag = Tag {
            name: (1, 5),
            attributes: (5, 5),
            empty: false,
        };
        return Ok((Item::Entry(read_blob(&mut scan, tag)?), scan.cursor));
    }
    let tag = scan.open()?;
    let fields = match scan.text(tag.name) {
        b"Blob" => read_blob(&mut scan, tag)?,
        b"BlobPrefix" => read_blob_prefix(&mut scan, tag)?,
        // Any other element where an entry belongs makes this a page this
        // crate cannot read. Skipping it would silently drop an entry that a
        // later service version added.
        _ => return fault(),
    };
    Ok((Item::Entry(fields), scan.cursor))
}

// The fields of one entry, as ranges into the entry's own bytes. Ranges
// rather than slices let the text be decoded later, after the entry has been
// split off the body.
struct Fields {
    prefix: bool,
    key: Option<(Span, u8)>,
    percent: bool,
    size: Option<Span>,
    e_tag: Option<(Span, u8)>,
    last_modified: Option<(Span, u8)>,
    resource_type: Option<Span>,
}

impl Fields {
    const fn new(prefix: bool) -> Self {
        Self {
            prefix,
            key: None,
            percent: false,
            size: None,
            e_tag: None,
            last_modified: None,
            resource_type: None,
        }
    }
}

// A value written twice is a fault. Choosing one of them would be a rule this
// crate made up.
fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<()> {
    if slot.is_some() {
        return fault();
    }
    *slot = Some(value);
    Ok(())
}

fn read_blob(scan: &mut Scan<'_>, tag: Tag) -> Result<Fields> {
    if tag.empty {
        return fault();
    }
    let mut fields = Fields::new(false);
    loop {
        scan.skip_space();
        if scan.lit(b"<Name>") {
            let value = scan.value_of(b"Name")?;
            set_once(&mut fields.key, value)?;
            continue;
        }
        if scan.lit(b"<Properties>") {
            read_properties(scan, &mut fields)?;
            continue;
        }
        if scan.lit(b"</Blob>") {
            break;
        }
        match scan.child(b"Blob")? {
            Child::Close => break,
            Child::Open(tag) => match scan.text(tag.name) {
                b"Name" => read_name(scan, tag, &mut fields)?,
                // Properties are read only under this element. A property
                // that a later service version adds beside it is not
                // mistaken for one of them.
                b"Properties" if !tag.empty => read_properties(scan, &mut fields)?,
                _ => scan.skip(tag)?,
            },
        }
    }
    // An entry without a name is not an object.
    if fields.key.is_none() {
        return fault();
    }
    Ok(fields)
}

// Reads the properties of a blob into `fields`. `<Properties>` has been
// consumed. Four of
// its children matter. The rest, a dozen or so per blob, are most of an Azure
// page and are skipped without being read.
fn read_properties(scan: &mut Scan<'_>, fields: &mut Fields) -> Result<()> {
    loop {
        scan.skip_space();
        // The children the service writes, matched whole. The byte after the
        // `<` picks the compare, so one child costs one compare.
        match scan.bytes.get(scan.cursor + 1).copied().unwrap_or(0) {
            b'L' if scan.lit(b"<Last-Modified>") => {
                let value = scan.value_of(b"Last-Modified")?;
                set_once(&mut fields.last_modified, value)?;
            }
            b'E' if scan.lit(b"<Etag>") => {
                let value = scan.value_of(b"Etag")?;
                set_once(&mut fields.e_tag, value)?;
            }
            b'C' if scan.lit(b"<Content-Length>") => {
                let value = scan.value_of(b"Content-Length")?;
                set_once(&mut fields.size, value.0)?;
            }
            b'R' if scan.lit(b"<ResourceType>") => {
                let value = scan.value_of(b"ResourceType")?;
                set_once(&mut fields.resource_type, value.0)?;
            }
            b'/' if scan.lit(b"</Properties>") => return Ok(()),
            _ => match scan.child(b"Properties")? {
                Child::Close => return Ok(()),
                Child::Open(tag) => match scan.text(tag.name) {
                    b"Last-Modified" => {
                        let value = scan.value(tag)?;
                        set_once(&mut fields.last_modified, value)?;
                    }
                    b"Etag" => {
                        let value = scan.value(tag)?;
                        set_once(&mut fields.e_tag, value)?;
                    }
                    b"Content-Length" => {
                        let value = scan.value(tag)?;
                        set_once(&mut fields.size, value.0)?;
                    }
                    b"ResourceType" => {
                        let value = scan.value(tag)?;
                        set_once(&mut fields.resource_type, value.0)?;
                    }
                    _ => scan.skip(tag)?,
                },
            },
        }
    }
}

// Reads a group of keys. A hierarchical-namespace account writes a
// `<Properties>` element here too. It is skipped, because a group has no size
// and no entity tag.
fn read_blob_prefix(scan: &mut Scan<'_>, tag: Tag) -> Result<Fields> {
    if tag.empty {
        return fault();
    }
    let mut fields = Fields::new(true);
    loop {
        match scan.child(b"BlobPrefix")? {
            Child::Close => break,
            Child::Open(tag) => match scan.text(tag.name) {
                b"Name" => read_name(scan, tag, &mut fields)?,
                _ => scan.skip(tag)?,
            },
        }
    }
    if fields.key.is_none() {
        return fault();
    }
    Ok(fields)
}

// Reads a `<Name>` element into `fields`, with its `Encoded` attribute, the
// one attribute this crate reads. Azure writes it at most once, with a
// boolean value.
fn read_name(scan: &mut Scan<'_>, tag: Tag, fields: &mut Fields) -> Result<()> {
    let mut encoded = None;
    for attribute in scan.attributes(tag) {
        let (attribute, value) = attribute?;
        if attribute == b"Encoded" {
            let value = match value {
                b"true" => true,
                b"false" => false,
                _ => return fault(),
            };
            set_once(&mut encoded, value)?;
        }
    }
    fields.percent = encoded.unwrap_or(false);
    let value = scan.value(tag)?;
    set_once(&mut fields.key, value)
}

// Builds one entry from the bytes it was written in, decoding its key, entity
// tag and date in place.
fn build_entry(chunk: &mut [u8], fields: Fields) -> Result<ListEntry<'_>> {
    let Some((key, key_flags)) = fields.key else {
        return fault();
    };
    // Azure never writes an empty name. Refused for consistency with the rest
    // of the grammar, not for a case that was seen.
    if key.0 == key.1 {
        return fault();
    }
    // The resource type is a fixed word the service writes, so it needs no
    // decoding and is read first.
    let directory = match fields.resource_type {
        Some(span) => {
            let (start, end) = trim(chunk, span);
            &chunk[start..end] == b"directory"
        }
        None => false,
    };
    let size = match fields.size {
        Some(span) => {
            let (start, end) = trim(chunk, span);
            match crate::azure::decimal(&chunk[start..end]) {
                Some(size) => Some(size),
                None => return fault(),
            }
        }
        // An object always has a length. A group of keys and a directory are
        // not objects and have none.
        None if !fields.prefix && !directory => return fault(),
        None => None,
    };
    let kind = match (fields.prefix, directory) {
        (true, _) => EntryKind::Prefix,
        (false, true) => EntryKind::Directory,
        (false, false) => EntryKind::Object,
    };

    // A key may begin or end with a space, so the key is not trimmed. Only
    // the values the service writes for itself are.
    let key_len = decode(&mut chunk[key.0..key.1], key_flags, fields.percent)?;
    if key_len < key.1 - key.0 {
        // The bytes the decoding no longer needs are set to zero, so that the
        // walk over the entry can tell where the decoded text ends. A decoded
        // key can hold `<` and `>`, so the walk cannot find that by looking
        // for the close tag. See `next_property`. The scanner refused a zero
        // byte in the document, so only this writes one.
        chunk[key.0 + key_len..key.1].fill(0);
        // A percent escape can name a zero byte, which would look like that
        // filler. XML forbids the character and Azure refuses it in a name,
        // so this is refused for consistency, not for a case seen.
        if fields.percent && chunk[key.0..key.0 + key_len].contains(&0) {
            return fault();
        }
    }
    let e_tag = decode_value_in_place(chunk, fields.e_tag)?;
    let last_modified = decode_value_in_place(chunk, fields.last_modified)?;

    let raw: &[u8] = chunk;
    Ok(ListEntry {
        kind,
        key: text(&raw[key.0..key.0 + key_len])?,
        size: if directory { None } else { size },
        e_tag: e_tag
            .filter(|_| !directory)
            .map(|(start, end)| text(&raw[start..end]))
            .transpose()?,
        last_modified: last_modified
            .map(|(start, end)| text(&raw[start..end]))
            .transpose()?,
        raw,
    })
}

// Returns a decoded value as text. The body was UTF-8 and a reference decodes
// to a character, so only a percent-decoded key can fail this.
fn text(bytes: &[u8]) -> Result<&str> {
    core::str::from_utf8(bytes).or_else(|_| fault())
}

// Trims one value, decodes it in place and returns the range of the decoded
// text.
fn decode_value_in_place(chunk: &mut [u8], field: Option<(Span, u8)>) -> Result<Option<Span>> {
    let Some((span, flags)) = field else {
        return Ok(None);
    };
    let (start, end) = trim(chunk, span);
    let len = decode(&mut chunk[start..end], flags, false)?;
    if len < end - start {
        chunk[start + len..end].fill(0);
    }
    Ok(Some((start, start + len)))
}

// Reads the children of the root element from `at` on, writing each entry
// into `into` until the root element ends. An entry the array has no room for
// is walked and counted but not built, so that the error can say how many
// entries the page holds.
fn read_root_children_into<'b, E: From<ListEntry<'b>>>(
    body: &'b mut [u8],
    at: usize,
    into: &mut [E],
) -> Result<Listing<'b>> {
    let mut rest = body;
    split_off(&mut rest, at);
    let mut within = false;
    let mut seen_entries = false;
    let mut seen_marker = false;
    let mut next_marker: Option<&'b str> = None;
    let mut filled = 0;
    let mut count = 0;

    loop {
        let mut space = Scan::new(rest);
        space.skip_space();
        let space = space.cursor;
        split_off(&mut rest, space);

        let (item, end) = read_next_child(rest, within)?;
        match item {
            Item::Entry(fields) => {
                let chunk = split_off(&mut rest, end);
                count += 1;
                if filled < into.len() {
                    into[filled] = build_entry(chunk, fields)?.into();
                    filled += 1;
                }
            }
            Item::Marker((span, flags)) => {
                // A second marker is refused the way a second name is. Azure
                // writes one; this is for consistency, not for a case seen.
                if seen_marker {
                    return fault();
                }
                seen_marker = true;
                let chunk = split_off(&mut rest, end);
                let (start, stop) = trim(chunk, span);
                let len = decode(&mut chunk[start..stop], flags, false)?;
                let chunk: &'b [u8] = chunk;
                // An empty marker means the listing is complete, so it is
                // reported as no next page rather than an empty one.
                next_marker = Some(&chunk[start..start + len])
                    .filter(|marker| !marker.is_empty())
                    .map(text)
                    .transpose()?;
            }
            Item::Entries { within: held } => {
                // The same for a second entries element.
                if seen_entries {
                    return fault();
                }
                within = held;
                seen_entries = true;
                split_off(&mut rest, end);
            }
            Item::Leave => {
                within = false;
                split_off(&mut rest, end);
            }
            Item::Skip => {
                split_off(&mut rest, end);
            }
            Item::End => {
                // A document with no entries element is not a page.
                if !seen_entries {
                    return fault();
                }
                if count > into.len() {
                    return Err(Error::Capacity(CapacityError {
                        required: count,
                        available: into.len(),
                    }));
                }
                return Ok(Listing {
                    filled,
                    next_marker,
                });
            }
        }
    }
}
