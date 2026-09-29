// Reads a `ListBucketResult` document, the page of an S3 ListObjectsV2, in
// one pass as the Azure reader does. `read_session` at the end reads the
// answer to an S3 Express CreateSession.
//
// Each object is a `Contents` child of the root, with its properties beside
// its key. Each group of keys is a `CommonPrefixes` child that holds one
// `Prefix`.
//
// The keys are URL-encoded, because the request asks for
// `encoding-type=url`. An `EncodingType` element confirms it, but it may
// follow the entries, and each entry is handed out as soon as it is read. So
// every key is decoded, and the page is refused at its end if decoding
// changed a key and the page never named the encoding.

use super::decode::decode;
use super::page::{
    ListProperty, ListPropertySet, check_body, check_room, decode_value_in_place, end_decoded_key,
    open_root_element, read_known, read_other, read_size, set_once, text, values_of,
};
use super::scan::{Child, Scan, Span, fault, trim};
use crate::s3::{ObjectProperty, PropertySet, PropertyValues, Session};
use crate::url::form_decode_in_place;
use crate::{EntryKind, ListEntry, Listing, Result};

const ROOT: &[u8] = b"ListBucketResult";
const OBJECT: &[u8] = b"Contents";
const GROUP: &[u8] = b"CommonPrefixes";

impl ListProperty for ObjectProperty {
    fn name(self) -> &'static str {
        ObjectProperty::name(self)
    }

    fn holds_elements(self) -> bool {
        ObjectProperty::holds_elements(self)
    }

    fn identify(name: &[u8]) -> Option<Self> {
        ObjectProperty::identify(name)
    }
}

impl ListPropertySet for PropertySet {
    type Property = ObjectProperty;

    fn contains(self, property: ObjectProperty) -> bool {
        PropertySet::contains(self, property)
    }

    fn is_empty(self) -> bool {
        PropertySet::is_empty(self)
    }

    fn slot(self, property: ObjectProperty) -> usize {
        PropertySet::slot(self, property)
    }
}

pub(crate) fn fill_listing<'b, E>(
    body: &'b mut [u8],
    into: &mut [E],
    wanted: PropertySet,
    mut build: impl FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) -> E,
) -> Result<Listing<'b>> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    open_root_element(&mut scan, ROOT)?;
    // As in the Azure reader, the read is compiled once for every entry type.
    let room = into.len();
    let mut built = 0;
    let mut sink = |entry: ListEntry<'b>, values: PropertyValues<'_, 'b>| {
        // In range: the read calls this only while `built` is below `room`.
        into[built] = build(entry, values);
        built += 1;
    };
    read_root_children_into(scan, room, wanted, &mut sink)
}

type Sink<'s, 'b> = dyn FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) + 's;

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
    wanted: PropertySet,
    sink: &mut Sink<'_, 'b>,
) -> Result<Listing<'b>> {
    let mut page = Page::default();
    // The spans of the wanted properties of one entry, then their values.
    // Only the first `wanted.len()` slots are used.
    let mut spans = [None; ObjectProperty::COUNT];
    let mut values: [Option<&'b [u8]>; ObjectProperty::COUNT] = [None; ObjectProperty::COUNT];
    let slots = wanted.len();
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
        let captured = &mut spans[..slots];
        captured.fill(None);
        // Nearly every child of a page is an object, which is one compare.
        let fields = if scan.lit(b"<Contents>") {
            Some(read_object(&mut scan, wanted, captured)?)
        } else {
            match scan.child(ROOT)? {
                Child::Close => break,
                Child::Open(tag) => match scan.text(tag.name) {
                    // An entry with nothing in it has no key.
                    OBJECT | GROUP if tag.empty => return fault(),
                    OBJECT => Some(read_object(&mut scan, wanted, captured)?),
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
                    // The echoed request values and the counts.
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
            values_of(entry.raw, &spans[..slots], &mut values[..slots]);
            sink(entry, PropertyValues::new(wanted, &values[..slots]));
            page.built += 1;
        }
    }
    finish(page, room)
}

fn finish<'b>(page: Page<'b>, room: usize) -> Result<Listing<'b>> {
    // Without `EncodingType`, a `%` or a `+` in a key may have been text.
    if page.decoded && !page.encoded {
        return fault();
    }
    check_room(page.held, room)?;
    // `IsTruncated` and the token must agree. Without `IsTruncated`, the
    // token decides.
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

// Reads an object. `<Contents>` has been consumed. Each field and property
// is matched whole, as AWS spells its start tag, and again by name on the
// general path for any other spelling. Add a new one to both lists.
//
// `captured` has one slot per member of `wanted`.
fn read_object(
    scan: &mut Scan<'_>,
    wanted: PropertySet,
    captured: &mut [Option<Span>],
) -> Result<Fields> {
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
            b'S' if scan.lit(b"<StorageClass>") => {
                read_known(scan, ObjectProperty::StorageClass, wanted, captured)?;
            }
            b'C' if scan.lit(b"<ChecksumAlgorithm>") => {
                read_known(scan, ObjectProperty::ChecksumAlgorithm, wanted, captured)?;
            }
            b'C' if scan.lit(b"<ChecksumType>") => {
                read_known(scan, ObjectProperty::ChecksumType, wanted, captured)?;
            }
            b'O' if scan.lit(b"<Owner>") => {
                read_known(scan, ObjectProperty::Owner, wanted, captured)?
            }
            b'R' if scan.lit(b"<RestoreStatus>") => {
                read_known(scan, ObjectProperty::RestoreStatus, wanted, captured)?;
            }
            b'/' if scan.lit(b"</Contents>") => break,
            _ => match scan.child(OBJECT)? {
                Child::Close => break,
                Child::Open(tag) => match scan.text(tag.name) {
                    b"Key" => set_once(&mut fields.key, scan.value(tag)?)?,
                    b"LastModified" => set_once(&mut fields.last_modified, scan.value(tag)?)?,
                    b"ETag" => set_once(&mut fields.e_tag, scan.value(tag)?)?,
                    b"Size" => set_once(&mut fields.size, scan.value(tag)?.0)?,
                    _ => read_other(scan, tag, wanted, captured)?,
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

// Builds one entry from its bytes, decoding its key, entity tag and date in
// place. Also returns whether URL-decoding changed the key.
fn build_entry(chunk: &mut [u8], fields: Fields) -> Result<(ListEntry<'_>, bool)> {
    let Some((key, key_flags)) = fields.key else {
        return fault();
    };
    // S3 stores no object under an empty key, and groups no keys under an
    // empty prefix.
    if key.0 == key.1 {
        return fault();
    }
    let size = fields.size.map(|span| read_size(chunk, span)).transpose()?;

    // A key may begin or end with a space, so the key is not trimmed. XML
    // escaping is undone first, because the document applied it last.
    let escaped = decode(&mut chunk[key.0..key.1], key_flags, false)?;
    let (key_len, decoded) = form_decode_in_place(&mut chunk[key.0..key.0 + escaped]);
    end_decoded_key(chunk, key, key_len, true)?;
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

// Reads a `CreateSessionResult`, the answer to an S3 Express CreateSession.
// Its `Credentials` child holds the key, the secret, the token and when they
// expire. Nothing is taken off the body until the root is closed, so every
// span indexes the whole document, and each value is decoded in place after.
pub(crate) fn read_session(body: &mut [u8]) -> Result<Session<'_>> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    open_root_element(&mut scan, b"CreateSessionResult")?;
    let mut fields: [Option<(Span, u8)>; 4] = [None; 4];
    let mut seen_credentials = false;
    loop {
        match scan.child(b"CreateSessionResult")? {
            Child::Close => break,
            Child::Open(tag) if scan.text(tag.name) == b"Credentials" && !tag.empty => {
                if seen_credentials {
                    return fault();
                }
                seen_credentials = true;
                loop {
                    match scan.child(b"Credentials")? {
                        Child::Close => break,
                        Child::Open(tag) => {
                            let slot = match scan.text(tag.name) {
                                b"AccessKeyId" => 0,
                                b"SecretAccessKey" => 1,
                                b"SessionToken" => 2,
                                b"Expiration" => 3,
                                _ => {
                                    scan.skip(tag)?;
                                    continue;
                                }
                            };
                            set_once(&mut fields[slot], scan.value(tag)?)?;
                        }
                    }
                }
            }
            Child::Open(tag) => scan.skip(tag)?,
        }
    }
    let chunk = scan.take();
    let mut spans = [None; 4];
    for (span, field) in spans.iter_mut().zip(fields) {
        *span = decode_value_in_place(chunk, field)?;
    }
    let [Some(key_id), Some(secret), Some(token), expiration] = spans else {
        return fault();
    };
    Ok(Session {
        key_id: text(&chunk[key_id.0..key_id.1])?,
        secret: text(&chunk[secret.0..secret.1])?,
        token: text(&chunk[token.0..token.1])?,
        expiration: match expiration {
            Some((start, end)) => Some(text(&chunk[start..end])?),
            None => None,
        },
    })
}
