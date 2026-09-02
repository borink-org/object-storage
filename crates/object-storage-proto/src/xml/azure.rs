// The `EnumerationResults` grammar, on the scanner beside it.
//
// One pass over the body. The document is read as it is walked: the entries
// are the only thing kept, each one is split off as its own borrow of the
// body so that its text can be decoded where it stands and then lent out, and
// the array the caller passed is the budget. An entry that does not fit is
// not read at all, so the bytes it stands in are still a document and the next
// call reads it from there.

use super::decode::decode;
use super::scan::{Child, Scan, Span, Tag, fault, split_off, trim};
use crate::{EntryKind, Fill, ListEntry, Listing, Result, Resume};

const ROOT: &[u8] = b"EnumerationResults";
const ENTRIES: &[u8] = b"Blobs";

pub(crate) fn fill_listing<'b>(body: &'b mut [u8], into: &mut [ListEntry<'b>]) -> Result<Fill<'b>> {
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

// The bytes before the first child of the root element.
//
// The whole body is checked for its encoding here, once, before anything is
// read out of it: a document that is not valid in its encoding is not XML.
// Measured at 45 to 50 GB/s against a read that runs at 1.7, so it is a few
// percent of the read. It does not stand in for the check each key gets after
// it is decoded, because a percent escape can write any byte.
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
    // status. That is not a page, and this is what says so.
    if sc.text(root.name) != ROOT || root.empty {
        return fault();
    }
    Ok(sc.i)
}

// One child of the element that is being read, as the driver needs to see it.
enum Item {
    Entry(Fields),
    // The element that holds the entries, and whether it holds any.
    Entries { within: bool },
    // The entries element has ended.
    Leave,
    // The text that names the next page.
    Marker((Span, u8)),
    // An element this crate reads nothing from.
    Skip,
    // The root element has ended.
    End,
}

// One item, read from the bytes that begin with it. The driver has already
// stepped over the whitespace before it, so the byte at zero is its own.
fn item(b: &[u8], within: bool) -> Result<(Item, usize)> {
    let mut sc = Scan::new(b);
    if !within {
        let item = match sc.child(ROOT)? {
            Child::Close => Item::End,
            Child::Open(tag) => match sc.text(tag.name) {
                ENTRIES => Item::Entries { within: !tag.empty },
                // The marker names the page to ask for next, so it holds text
                // and nothing else. The service writes it as `<NextMarker/>`,
                // as `<NextMarker />`, and with text between two tags, and all
                // three name the same thing.
                b"NextMarker" => Item::Marker(sc.value(tag)?),
                _ => {
                    sc.skip(tag)?;
                    Item::Skip
                }
            },
        };
        return Ok((item, sc.i));
    }
    // Inside the entries, where nothing but an entry belongs. A comment, a
    // character-data section and a processing instruction may each hold the
    // very tags that the entries are read by, so all three are refused here
    // rather than stepped over.
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
    // `<Blob>` is one six-byte compare, which is how nearly every entry of a
    // page is recognised. Anything else falls to the general path below.
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
        // Something else where an entry belongs is not a page this crate can
        // read: a later service version writing one here would be read as an
        // entry it is not.
        _ => return fault(),
    };
    Ok((Item::Entry(fields), sc.i))
}

// The fields of one entry, as ranges of the entry's own bytes. Recording
// ranges rather than slices is what lets the text be decoded afterwards, when
// the entry has been split off the body.
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

// A value written twice would leave which of them was meant to a rule this
// crate would be inventing, so the second one is a fault rather than a choice.
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
                // The properties are read only under this element, so one
                // that a later service version adds beside it cannot be
                // mistaken for one of them.
                b"Properties" if !tag.empty => properties(sc, &mut fields)?,
                _ => sc.skip(tag)?,
            },
        }
    }
    // An entry with no name names no object.
    if fields.key.is_none() {
        return fault();
    }
    Ok(fields)
}

// `<Properties>` has been consumed. Four of its children matter; the rest, a
// dozen or so per blob, are the bulk of an Azure page and are stepped over
// without being looked at.
fn properties(sc: &mut Scan<'_>, fields: &mut Fields) -> Result<()> {
    loop {
        sc.skip_space();
        // The children the service writes, matched whole and dispatched on
        // the byte after the `<`, so one child costs one compare.
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

// A group of keys. A hierarchical-namespace account gives one a `<Properties>`
// block too; nothing consumes it, since a group has no size and no entity tag
// to use.
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

// A name, and the one attribute this crate reads. Azure writes `Encoded`
// once, and writes a boolean in it.
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

// One entry, out of the bytes it was written in.
fn materialize(chunk: &mut [u8], fields: Fields) -> Result<ListEntry<'_>> {
    let Some((key, key_flags)) = fields.key else {
        return fault();
    };
    // A directory is told from an object before any decoding, because the
    // value is the service's own word and carries nothing to decode.
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
        // An object states its length. A group of keys and a directory are
        // not objects and have none to state.
        None if !fields.prefix && !directory => return fault(),
        None => None,
    };
    let kind = match (fields.prefix, directory) {
        (true, _) => EntryKind::Prefix,
        (false, true) => EntryKind::Directory,
        (false, false) => EntryKind::Object,
    };

    // A key may begin or end with a space, so only the values that the service
    // writes for itself are trimmed.
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

// One value, decoded where it stands, as what is left of it.
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
    // The marker stands past the entries, so the read and the marker are two
    // pieces of the body that are never the same bytes.
    let (before, mut rest) = body.split_at_mut(position.at);
    let before: &'b [u8] = before;
    let mut next_marker: Option<&'b [u8]> = position
        .marker
        .map(|(start, end)| &before[start..end])
        .filter(|marker| !marker.is_empty());
    let mut consumed = position.at;
    let mut filled = 0;
    // A document with no entries element at all is not a page. A resumed read
    // starts past it and cannot see it, so only a read from the start says so.
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
                // The array is the budget. An entry that does not fit is not
                // read at all, so the next call reads it from where it stands.
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
                // An empty marker is how the service says that the listing is
                // complete, so it names no next page rather than an empty one.
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
