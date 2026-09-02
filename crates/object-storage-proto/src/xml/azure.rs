// Reads an `EnumerationResults` document, using the scanner in `scan.rs`.
//
// The read is one pass over the body. Only the entries are kept. Each entry
// is split off as its own borrow of the body. Its text is decoded in place and
// then handed to the caller. The array the caller passed sets the limit. An entry that does not fit is not read at all, so its bytes are
// untouched and the next call reads from there.

use super::decode::decode;
use super::scan::{Child, Scan, Span, Tag, fault, split_off, trim};
use crate::{EntryKind, Fill, ListEntry, Listing, Result, Resume};

const ROOT: &[u8] = b"EnumerationResults";
const ENTRIES: &[u8] = b"Blobs";

pub(crate) fn fill_listing<'b>(body: &'b mut [u8], into: &mut [ListEntry<'b>]) -> Result<Fill<'b>> {
    // review: i'm not very happy with the names like `prelude` and `drive`, they are much too short and don't describe exactly what they are doing. Also I think some functions are maybe a bit too short
    // if they are not reused a lot I'd rather inline them in many cases; they should be created only when there is a very clear "phase" to them, or when otherwise the using function explodes, for clear reuse
    // // review: and then only if there is a good descriptive name
    let at = prelude(body)?;
    let start = Resume {
        at,
        within: false,
        marker: None,
    };
    drive(body, start, into, true)
}

pub(crate) fn resume_listing<'b>(
    body: &'b mut [u8],
    resume: Resume,
    into: &mut [ListEntry<'b>],
) -> Result<Fill<'b>> {
    drive(body, resume, into, false)
}

// Reads up to the first child of the root element and returns that offset.
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
fn prelude(body: &[u8]) -> Result<usize> {
    if core::str::from_utf8(body).is_err() {
        return fault();
    }
    let mut sc = Scan::new(body);
    if body.starts_with(&[0xEF, 0xBB, 0xBF]) {
        sc.i = 3;
    }
    loop {
        sc.skip_space();
        if sc.cur() != b'<' {
            return fault();
        }
        if !sc.skip_misc()? {
            break;
        }
    }
    let root = sc.open()?;
    // A service can answer a listing with an error document under a success
    // status. That is not a page.
    if sc.text(root.name) != ROOT || root.empty {
        return fault();
    }
    Ok(sc.i)
}

// One child of the element being read, as the driver sees it.
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

// Reads one item from the bytes that begin with it. The driver has already
// skipped the whitespace before it, so byte zero is the item's first byte.
fn item(b: &[u8], within: bool) -> Result<(Item, usize)> {
    let mut sc = Scan::new(b);
    if !within {
        let item = match sc.child(ROOT)? {
            Child::Close => Item::End,
            Child::Open(tag) => match sc.text(tag.name) {
                ENTRIES => Item::Entries { within: !tag.empty },
                // The marker names the next page, so it holds only text. The
                // service writes it as `<NextMarker/>`, as `<NextMarker />`,
                // or with text between two tags. All three are the same
                // element.
                b"NextMarker" => Item::Marker(sc.value(tag)?),
                _ => {
                    sc.skip(tag)?;
                    Item::Skip
                }
            },
        };
        return Ok((item, sc.i));
    }
    // Inside the entries element, where only entries belong. A comment, a
    // CDATA section and a processing instruction may each hold the tags that
    // entries are read by. All three are refused here rather than skipped.
    if sc.cur() != b'<' {
        return fault();
    }
    match b.get(1).copied().unwrap_or(0) {
        b'/' => {
            sc.close(ENTRIES)?;
            return Ok((Item::Leave, sc.i));
        }
        b'!' | b'?' => return fault(),
        _ => {}
    }
    // Nearly every entry of a page starts with `<Blob>`, which is one six-byte
    // compare. Anything else takes the general path below.
    if sc.lit(b"<Blob>") {
        let tag = Tag {
            name: (1, 5),
            attrs: (5, 5),
            empty: false,
        };
        return Ok((Item::Entry(blob(&mut sc, tag)?), sc.i));
    }
    let tag = sc.open()?;
    let fields = match sc.text(tag.name) {
        b"Blob" => blob(&mut sc, tag)?,
        b"BlobPrefix" => prefix(&mut sc, tag)?,
        // Any other element where an entry belongs makes this a page this
        // crate cannot read. Skipping it would silently drop an entry that a
        // later service version added.
        _ => return fault(),
    };
    Ok((Item::Entry(fields), sc.i))
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
fn set<T>(slot: &mut Option<T>, value: T) -> Result<()> {
    if slot.is_some() {
        return fault();
    }
    *slot = Some(value);
    Ok(())
}

fn blob(sc: &mut Scan<'_>, tag: Tag) -> Result<Fields> {
    if tag.empty {
        return fault();
    }
    let mut fields = Fields::new(false);
    loop {
        sc.skip_space();
        if sc.lit(b"<Name>") {
            let value = sc.value_of(b"Name")?;
            set(&mut fields.key, value)?;
            continue;
        }
        if sc.lit(b"<Properties>") {
            properties(sc, &mut fields)?;
            continue;
        }
        if sc.lit(b"</Blob>") {
            break;
        }
        match sc.child(b"Blob")? {
            Child::Close => break,
            Child::Open(tag) => match sc.text(tag.name) {
                b"Name" => name(sc, tag, &mut fields)?,
                // Properties are read only under this element. A property
                // that a later service version adds beside it is not
                // mistaken for one of them.
                b"Properties" if !tag.empty => properties(sc, &mut fields)?,
                _ => sc.skip(tag)?,
            },
        }
    }
    // An entry without a name is not an object.
    if fields.key.is_none() {
        return fault();
    }
    Ok(fields)
}

// Reads the properties of a blob. `<Properties>` has been consumed. Four of
// its children matter. The rest, a dozen or so per blob, are most of an Azure
// page and are skipped without being read.
fn properties(sc: &mut Scan<'_>, fields: &mut Fields) -> Result<()> {
    loop {
        sc.skip_space();
        // The children the service writes, matched whole. The byte after the
        // `<` picks the compare, so one child costs one compare.
        match sc.b.get(sc.i + 1).copied().unwrap_or(0) {
            b'L' if sc.lit(b"<Last-Modified>") => {
                let value = sc.value_of(b"Last-Modified")?;
                set(&mut fields.last_modified, value)?;
            }
            b'E' if sc.lit(b"<Etag>") => {
                let value = sc.value_of(b"Etag")?;
                set(&mut fields.e_tag, value)?;
            }
            b'C' if sc.lit(b"<Content-Length>") => {
                let value = sc.value_of(b"Content-Length")?;
                set(&mut fields.size, value.0)?;
            }
            b'R' if sc.lit(b"<ResourceType>") => {
                let value = sc.value_of(b"ResourceType")?;
                set(&mut fields.resource_type, value.0)?;
            }
            b'/' if sc.lit(b"</Properties>") => return Ok(()),
            _ => match sc.child(b"Properties")? {
                Child::Close => return Ok(()),
                Child::Open(tag) => match sc.text(tag.name) {
                    b"Last-Modified" => {
                        let value = sc.value(tag)?;
                        set(&mut fields.last_modified, value)?;
                    }
                    b"Etag" => {
                        let value = sc.value(tag)?;
                        set(&mut fields.e_tag, value)?;
                    }
                    b"Content-Length" => {
                        let value = sc.value(tag)?;
                        set(&mut fields.size, value.0)?;
                    }
                    b"ResourceType" => {
                        let value = sc.value(tag)?;
                        set(&mut fields.resource_type, value.0)?;
                    }
                    _ => sc.skip(tag)?,
                },
            },
        }
    }
}

// Reads a group of keys. A hierarchical-namespace account writes a
// `<Properties>` element here too. It is skipped, because a group has no size
// and no entity tag.
fn prefix(sc: &mut Scan<'_>, tag: Tag) -> Result<Fields> {
    if tag.empty {
        return fault();
    }
    let mut fields = Fields::new(true);
    loop {
        match sc.child(b"BlobPrefix")? {
            Child::Close => break,
            Child::Open(tag) => match sc.text(tag.name) {
                b"Name" => name(sc, tag, &mut fields)?,
                _ => sc.skip(tag)?,
            },
        }
    }
    if fields.key.is_none() {
        return fault();
    }
    Ok(fields)
}

// Reads a name and its `Encoded` attribute, the one attribute this crate
// reads. Azure writes it at most once, with a boolean value.
fn name(sc: &mut Scan<'_>, tag: Tag, fields: &mut Fields) -> Result<()> {
    let mut encoded = None;
    for attribute in sc.attributes(tag) {
        let (attribute, value) = attribute?;
        if attribute == b"Encoded" {
            let value = match value {
                b"true" => true,
                b"false" => false,
                _ => return fault(),
            };
            set(&mut encoded, value)?;
        }
    }
    fields.percent = encoded.unwrap_or(false);
    let value = sc.value(tag)?;
    set(&mut fields.key, value)
}

// Builds one entry from the bytes it was written in.
fn materialize(chunk: &mut [u8], fields: Fields) -> Result<ListEntry<'_>> {
    let Some((key, key_flags)) = fields.key else {
        return fault();
    };
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
    let e_tag = lend(chunk, fields.e_tag)?;
    let last_modified = lend(chunk, fields.last_modified)?;

    let raw: &[u8] = chunk;
    let key = match core::str::from_utf8(&raw[key.0..key.0 + key_len]) {
        Ok(key) => key,
        Err(_) => return fault(),
    };
    Ok(ListEntry {
        kind,
        key,
        size: if directory { None } else { size },
        e_tag: e_tag
            .filter(|_| !directory)
            .map(|(start, end)| &raw[start..end]),
        last_modified: last_modified.map(|(start, end)| &raw[start..end]),
        raw,
    })
}

// Decodes one value in place and returns the range of the decoded text.
fn lend(chunk: &mut [u8], field: Option<(Span, u8)>) -> Result<Option<Span>> {
    let Some((span, flags)) = field else {
        return Ok(None);
    };
    let (start, end) = trim(chunk, span);
    let len = decode(&mut chunk[start..end], flags, false)?;
    Ok(Some((start, start + len)))
}

fn drive<'b>(
    body: &'b mut [u8],
    mut position: Resume,
    into: &mut [ListEntry<'b>],
    fresh: bool,
) -> Result<Fill<'b>> {
    let past_the_marker = position
        .marker
        .is_some_and(|(start, end)| start > end || end > position.at);
    if position.at > body.len() || past_the_marker {
        return fault();
    }
    // The marker comes after the entries, so the bytes still to read and the
    // marker never overlap.
    let (before, mut rest) = body.split_at_mut(position.at);
    let before: &'b [u8] = before;
    let mut next_marker: Option<&'b [u8]> = position
        .marker
        .map(|(start, end)| &before[start..end])
        .filter(|marker| !marker.is_empty());
    let mut consumed = position.at;
    let mut filled = 0;
    // A document with no entries element is not a page. A resumed read starts
    // past that element and cannot see it, so only a fresh read checks this.
    let mut entries = !fresh;

    loop {
        let mut space = Scan::new(rest);
        space.skip_space();
        let space = space.i;
        split_off(&mut rest, space);
        consumed += space;

        let (item, end) = item(rest, position.within)?;
        match item {
            Item::Entry(fields) => {
                // The array sets the limit. An entry that does not fit is not
                // read at all, so the next call reads it from here.
                if filled == into.len() {
                    return Ok(Fill::Partial {
                        filled,
                        resume: Resume {
                            at: consumed,
                            ..position
                        },
                    });
                }
                let chunk = split_off(&mut rest, end);
                into[filled] = materialize(chunk, fields)?;
                filled += 1;
            }
            Item::Marker((span, flags)) => {
                let chunk = split_off(&mut rest, end);
                let (start, stop) = trim(chunk, span);
                let len = decode(&mut chunk[start..stop], flags, false)?;
                let chunk: &'b [u8] = chunk;
                position.marker = Some((consumed + start, consumed + start + len));
                // An empty marker means the listing is complete, so it is
                // reported as no next page rather than an empty one.
                next_marker = Some(&chunk[start..start + len]).filter(|m| !m.is_empty());
            }
            Item::Entries { within } => {
                position.within = within;
                entries = true;
                split_off(&mut rest, end);
            }
            Item::Leave => {
                position.within = false;
                split_off(&mut rest, end);
            }
            Item::Skip => {
                split_off(&mut rest, end);
            }
            Item::End => {
                if !entries {
                    return fault();
                }
                return Ok(Fill::Page(Listing {
                    filled,
                    next_marker,
                }));
            }
        }
        consumed += end;
    }
}
