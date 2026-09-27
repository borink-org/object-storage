// Reads a `ListBucketResult` document, the page of an S3 ListObjectsV2, using
// the scanner in `scan.rs`.
//
// The read is one pass over the body, as the Azure reader's is, and it shares
// that reader's checks and decoding. The document differs in three ways.
//
// - The entries are children of the root itself. Each object is a `Contents`
//   element, and each group of keys a `CommonPrefixes` element that holds one
//   `Prefix`. AWS writes every object of a page before its groups; this
//   reader takes them in any order.
// - An object's properties stand in the entry beside its key, not under an
//   element of their own.
// - The keys are URL-encoded, because the request asks for
//   `encoding-type=url`. The page says so in an `EncodingType` element, which
//   AWS writes before the entries and other services may write after them.
//   A key is taken off the body and handed out as soon as it has been read,
//   so the reader cannot wait for that element. It decodes every key, and
//   refuses the page at its end if decoding changed a key and the page never
//   said that it encoded one. A key that holds no `%` and no `+` reads the
//   same either way.

use super::azure::{check_body, decode_value_in_place, open_root_element, set_once, text};
use super::decode::{decode, decode_url};
use super::scan::{Child, Scan, Span, fault, trim};
use crate::{CapacityError, EntryKind, Error, ListEntry, Listing, Result};

const ROOT: &[u8] = b"ListBucketResult";
const OBJECT: &[u8] = b"Contents";
const GROUP: &[u8] = b"CommonPrefixes";

pub(crate) fn fill_listing<'b, E>(
    body: &'b mut [u8],
    into: &mut [E],
    mut build: impl FnMut(ListEntry<'b>) -> E,
) -> Result<Listing<'b>> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    open_root_element(&mut scan, ROOT)?;
    // As in the Azure reader, the read is compiled once whatever the entry
    // type, and hands each entry to this closure.
    let room = into.len();
    let mut built = 0;
    let mut sink = |entry: ListEntry<'b>| {
        // In range: the read calls this only while `built` is below `room`.
        into[built] = build(entry);
        built += 1;
    };
    read_root_children_into(scan, room, &mut sink)
}

type Sink<'s, 'b> = dyn FnMut(ListEntry<'b>) + 's;

// What the page said about itself, collected as its root's children are read.
#[derive(Default)]
struct Page<'b> {
    // How many entries the page held, and how many were built.
    held: usize,
    built: usize,
    // Whether decoding changed any key.
    decoded: bool,
    // Whether the page said that it URL-encoded its keys.
    encoded: bool,
    truncated: Option<bool>,
    token: Option<&'b str>,
}

fn read_root_children_into<'b>(
    mut scan: Scan<'b>,
    room: usize,
    sink: &mut Sink<'_, 'b>,
) -> Result<Listing<'b>> {
    let mut page = Page::default();
    let mut seen_encoding = false;
    let mut seen_token = false;
    loop {
        // Drop what was read before this child, so that the child begins at
        // offset zero, and the bytes taken after it are the child whole.
        scan.skip_space();
        scan.take();
        // A comment or a processing instruction is taken off on its own, so
        // that an entry's bytes begin with the entry's own tag.
        if scan.cur() == b'<' && scan.skip_misc()? {
            continue;
        }
        // Nearly every child of a page is an object, which is one compare.
        let fields = if scan.lit(b"<Contents>") {
            Some(read_object(&mut scan)?)
        } else {
            match scan.child(ROOT)? {
                Child::Close => break,
                Child::Open(tag) => match scan.text(tag.name) {
                    // An entry with nothing in it has no key.
                    OBJECT | GROUP if tag.empty => return fault(),
                    OBJECT => Some(read_object(&mut scan)?),
                    GROUP => Some(read_group(&mut scan)?),
                    b"EncodingType" => {
                        // A second one is refused, as a second key is.
                        if seen_encoding {
                            return fault();
                        }
                        seen_encoding = true;
                        let (span, _) = scan.value(tag)?;
                        // The request asked for `url`, and for nothing else.
                        if scan.text(span).trim_ascii() != b"url" {
                            return fault();
                        }
                        page.encoded = true;
                        None
                    }
                    b"IsTruncated" => {
                        let (span, _) = scan.value(tag)?;
                        let truncated = match scan.text(span).trim_ascii() {
                            b"true" => true,
                            b"false" => false,
                            _ => return fault(),
                        };
                        set_once(&mut page.truncated, truncated)?;
                        None
                    }
                    b"NextContinuationToken" => {
                        if seen_token {
                            return fault();
                        }
                        seen_token = true;
                        let (span, flags) = scan.value(tag)?;
                        let chunk = scan.take();
                        let (start, stop) = trim(chunk, span);
                        // The token is not URL-encoded, only escaped for XML.
                        // `decode` returns at most the length it was given.
                        let len = decode(&mut chunk[start..stop], flags, false)?;
                        let chunk: &'b [u8] = chunk;
                        page.token = Some(&chunk[start..start + len])
                            .filter(|token| !token.is_empty())
                            .map(text)
                            .transpose()?;
                        None
                    }
                    // The echo of the request, and the counts, which the
                    // entries themselves say.
                    _ => {
                        scan.skip(tag)?;
                        None
                    }
                },
            }
        };
        let Some(fields) = fields else {
            continue;
        };
        let chunk = scan.take();
        page.held += 1;
        if page.built < room {
            let (entry, decoded) = build_entry(chunk, fields)?;
            page.decoded |= decoded;
            sink(entry);
            page.built += 1;
        }
    }
    finish(page, room)
}

fn finish<'b>(page: Page<'b>, room: usize) -> Result<Listing<'b>> {
    // A key that decoding changed, on a page that never said it encoded its
    // keys, may have held its `%` or `+` as text. The entries built from it
    // name keys that are not the objects'.
    if page.decoded && !page.encoded {
        return fault();
    }
    if page.held > room {
        return Err(Error::Capacity(CapacityError {
            required: page.held,
            ..CapacityError::default()
        }));
    }
    // A page that names no next one and says that more follow, or names one
    // and says that none do, contradicts itself. A page that does not say is
    // taken at its token.
    let next_marker = match (page.truncated, page.token) {
        (Some(true) | None, Some(token)) => Some(token),
        (Some(false) | None, None) => None,
        (Some(true), None) | (Some(false), Some(_)) => return fault(),
    };
    Ok(Listing {
        filled: page.built,
        next_marker,
    })
}

// The fields of one entry, as ranges into the entry's own bytes, which are
// decoded once the entry has been taken off the body.
struct Fields {
    prefix: bool,
    key: Option<(Span, u8)>,
    size: Option<Span>,
    e_tag: Option<(Span, u8)>,
    last_modified: Option<(Span, u8)>,
}

impl Fields {
    const fn new(prefix: bool) -> Self {
        Self {
            prefix,
            key: None,
            size: None,
            e_tag: None,
            last_modified: None,
        }
    }
}

// Reads an object. `<Contents>` has been consumed. Each field is matched
// whole, as AWS spells its start tag, and again by name on the general path,
// which any legal spelling reaches. A field added to one list must be added
// to the other. Everything else, such as `StorageClass` and `Owner`, stays
// in the entry's bytes for `ListEntry::property`.
fn read_object(scan: &mut Scan<'_>) -> Result<Fields> {
    let mut fields = Fields::new(false);
    loop {
        scan.skip_space();
        match scan.peek(1) {
            b'K' if scan.lit(b"<Key>") => set_once(&mut fields.key, scan.value_of(b"Key")?)?,
            b'L' if scan.lit(b"<LastModified>") => {
                let value = scan.value_of(b"LastModified")?;
                set_once(&mut fields.last_modified, value)?;
            }
            b'E' if scan.lit(b"<ETag>") => set_once(&mut fields.e_tag, scan.value_of(b"ETag")?)?,
            b'S' if scan.lit(b"<Size>") => set_once(&mut fields.size, scan.value_of(b"Size")?.0)?,
            b'/' if scan.lit(b"</Contents>") => break,
            _ => match scan.child(OBJECT)? {
                Child::Close => break,
                Child::Open(tag) => match scan.text(tag.name) {
                    b"Key" => set_once(&mut fields.key, scan.value(tag)?)?,
                    b"LastModified" => set_once(&mut fields.last_modified, scan.value(tag)?)?,
                    b"ETag" => set_once(&mut fields.e_tag, scan.value(tag)?)?,
                    b"Size" => set_once(&mut fields.size, scan.value(tag)?.0)?,
                    _ => scan.skip(tag)?,
                },
            },
        }
    }
    // An object always has a key and a length.
    if fields.key.is_none() || fields.size.is_none() {
        return fault();
    }
    Ok(fields)
}

// Reads a group of keys. `<CommonPrefixes>` has been consumed. S3 writes one
// `Prefix` in each.
fn read_group(scan: &mut Scan<'_>) -> Result<Fields> {
    let mut fields = Fields::new(true);
    loop {
        match scan.child(GROUP)? {
            Child::Close => break,
            Child::Open(tag) => match scan.text(tag.name) {
                b"Prefix" => set_once(&mut fields.key, scan.value(tag)?)?,
                _ => scan.skip(tag)?,
            },
        }
    }
    if fields.key.is_none() {
        return fault();
    }
    Ok(fields)
}

// Builds one entry from the bytes it was written in, decoding its key, entity
// tag and date in place, and returns whether URL-decoding changed the key.
fn build_entry(chunk: &mut [u8], fields: Fields) -> Result<(ListEntry<'_>, bool)> {
    let Some((key, key_flags)) = fields.key else {
        return fault();
    };
    // S3 stores no object under an empty key, and groups no keys under an
    // empty prefix.
    if key.0 == key.1 {
        return fault();
    }
    let size = match fields.size {
        Some(span) => {
            let (start, end) = trim(chunk, span);
            match crate::common::decimal(&chunk[start..end]) {
                Some(size) => Some(size),
                None => return fault(),
            }
        }
        None => None,
    };

    // A key may begin or end with a space, so the key is not trimmed. XML
    // escaping is undone first, because the document applied it last.
    let escaped = decode(&mut chunk[key.0..key.1], key_flags, false)?;
    let (key_len, decoded) = decode_url(&mut chunk[key.0..key.0 + escaped])?;
    if key_len < key.1 - key.0 {
        // As on Azure: the bytes the key no longer needs are set to zero, so
        // that the walk over the entry can tell where the decoded text ends.
        chunk[key.0 + key_len..key.1].fill(0);
        // A percent escape can name a zero byte, which would look like that
        // filler. It is refused, as a key the walk could not read back.
        if chunk[key.0..key.0 + key_len].contains(&0) {
            return fault();
        }
    }
    let e_tag = decode_value_in_place(chunk, fields.e_tag)?;
    let last_modified = decode_value_in_place(chunk, fields.last_modified)?;

    let raw: &[u8] = chunk;
    let entry = ListEntry {
        kind: if fields.prefix {
            EntryKind::Prefix
        } else {
            EntryKind::Object
        },
        key: text(&raw[key.0..key.0 + key_len])?,
        size,
        e_tag: e_tag
            .map(|(start, end)| text(&raw[start..end]))
            .transpose()?,
        last_modified: last_modified
            .map(|(start, end)| text(&raw[start..end]))
            .transpose()?,
        content_type: None,
        raw,
    };
    Ok((entry, decoded))
}
