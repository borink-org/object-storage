// Reads an `EnumerationResults` document, using the scanner in `scan.rs`.
//
// The read is one pass over the body. Only the entries are kept. Each entry
// is taken off the body as its own borrow the moment its close tag has been
// read. Its text is decoded in place and then handed to the caller. The array
// the caller passed must hold the whole page; one that does not is refused
// with the number of entries the page holds.

use super::decode::decode;
use super::scan::{Child, Scan, Span, Tag, fault, trim};
use crate::{
    BlobProperty, CapacityError, EntryKind, Error, ListEntry, Listing, PropertySet, PropertyValues,
    Result,
};

const ROOT: &[u8] = b"EnumerationResults";
const BLOBS: &[u8] = b"Blobs";

pub(crate) fn fill_listing<'b, E>(
    body: &'b mut [u8],
    into: &mut [E],
    wanted: PropertySet,
    mut build: impl FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) -> E,
) -> Result<Listing<'b>> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    open_root_element(&mut scan)?;
    // The read below is written once, whatever the entry type: it hands each
    // entry it builds to this closure, which makes the caller's and writes
    // it. A generic read would be compiled once per entry type, and a program
    // with several, such as the C crate with its two fill calls, would carry
    // several copies of the reader. The indirect call costs one entry's worth
    // of nothing measurable.
    let room = into.len();
    let mut built = 0;
    let mut sink = |entry: ListEntry<'b>, values: PropertyValues<'_, 'b>| {
        // In range: the read calls this only while `built` is below `room`.
        into[built] = build(entry, values);
        built += 1;
    };
    read_root_children_into(scan, room, wanted, &mut sink)
}

// Receives each entry the read builds, with its values. See `fill_listing`.
type Sink<'s, 'b> = dyn FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) + 's;

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
fn check_body(body: &[u8]) -> Result<()> {
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

// Reads the prolog and the root's opening tag, and leaves the scan where the
// root's first child begins.
fn open_root_element(scan: &mut Scan<'_>) -> Result<()> {
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
    let root = scan.open()?;
    // A service can answer a listing with an error document under a success
    // status. That is not a page.
    if scan.text(root.name) != ROOT || root.empty {
        return fault();
    }
    Ok(())
}

// One child of the root element, as the loop in `read_root_children_into`
// sees it.
enum Item {
    // The element that holds the entries, and whether it was written as an
    // empty tag. Its children are read by `read_entries_into`.
    Blobs { empty: bool },
    // The text that names the next page.
    Marker((Span, u8)),
    // An element this crate does not read.
    Skip,
    // The root element has ended.
    End,
}

// Reads one child of the root. The caller has already skipped the whitespace
// before it and taken everything before that off the scan, so the child
// begins at offset zero.
fn read_root_child(scan: &mut Scan<'_>) -> Result<Item> {
    let item = match scan.child(ROOT)? {
        Child::Close => Item::End,
        Child::Open(tag) => match scan.text(tag.name) {
            BLOBS => Item::Blobs { empty: tag.empty },
            // The marker names the next page, so it holds only text. The
            // service writes it as `<NextMarker/>`, as `<NextMarker />`, or
            // with text between two tags. All three are the same element.
            b"NextMarker" => Item::Marker(scan.value(tag)?),
            _ => {
                scan.skip(tag)?;
                Item::Skip
            }
        },
    };
    Ok(item)
}

// What reading the entries element found.
struct Entries {
    // How many entries the element held.
    held: usize,
    // How many of them were built into the caller's array. Less than `held`
    // only when the array was too small.
    built: usize,
}

// Reads the entries. `<Blobs>` has been consumed, and the read ends after
// `</Blobs>`. Each entry is written into `into` until the array is full.
// After that an entry is still walked and counted, but not built, so that
// the error can say how many entries the page holds.
//
// The values of the wanted properties are collected per entry into a scratch
// of which only the first `wanted.len()` slots are used, so a set that names
// nothing costs nothing here.
fn read_entries_into<'b>(
    scan: &mut Scan<'b>,
    room: usize,
    wanted: PropertySet,
    sink: &mut Sink<'_, 'b>,
) -> Result<Entries> {
    let mut entries = Entries { held: 0, built: 0 };
    let mut spans = [None; BlobProperty::COUNT];
    let mut values: [Option<&'b [u8]>; BlobProperty::COUNT] = [None; BlobProperty::COUNT];
    // At most `COUNT`: a set holds only bits that name a property.
    let slots = wanted.len();
    loop {
        // Drop what was read before this entry, so that the entry begins at
        // offset zero and the spans it records index its own bytes. Then,
        // once it has been read, the bytes the scan has read are exactly the
        // entry, from its opening tag to its closing one.
        scan.skip_space();
        scan.take();

        // Only entries belong here. A comment, a CDATA section and a
        // processing instruction may each hold the tags that entries are
        // read by. All three are refused rather than skipped.
        if scan.cur() != b'<' {
            return fault();
        }
        match scan.peek(1) {
            b'/' => {
                scan.close(BLOBS)?;
                return Ok(entries);
            }
            b'!' | b'?' => return fault(),
            _ => {}
        }
        let captured = &mut spans[..slots];
        captured.fill(None);
        // Nearly every entry of a page starts with `<Blob>`, which is one
        // six-byte compare. Anything else takes the general path below.
        let fields = if scan.lit(b"<Blob>") {
            read_blob(scan, wanted, captured)?
        } else {
            let tag = scan.open()?;
            // An entry with nothing in it has no name, so it is not an entry.
            if tag.empty {
                return fault();
            }
            match scan.text(tag.name) {
                b"Blob" => read_blob(scan, wanted, captured)?,
                // A group of keys gives no values, so its slots stay empty.
                b"BlobPrefix" => read_blob_prefix(scan)?,
                // Any other element where an entry belongs makes this a page
                // this crate cannot read. Skipping it would silently drop an
                // entry that a later service version added.
                _ => return fault(),
            }
        };
        let chunk = scan.take();
        entries.held += 1;
        if entries.built < room {
            let entry = build_entry(chunk, fields)?;
            for (value, span) in values[..slots].iter_mut().zip(&spans[..slots]) {
                // The spans were recorded on the chunk, which `raw` is.
                *value = span.map(|(start, end)| &entry.raw[start..end]);
            }
            sink(entry, PropertyValues::new(wanted, &values[..slots]));
            entries.built += 1;
        }
    }
}

// The fields of one entry, as ranges into the entry's own bytes. Ranges
// rather than slices let the text be decoded later, after the entry has been
// taken off the body.
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

// The functions below are inlined into every instantiation of the entries
// loop rather than shared between them. Shared, each has several callers
// once a program uses more than one fill call, and LLVM then leaves them out
// of line, which costs an eighth of the read. Measured; the bench has a row
// through the C entry point for it.
//
// Reads a blob. `<Blob>` has been consumed. Each child the grammar wants is
// matched twice: whole, as the service spells it, which is one compare, and
// again by name on the general path, which any legal spelling reaches. A
// field added to one list must be added to the other.
//
// `captured` has one slot per member of `wanted`, and receives the span of
// each wanted property's value that this blob writes.
fn read_blob(
    scan: &mut Scan<'_>,
    wanted: PropertySet,
    captured: &mut [Option<Span>],
) -> Result<Fields> {
    let mut fields = Fields::new(false);
    loop {
        scan.skip_space();
        if scan.lit(b"<Name>") {
            let value = scan.value_of(b"Name")?;
            set_once(&mut fields.key, value)?;
            continue;
        }
        if scan.lit(b"<Properties>") {
            read_properties(scan, &mut fields, wanted, captured)?;
            continue;
        }
        if scan.lit(b"</Blob>") {
            break;
        }
        // The element beside the properties that every blob carries, and
        // the versioning elements an account that keeps versions adds.
        if scan.lit(b"<OrMetadata />") || read_known_element(scan, wanted, captured)? {
            continue;
        }
        match scan.child(b"Blob")? {
            Child::Close => break,
            Child::Open(tag) => match scan.text(tag.name) {
                b"Name" => read_name(scan, tag, &mut fields)?,
                // Properties are read only under this element. A property
                // that a later service version adds beside it is not
                // mistaken for one of them.
                b"Properties" if !tag.empty => {
                    read_properties(scan, &mut fields, wanted, captured)?;
                }
                _ => read_other_element(scan, tag, wanted, captured)?,
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
// consumed. Four of its children are fields of every entry. The rest, a
// dozen or so per blob, are most of an Azure page: each is matched whole by
// `read_known_element`, which reads past it and keeps its value if the
// caller asked for it. The four are listed twice, as in `read_blob`.
fn read_properties(
    scan: &mut Scan<'_>,
    fields: &mut Fields,
    wanted: PropertySet,
    captured: &mut [Option<Span>],
) -> Result<()> {
    loop {
        scan.skip_space();
        // The children the service writes, matched whole. The byte after the
        // `<` picks the compare, so one child costs one compare.
        match scan.peek(1) {
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
            _ if read_known_element(scan, wanted, captured)? => {}
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
                    _ => read_other_element(scan, tag, wanted, captured)?,
                },
            },
        }
    }
}

// Reads past an element that the whole-tag match did not take: one this
// crate does not know, or a known one in a spelling the service does not
// use, such as `<AccessTier >` or `<Content-Type/>`. The second kind is
// still kept if the caller asked for it.
fn read_other_element(
    scan: &mut Scan<'_>,
    tag: Tag,
    wanted: PropertySet,
    captured: &mut [Option<Span>],
) -> Result<()> {
    if wanted.is_empty() {
        return scan.skip(tag);
    }
    match BlobProperty::identify(scan.text(tag.name)) {
        Some(property) if wanted.contains(property) => {
            let (span, _) = scan.value(tag)?;
            // A slot is always in range: it is the property's rank in the set,
            // and there is one slot per member. Written through `get_mut` so
            // that no bounds check or panic path is compiled in.
            if let Some(slot) = captured.get_mut(wanted.slot(property)) {
                *slot = Some(span);
            }
            Ok(())
        }
        _ => scan.skip(tag),
    }
}

// Reads past a known element whose start tag was matched whole, and keeps
// its value if the caller asked for it. The value is found the way a field's
// is: the text up to the close tag, which a leaf element is.
// Returns true, for the match above.
#[inline(always)]
fn known(
    scan: &mut Scan<'_>,
    property: BlobProperty,
    wanted: PropertySet,
    captured: &mut [Option<Span>],
) -> Result<bool> {
    let (span, _) = scan.value_of(property.name().as_bytes())?;
    if wanted.contains(property) {
        // A slot is always in range: it is the property's rank in the set,
        // and there is one slot per member. Written through `get_mut` so
        // that no bounds check or panic path is compiled in.
        if let Some(slot) = captured.get_mut(wanted.slot(property)) {
            *slot = Some(span);
        }
    }
    Ok(true)
}

// The same for the empty spelling, `<Name />`, which has no value to read
// past. The empty span stands for an element written empty.
#[inline(always)]
fn known_empty(property: BlobProperty, wanted: PropertySet, captured: &mut [Option<Span>]) -> bool {
    if wanted.contains(property)
        && let Some(slot) = captured.get_mut(wanted.slot(property))
    {
        *slot = Some((0, 0));
    }
    true
}

// Reads past one known element if the bytes at the cursor are its start tag
// as the service spells it, keeping its value if the caller asked for it,
// and returns whether it did. The byte after the `<` picks the group, and
// the names of that group are compared whole in the order a page writes
// them, so the common ones cost one compare. A tag that is none of them is
// left for the general path.
//
// Every member of `BlobProperty::ALL` has an arm here. The content headers,
// which the service writes as `<Name />` when the blob has no such value,
// have a second arm for that spelling, so a page from Azure never reaches
// the general path for them. The tests below check both.
#[inline(always)]
fn read_known_element(
    scan: &mut Scan<'_>,
    wanted: PropertySet,
    captured: &mut [Option<Span>],
) -> Result<bool> {
    use BlobProperty::*;
    match scan.peek(1) {
        b'A' => {
            if scan.lit(b"<AccessTier>") {
                return known(scan, AccessTier, wanted, captured);
            }
            if scan.lit(b"<AccessTierInferred>") {
                return known(scan, AccessTierInferred, wanted, captured);
            }
            if scan.lit(b"<AccessTierChangeTime>") {
                return known(scan, AccessTierChangeTime, wanted, captured);
            }
            if scan.lit(b"<ArchiveStatus>") {
                return known(scan, ArchiveStatus, wanted, captured);
            }
            if scan.lit(b"<Acl>") {
                return known(scan, Acl, wanted, captured);
            }
            Ok(false)
        }
        b'B' => {
            if scan.lit(b"<BlobType>") {
                return known(scan, BlobType, wanted, captured);
            }
            Ok(false)
        }
        b'C' => {
            if scan.lit(b"<Creation-Time>") {
                return known(scan, CreationTime, wanted, captured);
            }
            if scan.lit(b"<Content-Type />") {
                return Ok(known_empty(ContentType, wanted, captured));
            }
            if scan.lit(b"<Content-Type>") {
                return known(scan, ContentType, wanted, captured);
            }
            if scan.lit(b"<Content-Encoding />") {
                return Ok(known_empty(ContentEncoding, wanted, captured));
            }
            if scan.lit(b"<Content-Encoding>") {
                return known(scan, ContentEncoding, wanted, captured);
            }
            if scan.lit(b"<Content-Language />") {
                return Ok(known_empty(ContentLanguage, wanted, captured));
            }
            if scan.lit(b"<Content-Language>") {
                return known(scan, ContentLanguage, wanted, captured);
            }
            if scan.lit(b"<Content-CRC64 />") {
                return Ok(known_empty(ContentCrc64, wanted, captured));
            }
            if scan.lit(b"<Content-CRC64>") {
                return known(scan, ContentCrc64, wanted, captured);
            }
            if scan.lit(b"<Content-MD5 />") {
                return Ok(known_empty(ContentMd5, wanted, captured));
            }
            if scan.lit(b"<Content-MD5>") {
                return known(scan, ContentMd5, wanted, captured);
            }
            if scan.lit(b"<Cache-Control />") {
                return Ok(known_empty(CacheControl, wanted, captured));
            }
            if scan.lit(b"<Cache-Control>") {
                return known(scan, CacheControl, wanted, captured);
            }
            if scan.lit(b"<Content-Disposition />") {
                return Ok(known_empty(ContentDisposition, wanted, captured));
            }
            if scan.lit(b"<Content-Disposition>") {
                return known(scan, ContentDisposition, wanted, captured);
            }
            if scan.lit(b"<CopyId>") {
                return known(scan, CopyId, wanted, captured);
            }
            if scan.lit(b"<CopyStatus>") {
                return known(scan, CopyStatus, wanted, captured);
            }
            if scan.lit(b"<CopySource>") {
                return known(scan, CopySource, wanted, captured);
            }
            if scan.lit(b"<CopyProgress>") {
                return known(scan, CopyProgress, wanted, captured);
            }
            if scan.lit(b"<CopyCompletionTime>") {
                return known(scan, CopyCompletionTime, wanted, captured);
            }
            if scan.lit(b"<CopyStatusDescription>") {
                return known(scan, CopyStatusDescription, wanted, captured);
            }
            Ok(false)
        }
        b'D' => {
            if scan.lit(b"<DeletedTime>") {
                return known(scan, DeletedTime, wanted, captured);
            }
            if scan.lit(b"<Deleted>") {
                return known(scan, Deleted, wanted, captured);
            }
            Ok(false)
        }
        b'E' => {
            if scan.lit(b"<EncryptionScope>") {
                return known(scan, EncryptionScope, wanted, captured);
            }
            if scan.lit(b"<Expiry-Time>") {
                return known(scan, ExpiryTime, wanted, captured);
            }
            Ok(false)
        }
        b'G' => {
            if scan.lit(b"<Group>") {
                return known(scan, Group, wanted, captured);
            }
            Ok(false)
        }
        b'I' => {
            if scan.lit(b"<IsCurrentVersion>") {
                return known(scan, IsCurrentVersion, wanted, captured);
            }
            if scan.lit(b"<IncrementalCopy>") {
                return known(scan, IncrementalCopy, wanted, captured);
            }
            if scan.lit(b"<ImmutabilityPolicyUntilDate>") {
                return known(scan, ImmutabilityPolicyUntilDate, wanted, captured);
            }
            if scan.lit(b"<ImmutabilityPolicyMode>") {
                return known(scan, ImmutabilityPolicyMode, wanted, captured);
            }
            Ok(false)
        }
        b'L' => {
            if scan.lit(b"<LeaseStatus>") {
                return known(scan, LeaseStatus, wanted, captured);
            }
            if scan.lit(b"<LeaseState>") {
                return known(scan, LeaseState, wanted, captured);
            }
            if scan.lit(b"<LeaseDuration>") {
                return known(scan, LeaseDuration, wanted, captured);
            }
            if scan.lit(b"<LegalHold>") {
                return known(scan, LegalHold, wanted, captured);
            }
            Ok(false)
        }
        b'O' => {
            if scan.lit(b"<Owner>") {
                return known(scan, Owner, wanted, captured);
            }
            Ok(false)
        }
        b'P' => {
            if scan.lit(b"<Permissions>") {
                return known(scan, Permissions, wanted, captured);
            }
            Ok(false)
        }
        b'R' => {
            if scan.lit(b"<RemainingRetentionDays>") {
                return known(scan, RemainingRetentionDays, wanted, captured);
            }
            if scan.lit(b"<RehydratePriority>") {
                return known(scan, RehydratePriority, wanted, captured);
            }
            Ok(false)
        }
        b'S' => {
            if scan.lit(b"<ServerEncrypted>") {
                return known(scan, ServerEncrypted, wanted, captured);
            }
            if scan.lit(b"<Snapshot>") {
                return known(scan, Snapshot, wanted, captured);
            }
            Ok(false)
        }
        b'T' => {
            if scan.lit(b"<TagCount>") {
                return known(scan, TagCount, wanted, captured);
            }
            Ok(false)
        }
        b'V' => {
            if scan.lit(b"<VersionId>") {
                return known(scan, VersionId, wanted, captured);
            }
            Ok(false)
        }
        b'x' => {
            if scan.lit(b"<x-ms-blob-sequence-number>") {
                return known(scan, BlobSequenceNumber, wanted, captured);
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

// Reads a group of keys. `<BlobPrefix>` has been consumed. A
// hierarchical-namespace account writes a `<Properties>` element here too. It
// is skipped, because a group has no size and no entity tag.
fn read_blob_prefix(scan: &mut Scan<'_>) -> Result<Fields> {
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
// tag and date in place. Every range below is a span the scanner recorded on
// the chunk, or the part of one that decoding kept, which is never longer.
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

// Reads the children of the root element until it ends. The scan stands
// where the root's first child begins. The entries are read by
// `read_entries_into` when their element is opened.
fn read_root_children_into<'b>(
    mut scan: Scan<'b>,
    room: usize,
    wanted: PropertySet,
    sink: &mut Sink<'_, 'b>,
) -> Result<Listing<'b>> {
    // Set once the entries element has been read.
    let mut entries: Option<Entries> = None;
    let mut seen_marker = false;
    let mut next_marker: Option<&'b str> = None;

    loop {
        // Drop what was read before this child, so that the child begins at
        // offset zero.
        scan.skip_space();
        scan.take();

        match read_root_child(&mut scan)? {
            Item::Blobs { empty } => {
                // A second entries element is refused the way a second name
                // is. Azure writes one; this is for consistency, not for a
                // case seen.
                if entries.is_some() {
                    return fault();
                }
                entries = Some(if empty {
                    Entries { held: 0, built: 0 }
                } else {
                    read_entries_into(&mut scan, room, wanted, sink)?
                });
            }
            Item::Marker((span, flags)) => {
                // The same for a second marker.
                if seen_marker {
                    return fault();
                }
                seen_marker = true;
                let chunk = scan.take();
                let (start, stop) = trim(chunk, span);
                // `decode` returns at most the length it was given.
                let len = decode(&mut chunk[start..stop], flags, false)?;
                let chunk: &'b [u8] = chunk;
                // An empty marker means the listing is complete, so it is
                // reported as no next page rather than an empty one.
                next_marker = Some(&chunk[start..start + len])
                    .filter(|marker| !marker.is_empty())
                    .map(text)
                    .transpose()?;
            }
            Item::Skip => {}
            Item::End => {
                // A document with no entries element is not a page.
                let Some(entries) = entries else {
                    return fault();
                };
                if entries.held > room {
                    return Err(Error::Capacity(CapacityError {
                        required: entries.held,
                        available: room,
                    }));
                }
                return Ok(Listing {
                    filled: entries.built,
                    next_marker,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{BlobProperty, PropertySet, Scan, read_known_element};

    // The properties whose empty spelling the match tries.
    const WRITTEN_EMPTY: [BlobProperty; 7] = [
        BlobProperty::ContentType,
        BlobProperty::ContentEncoding,
        BlobProperty::ContentLanguage,
        BlobProperty::ContentCrc64,
        BlobProperty::ContentMd5,
        BlobProperty::CacheControl,
        BlobProperty::ContentDisposition,
    ];

    // Feeds one start tag to the whole-tag match and returns whether it was
    // taken, with the span it recorded.
    fn matched(document: &str, property: BlobProperty) -> (bool, Option<(usize, usize)>) {
        let wanted = PropertySet::of(&[property]);
        let mut captured = [None; 1];
        let mut bytes = std::vec::Vec::from(document.as_bytes());
        let mut scan = Scan::new(&mut bytes);
        let taken = read_known_element(&mut scan, wanted, &mut captured).unwrap();
        (taken, captured[0])
    }

    /// The match is written by hand, so this checks it against the enum:
    /// every property is matched whole as the service spells it, and its
    /// value is the text between the tags.
    #[test]
    fn every_property_is_matched_whole_as_the_service_spells_it() {
        for property in BlobProperty::ALL {
            let name = property.name();
            let document = std::format!("<{name}>x</{name}>");
            let start = name.len() + 2;
            assert_eq!(
                matched(&document, *property),
                (true, Some((start, start + 1))),
                "{name}"
            );
        }
    }

    /// The empty spelling is matched for the properties the service writes
    /// that way, and recorded as an empty value.
    #[test]
    fn the_empty_spelling_is_matched_where_the_service_writes_it() {
        for property in WRITTEN_EMPTY {
            let document = std::format!("<{} />", property.name());
            assert_eq!(matched(&document, property), (true, Some((0, 0))));
        }
        // Any other tag is left for the general path.
        assert_eq!(
            matched("<Metadata>", BlobProperty::AccessTier),
            (false, None)
        );
        assert_eq!(
            matched("<AccessTier >", BlobProperty::AccessTier),
            (false, None)
        );
    }
}
