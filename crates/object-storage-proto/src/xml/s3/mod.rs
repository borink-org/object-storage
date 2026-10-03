// Reads a `ListBucketResult` document, the page of an S3 ListObjectsV2, or
// a `ListVersionsResult`, the page of a ListObjectVersions, in one pass. `read_session` at the end reads the
// answer to an S3 Express CreateSession. `parts.rs` reads the answers of
// an upload in parts, `copy.rs` those of a copy, and `batch.rs` the answer
// of a DeleteObjects. `read_values` reads the small documents that the
// last three answer with.
//
// Each object is a `Contents` child of the root, with its properties beside
// its key. Each group of keys is a `CommonPrefixes` child that holds one
// `Prefix`. A page of versions holds a `Version` for each version of an
// object, and a `DeleteMarker`, which has no size, for each delete marker.
// It names the next page with `NextKeyMarker`, a key, and
// `NextVersionIdMarker` rather than a continuation token.
//
// The keys are URL-encoded, because the request asks for
// `encoding-type=url`. An `EncodingType` element confirms it, but it may
// follow the entries, and each entry is handed out as soon as it is read. So
// every key is decoded, and the page is refused at its end if decoding
// changed a key and the page never named the encoding.

pub(crate) mod batch;
pub(crate) mod copy;
pub(crate) mod parts;

use crate::layered::iso8601_ms;
use crate::s3::{ObjectProperty, PropertySet, PropertyValues, Session};
use crate::url::form_decode_in_place;
use crate::xml::decode::decode;
use crate::xml::page::{
    ListProperty, ListPropertySet, check_body, check_room, decode_value_in_place, end_decoded_key,
    open_root_element, read_known, read_other, read_size, set_once, text, values_of,
};
use crate::xml::scan::{Child, Scan, Span, fault, trim};
use crate::{EntryKind, ListEntry, ListMarker, Listing, Result};

const ROOT: &[u8] = b"ListBucketResult";
const OBJECT: &[u8] = b"Contents";
const GROUP: &[u8] = b"CommonPrefixes";
const VERSIONS_ROOT: &[u8] = b"ListVersionsResult";
const VERSION: &[u8] = b"Version";
const DELETE_MARKER: &[u8] = b"DeleteMarker";

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
    // A ListObjectVersions answers with a root of its own.
    let versions = crate::xml::root_is(body, VERSIONS_ROOT);
    let mut scan = Scan::new(body);
    open_root_element(&mut scan, if versions { VERSIONS_ROOT } else { ROOT })?;
    // The read is compiled once for every entry type.
    let room = into.len();
    let mut built = 0;
    let mut sink = |entry: ListEntry<'b>, values: PropertyValues<'_, 'b>| {
        // In range: the read calls this only while `built` is below `room`.
        into[built] = build(entry, values);
        built += 1;
    };
    read_root_children_into(scan, room, wanted, versions, &mut sink)
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
    // The continuation token, or on a page of versions the key marker.
    token: Option<&'b str>,
    // The version marker of a page of versions.
    version_token: Option<&'b str>,
    // Whether the page lists versions, and so names its next page by a key
    // and a version.
    versions: bool,
}

fn read_root_children_into<'b>(
    mut scan: Scan<'b>,
    room: usize,
    wanted: PropertySet,
    versions: bool,
    sink: &mut Sink<'_, 'b>,
) -> Result<Listing<'b>> {
    let root = if versions { VERSIONS_ROOT } else { ROOT };
    let mut page = Page {
        versions,
        ..Page::default()
    };
    // The spans of the wanted properties of one entry, then their values.
    // Only the first `wanted.len()` slots are used.
    let mut spans = [None; ObjectProperty::COUNT];
    let mut values: [Option<&'b [u8]>; ObjectProperty::COUNT] = [None; ObjectProperty::COUNT];
    let slots = wanted.len();
    let mut seen_encoding = false;
    let mut seen_token = false;
    let mut seen_version_token = false;
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
        let fields = if !versions && scan.lit(b"<Contents>") {
            Some(read_object(&mut scan, wanted, captured, Entry::Object)?)
        } else {
            match scan.child(root)? {
                Child::Close => break,
                Child::Open(tag) => match scan.text(tag.name) {
                    // An entry with nothing in it has no key.
                    OBJECT | GROUP | VERSION | DELETE_MARKER if tag.empty => return fault(),
                    OBJECT if !versions => {
                        Some(read_object(&mut scan, wanted, captured, Entry::Object)?)
                    }
                    VERSION if versions => {
                        Some(read_object(&mut scan, wanted, captured, Entry::Version)?)
                    }
                    DELETE_MARKER if versions => Some(read_object(
                        &mut scan,
                        wanted,
                        captured,
                        Entry::DeleteMarker,
                    )?),
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
                    b"NextKeyMarker" if versions => {
                        if seen_token {
                            return fault();
                        }
                        seen_token = true;
                        let (span, flags) = scan.value(tag)?;
                        let chunk = scan.take();
                        // The marker is a key, so it is URL-encoded as the
                        // keys are, and kept with its spaces.
                        let escaped = decode(&mut chunk[span.0..span.1], flags, false)?;
                        let (len, decoded) =
                            form_decode_in_place(&mut chunk[span.0..span.0 + escaped]);
                        page.decoded |= decoded;
                        let chunk: &'b [u8] = chunk;
                        page.token = Some(&chunk[span.0..span.0 + len])
                            .filter(|marker| !marker.is_empty())
                            .map(text)
                            .transpose()?;
                        None
                    }
                    b"NextVersionIdMarker" if versions => {
                        if seen_version_token {
                            return fault();
                        }
                        seen_version_token = true;
                        let (span, flags) = scan.value(tag)?;
                        let chunk = scan.take();
                        let (start, stop) = trim(chunk, span);
                        let len = decode(&mut chunk[start..stop], flags, false)?;
                        let chunk: &'b [u8] = chunk;
                        page.version_token = Some(&chunk[start..start + len])
                            .filter(|marker| !marker.is_empty())
                            .map(text)
                            .transpose()?;
                        None
                    }
                    b"NextContinuationToken" if !versions => {
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
    // A version marker names a place among the versions of the next key.
    if page.version_token.is_some() && next_marker.is_none() {
        return fault();
    }
    let next_marker = next_marker.map(|token| match page.versions {
        true => ListMarker::Version {
            key: token,
            version: page.version_token,
        },
        false => ListMarker::Text(token),
    });
    Ok(Listing {
        filled: page.built,
        next_marker,
    })
}

// The kinds of entry that hold an object's fields.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    // A `Contents` of a ListObjectsV2.
    Object,
    // A `Version` of a ListObjectVersions.
    Version,
    // A `DeleteMarker` of a ListObjectVersions, which has no size.
    DeleteMarker,
}

impl Entry {
    const fn name(self) -> &'static [u8] {
        match self {
            Self::Object => OBJECT,
            Self::Version => VERSION,
            Self::DeleteMarker => DELETE_MARKER,
        }
    }

    const fn close(self) -> &'static [u8] {
        match self {
            Self::Object => b"</Contents>",
            Self::Version => b"</Version>",
            Self::DeleteMarker => b"</DeleteMarker>",
        }
    }
}

// The fields of one entry, as ranges into the entry's own bytes, which are
// decoded once the entry has been taken off the body.
struct Fields {
    prefix: bool,
    delete_marker: bool,
    key: Option<(Span, u8)>,
    size: Option<Span>,
    e_tag: Option<(Span, u8)>,
    last_modified: Option<(Span, u8)>,
}

impl Fields {
    const fn new(prefix: bool) -> Self {
        Self {
            prefix,
            delete_marker: false,
            key: None,
            size: None,
            e_tag: None,
            last_modified: None,
        }
    }
}

// Reads an object, a version or a delete marker, whose start tag has been
// consumed. Each field and property is matched whole, as AWS spells its
// start tag, and again by name on the general path for any other spelling.
// Add a new one to both lists.
//
// `captured` has one slot per member of `wanted`.
fn read_object(
    scan: &mut Scan<'_>,
    wanted: PropertySet,
    captured: &mut [Option<Span>],
    entry: Entry,
) -> Result<Fields> {
    let mut fields = Fields::new(false);
    fields.delete_marker = entry == Entry::DeleteMarker;
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
            b'/' if scan.lit(entry.close()) => break,
            _ => match scan.child(entry.name())? {
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
    // An object always has a key and a length. A delete marker has a key
    // and no bytes.
    if fields.key.is_none() || fields.size.is_none() != fields.delete_marker {
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
        } else if fields.delete_marker {
            EntryKind::DeleteMarker
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
        expires_at: match expiration {
            Some((start, end)) => match iso8601_ms(text(&chunk[start..end])?) {
                Some(millis) => Some(millis / 1000),
                None => return fault(),
            },
            None => None,
        },
    })
}

// Reads the values of the children of the root `root` that `names` names,
// each decoded in place, and skips every other child. A child named twice is
// a fault. As in `read_session`, nothing is taken off the body until the root
// is closed, so every span indexes the whole document.
pub(crate) fn read_values<'b, const N: usize>(
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
