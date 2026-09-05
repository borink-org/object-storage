//! The responses the corpus holds, and the objects each one needs.
//!
//! Read this file as the index of the corpus: every recorded response is one
//! `session.capture` call, and above it stands the state it was recorded
//! against. A test that reads a file finds here what produced it.

use borink_object_storage_proto::{
    ConditionKind, DeleteKind, GetKind, ListEntry, Payload, PhysicalDelete, PhysicalGet,
    PhysicalList, PhysicalPut, RequestedRange, layered,
};
use test_support::base64;

use crate::wire::Request;
use crate::{Account, PREFIX, Session, encoded, now};

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// Records every group.
pub fn record(session: &mut Session, flat: &Account, hierarchical: &Account) -> Fallible {
    listing(session, flat, hierarchical)?;
    reads(session, flat)?;
    writes(session, flat)?;
    removals(session, flat)?;
    multipart(session, flat, hierarchical)
}

// ---------------------------------------------------------------- listings

fn listing(session: &mut Session, flat: &Account, hierarchical: &Account) -> Fallible {
    session.group(
        "azure-listing",
        "Recorded listing responses",
        "The pages that `crates/object-storage-proto/tests/azure_list.rs` reads. Between them \
         they hold every shape the listing reader claims to read: a group of keys, a directory, \
         a name the service encoded, a page that names a next one, and a page that holds \
         nothing.",
    );

    // A board of three objects, whose names put the group of keys between two
    // objects: `a.txt`, `nested/`, `z.txt`. A delimited listing of it
    // interleaves the two kinds of entry, and an undelimited one does not.
    let board = format!("{PREFIX}board/");
    session.seed(flat, &format!("{board}a.txt"), b"12345678")?;
    session.seed(flat, &format!("{board}nested/c.txt"), b"c")?;
    session.seed(flat, &format!("{board}z.txt"), b"zz")?;
    session.seed(hierarchical, &format!("{board}a.txt"), b"12345678")?;
    session.seed(hierarchical, &format!("{board}nested/c.txt"), b"c")?;
    session.seed(hierarchical, &format!("{board}z.txt"), b"zz")?;

    session.capture(
        flat,
        "list-page",
        "a page of objects: each `<Blob>` states a length, an entity tag and a last-modified, \
         and the account's versioning writes `<VersionId>` and `<IsCurrentVersion>` beside the \
         properties rather than inside them",
        list(flat, &PhysicalList::new(&board))?,
    )?;

    session.capture(
        flat,
        "list-delimited",
        "a delimited page: the group of keys stands between the two objects, in name order, and \
         on a flat account a `<BlobPrefix>` holds a `<Name>` and nothing else",
        list(
            flat,
            &PhysicalList {
                delimited: true,
                ..PhysicalList::new(&board)
            },
        )?,
    )?;

    session.capture(
        hierarchical,
        "list-delimited-hierarchical",
        "the same delimited page from an account with a hierarchical namespace: a `<BlobPrefix>` \
         also holds a `<Properties>` block, with the directory's own creation time, \
         last-modified, entity tag, `<ResourceType>directory</ResourceType>` and a zero length",
        list(
            hierarchical,
            &PhysicalList {
                delimited: true,
                ..PhysicalList::new(&board)
            },
        )?,
    )?;

    session.capture(
        hierarchical,
        "list-hierarchical-directory",
        "an undelimited page from the same account: the directory is a `<Blob>` of its own, with \
         `<ResourceType>directory</ResourceType>`, no trailing separator and a zero length, which \
         the reader reports as a directory rather than as an object",
        list(hierarchical, &PhysicalList::new(&board))?,
    )?;

    // The same board, two objects at a time. The first page names where the
    // second starts, and the second names no third.
    let first = session.capture(
        flat,
        "list-first-page",
        "the first of two pages: `<NextMarker>` carries the service's own opaque text, which \
         names where the next page starts",
        list(
            flat,
            &PhysicalList {
                max_results: Some(2),
                ..PhysicalList::new(&board)
            },
        )?,
    )?;

    let mut body = first.body.clone();
    let blobs = flat.blobs();
    let mut entries = vec![ListEntry::default(); 8];
    let marker = blobs
        .fill_listing(&mut body, &mut entries)?
        .next_marker
        .ok_or("the first page named no next page")?
        .to_owned();
    session.capture(
        flat,
        "list-next-page",
        "the page that marker named: the last page of a listing writes `<NextMarker />`, the \
         empty element with a space before the slash",
        list(
            flat,
            &PhysicalList {
                max_results: Some(2),
                marker: Some(&marker),
                ..PhysicalList::new(&board)
            },
        )?,
    )?;

    session.capture(
        flat,
        "list-empty",
        "a prefix that holds nothing: `<Blobs />` and `<NextMarker />`, a page with no entry and \
         no next page",
        list(flat, &PhysicalList::new(&format!("{PREFIX}nothing/")))?,
    )?;

    // Names that XML cannot carry as they stand. The first two are escaped in
    // the document; the third holds a character XML has no way to write, so
    // the service encodes the whole name and says so.
    let names = format!("{PREFIX}names/");
    session.seed(flat, &format!("{names}a&b.txt"), b"1")?;
    session.seed(flat, &format!("{names}café.txt"), b"2")?;
    session.seed(flat, &format!("{names}100%-\u{fffe}-name.txt"), b"3")?;
    session.capture(
        flat,
        "list-encoded-names",
        "three names the document cannot carry as they stand: `&` written `&amp;`, a name written \
         in UTF-8, and a name holding a character XML cannot write at all, which the service \
         percent-encodes whole, separators included, under `Encoded=\"true\"`",
        list(flat, &PhysicalList::new(&names))?,
    )?;

    // Keys that lean on the separator. The flat account stores each of these
    // under the name it was given, so a page carrying them is the other half
    // of the round trip that the live suite starts.
    let separators = format!("{PREFIX}separators/");
    for key in [
        "trailing/",
        "double//slash",
        "space /x",
        "a.b/c",
        "..leading",
    ] {
        session.seed(flat, &format!("{separators}{key}"), b"1")?;
    }
    session.capture(
        flat,
        "list-separator-keys",
        "a trailing separator, a doubled one, a space before one, a dot inside a segment and \
         leading dots: the flat account keeps each name as it was given, and the page reports it \
         whole",
        list(flat, &PhysicalList::new(&separators))?,
    )?;

    // One key of as many segments as the flat account takes: 255, of which the
    // prefix above spends two.
    let segments = format!("{PREFIX}{}", vec!["s"; 253].join("/"));
    session.seed(flat, &segments, b"1")?;
    session.capture(
        flat,
        "list-many-segments",
        "one key of 255 path segments, the most the flat account takes: a page carrying it is a \
         page of one entry like any other",
        list(flat, &PhysicalList::new(&format!("{PREFIX}s/")))?,
    )?;

    // Every property the service writes for one object, including the ones
    // this crate reads nothing from, and metadata, which a listing writes only
    // when the request asks for it.
    let furnished = format!("{PREFIX}furnished/");
    session.seed_with(
        flat,
        &format!("{furnished}a.txt"),
        b"1234",
        &[
            ("content-type", "text/plain"),
            ("content-encoding", "gzip"),
            ("content-disposition", "inline"),
            ("cache-control", "max-age=60"),
            ("x-ms-meta-colour", "a&b"),
        ],
    )?;
    session.capture(
        flat,
        "list-furnished",
        "one object with every property the service writes for it, and its metadata, which a \
         listing writes only when the request asks for it: an element that carries nothing, one \
         that carries other elements, and values beside the properties element as well as inside \
         it",
        flat.raw(
            "GET",
            format!(
                "{}/{}?restype=container&comp=list&prefix={}&include=metadata",
                flat.endpoint,
                flat.container,
                test_support::percent_encode(&furnished)
            ),
        ),
    )?;

    session.capture(
        flat,
        "list-container-missing",
        "a listing of a container that is not there: `404 ContainerNotFound`, the one thing a \
         listing does not find. The recording identity reads the whole blob service, so the \
         service answers what it found rather than refusing the request first",
        list(
            &flat.in_container("no-such-container"),
            &PhysicalList::new(""),
        )?,
    )?;

    session.capture(
        flat,
        "list-unauthenticated",
        "a listing whose token is not a token at all: `401 InvalidAuthenticationInfo`, which is \
         not the same answer as a token the service accepts and an identity it refuses",
        list(&flat.unauthorized(), &PhysicalList::new(PREFIX))?,
    )?;

    Ok(())
}

// ------------------------------------------------------------------- reads

/// The object every read is recorded against: thirty bytes, so a range can ask
/// for part of it and a range past its end can be clamped to it.
const READ_CONTENT: &[u8] = b"0123456789-azure-record-object";

fn reads(session: &mut Session, flat: &Account) -> Fallible {
    session.group(
        "azure-get",
        "Recorded read responses",
        "The heads that `crates/object-storage-proto/tests/azure_responses.rs` reads: a whole \
         object, a range, a range past the end of one, a condition that held and one that did \
         not, and the statuses a read answers with when there is nothing to return.",
    );

    let key = format!("{PREFIX}read/object.txt");
    let stored = session.seed_with(
        flat,
        &key,
        READ_CONTENT,
        &[("content-type", "text/plain"), ("content-encoding", "gzip")],
    )?;
    let e_tag = String::from_utf8(
        stored
            .header("etag")
            .ok_or("the write named no entity tag")?
            .to_vec(),
    )?;

    session.capture(
        flat,
        "get-whole",
        "a whole read: the length, the entity tag, the last-modified and the encoding the object \
         is stored under, which the reader carries rather than decodes",
        get(flat, &PhysicalGet::new(&key))?,
    )?;

    session.capture(
        flat,
        "head-metadata",
        "the same object asked for by its metadata alone: the head of a read, with no body to \
         follow it",
        get(
            flat,
            &PhysicalGet {
                kind: GetKind::Metadata,
                ..PhysicalGet::new(&key)
            },
        )?,
    )?;

    session.capture(
        flat,
        "get-range",
        "a bounded range: `Content-Range` states the bytes returned and the size of the object \
         they came from",
        get(
            flat,
            &PhysicalGet {
                range: RequestedRange::Bounded { start: 2, end: 6 },
                ..PhysicalGet::new(&key)
            },
        )?,
    )?;

    session.capture(
        flat,
        "get-range-past-the-end",
        "a range whose end is past the end of the object: the service answers with every byte it \
         has from the start of the range, which is maximal satisfaction of the request",
        get(
            flat,
            &PhysicalGet {
                range: RequestedRange::Bounded { start: 28, end: 64 },
                ..PhysicalGet::new(&key)
            },
        )?,
    )?;

    session.capture(
        flat,
        "get-range-not-satisfiable",
        "a range that starts past the end: `416`, with `bytes */N` naming the size of the \
         object, which is the only place that size is stated",
        get(
            flat,
            &PhysicalGet {
                range: RequestedRange::Offset(40),
                ..PhysicalGet::new(&key)
            },
        )?,
    )?;

    session.capture(
        flat,
        "get-not-modified",
        "a read under `If-None-Match` with the object's own entity tag: `304`, with the tag and \
         no body",
        get(
            flat,
            &PhysicalGet {
                condition: ConditionKind::IfNoneMatch,
                condition_value: Some(e_tag.as_bytes()),
                ..PhysicalGet::new(&key)
            },
        )?,
    )?;

    session.capture(
        flat,
        "get-precondition-failed",
        "a read under `If-Match` with an entity tag the object does not have: `412`, which names \
         no error code",
        get(
            flat,
            &PhysicalGet {
                condition: ConditionKind::IfMatch,
                condition_value: Some(b"\"0x8DF0000000000000\""),
                ..PhysicalGet::new(&key)
            },
        )?,
    )?;

    session.capture(
        flat,
        "get-missing",
        "a read of a key that holds nothing: `404 BlobNotFound`, named in the head and repeated \
         in the body",
        get(flat, &PhysicalGet::new(&format!("{PREFIX}read/absent.txt")))?,
    )?;

    session.capture(
        flat,
        "get-container-missing",
        "a read addressed to a container that is not there: `404 ContainerNotFound`, which names \
         the container rather than the object, and is a different outcome from a key that holds \
         nothing",
        get(
            &flat.in_container("no-such-container"),
            &PhysicalGet::new("object.txt"),
        )?,
    )?;

    session.capture(
        flat,
        "get-unauthenticated",
        "a read whose token is not a token at all: `401 InvalidAuthenticationInfo`, which is not \
         the same answer as a token the service accepts and an identity it refuses",
        get(&flat.unauthorized(), &PhysicalGet::new(&key))?,
    )?;

    Ok(())
}

// ------------------------------------------------------------------ writes

fn writes(session: &mut Session, flat: &Account) -> Fallible {
    session.group(
        "azure-put",
        "Recorded write responses",
        "The heads that `crates/object-storage-proto/tests/azure_put.rs` reads: a write that \
         stored the object, a write that lost the race to create it, and a write whose condition \
         did not hold.",
    );

    let key = format!("{PREFIX}write/object.txt");
    session.capture(
        flat,
        "put-created",
        "a stored object: `201`, with the entity tag and last-modified it now has, and, on an \
         account that keeps versions, the version this write made",
        put(flat, &PhysicalPut::new(&key), b"0123456789")?,
    )?;

    session.capture(
        flat,
        "put-created-empty",
        "an object of no bytes, which is an object: the same `201`, under a stated length of \
         zero",
        put(
            flat,
            &PhysicalPut::new(&format!("{PREFIX}write/empty.bin")),
            b"",
        )?,
    )?;

    session.capture(
        flat,
        "put-lost-the-race-to-create",
        "a write under `If-None-Match: *` to a key that already holds something: `409 \
         BlobAlreadyExists`",
        put(
            flat,
            &PhysicalPut {
                condition: ConditionKind::IfNoneMatch,
                condition_value: Some(b"*"),
                ..PhysicalPut::new(&key)
            },
            b"another object",
        )?,
    )?;

    session.capture(
        flat,
        "put-precondition-failed",
        "a write under `If-Match` with an entity tag the object does not have: `412 \
         ConditionNotMet`",
        put(
            flat,
            &PhysicalPut {
                condition: ConditionKind::IfMatch,
                condition_value: Some(b"\"0x8DF0000000000000\""),
                ..PhysicalPut::new(&key)
            },
            b"another object",
        )?,
    )?;

    // The same write, under each of the two identities. Azure settles the
    // grant before it looks for the container, so what comes back says which
    // grant the sender holds and not what a write to a missing container is.
    session.capture(
        &flat.account_scoped(),
        "put-container-missing",
        "a write addressed to a container that is not there, by an identity that may write \
         anywhere in the account: `404 ContainerNotFound`. The grant is settled first and this \
         one covers the container, so the service goes on to look for it",
        put(
            &flat.account_scoped().in_container("no-such-container"),
            &PhysicalPut::new("object.txt"),
            b"nothing is stored",
        )?,
    )?;

    session.capture(
        flat,
        "put-refused",
        "a write the identity is not allowed to make: `403 AuthorizationPermissionMismatch`. Its \
         writing role covers one container and this request names another, and the service \
         settles that before it looks for the container, so the answer says nothing about \
         whether the container is there. A read of the very same container does say",
        put(
            &flat.in_container("no-such-container"),
            &PhysicalPut::new("object.txt"),
            b"nothing is stored",
        )?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------- removals

fn removals(session: &mut Session, flat: &Account) -> Fallible {
    session.group(
        "azure-delete",
        "Recorded removal responses",
        "The heads that `crates/object-storage-proto/tests/azure_delete.rs` reads: a removal the \
         service accepted, one of a key that held nothing, one whose condition did not hold, and \
         one the service refused because the object had snapshots that the plan did not name.",
    );

    let key = format!("{PREFIX}remove/object.txt");
    session.seed(flat, &key, b"1")?;
    session.capture(
        flat,
        "delete-accepted",
        "an accepted removal: `202`, and no metadata, because the object is gone",
        delete(flat, &PhysicalDelete::new(&key))?,
    )?;

    session.capture(
        flat,
        "delete-missing",
        "a removal of a key that holds nothing: `404 BlobNotFound`, an outcome rather than a \
         fault",
        delete(flat, &PhysicalDelete::new(&key))?,
    )?;

    session.seed(flat, &key, b"1")?;
    session.capture(
        flat,
        "delete-precondition-failed",
        "a removal under `If-Match` with an entity tag the object does not have: `412 \
         ConditionNotMet`",
        delete(
            flat,
            &PhysicalDelete {
                condition: ConditionKind::IfMatch,
                condition_value: Some(b"\"0x8DF0000000000000\""),
                ..PhysicalDelete::new(&key)
            },
        )?,
    )?;

    // A snapshot of the object, so that a removal naming the object alone has
    // something to refuse.
    let snapshot = session.send(flat.raw("PUT", format!("{}?comp=snapshot", flat.url(&key))))?;
    if snapshot.status != 201 {
        return Err(format!("the snapshot was not taken: {}", snapshot.status_line).into());
    }
    session.capture(
        flat,
        "delete-refused-for-snapshots",
        "a removal naming the object alone, of an object that has snapshots: `409 \
         SnapshotsPresent`. A plan that does not say what it takes with it does not take them",
        delete(flat, &PhysicalDelete::new(&key))?,
    )?;
    session.capture(
        flat,
        "delete-accepted-with-snapshots",
        "the same removal, naming the snapshots as well: `202`, the same answer as any other \
         accepted removal",
        delete(
            flat,
            &PhysicalDelete {
                kind: DeleteKind::ObjectAndSnapshots,
                ..PhysicalDelete::new(&key)
            },
        )?,
    )?;

    session.capture(
        &flat.account_scoped(),
        "delete-container-missing",
        "a removal addressed to a container that is not there, by an identity that may write \
         anywhere in the account: `404 ContainerNotFound`, the same answer as the write and the \
         read of that container",
        delete(
            &flat.account_scoped().in_container("no-such-container"),
            &PhysicalDelete::new("object.txt"),
        )?,
    )?;

    session.capture(
        flat,
        "delete-refused",
        "a removal the identity is not allowed to make: `403 AuthorizationPermissionMismatch`. A \
         removal needs the writing role, which covers one container here, so it is refused where \
         a read of the same container answers `404 ContainerNotFound` under the wider reading \
         role",
        delete(
            &flat.in_container("no-such-container"),
            &PhysicalDelete::new("object.txt"),
        )?,
    )?;

    Ok(())
}

// --------------------------------------------------------------- multipart

// A block identifier is base64, and every identifier of one blob must decode
// to the same length. These are `block-0`, `block-1` and `block-2`.
fn block_id(index: u8) -> String {
    base64(format!("block-{index}").as_bytes())
}

fn multipart(session: &mut Session, flat: &Account, hierarchical: &Account) -> Fallible {
    session.group(
        "azure-multipart",
        "Recorded multipart responses",
        "`Put Block`, `Put Block List` and `Get Block List`, which this crate does not support \
         yet. Nothing reads these files. They are here so that the multipart types can be \
         written against what Azure actually sent.",
    );

    let key = format!("{PREFIX}multipart/object.bin");
    let url = flat.url(&key);
    let block = |id: &str| {
        format!(
            "{url}?comp=block&blockid={}",
            test_support::percent_encode(id)
        )
    };
    let list_url = format!("{url}?comp=blocklist");

    // A run that died here left blocks staged against this key, which no
    // listing shows and emptying the prefix cannot reach. A whole-object write
    // discards them, and removing what it wrote leaves the key holding nothing.
    session.seed(flat, &key, b"whole")?;
    session.send(flat.raw("DELETE", url.clone()))?;

    // Two blocks, staged in the order that is not their identifier order, so
    // that the listing shows which of the two orders it reports.
    session.capture(
        flat,
        "put-block-201",
        "a staged block: a CRC64 of what was staged, and no entity tag, no last-modified and no \
         MD5, because staging a block changes no object",
        flat.raw("PUT", block(&block_id(1)))
            .body(b"second".to_vec()),
    )?;
    session.send(flat.raw("PUT", block(&block_id(0))).body(b"first".to_vec()))?;

    session.capture(
        flat,
        "put-block-empty-400",
        "an empty block: refused, and the refusal names the header that states the length",
        flat.raw("PUT", block(&block_id(2))).body(Vec::new()),
    )?;

    session.capture(
        flat,
        "put-block-mixed-length-400",
        "an identifier that decodes to another length than the ones already staged: `400 \
         InvalidBlobOrBlock`, at staging time rather than at commit time",
        flat.raw("PUT", block(&base64(b"a-much-longer-block-identifier")))
            .body(b"third".to_vec()),
    )?;

    session.capture(
        flat,
        "get-block-list-uncommitted-only",
        "both sections of a key whose blocks are all staged: the empty one written \
         `<CommittedBlocks />`, and the blocks ordered by identifier rather than by when they \
         were staged",
        flat.raw("GET", format!("{list_url}&blocklisttype=all")),
    )?;

    session.capture(
        flat,
        "get-block-list-committed-empty",
        "the committed listing of that same key: `200` with an empty section and no entity tag, \
         not the `404` that a key holding nothing answers",
        flat.raw("GET", format!("{list_url}&blocklisttype=committed")),
    )?;

    session.capture(
        flat,
        "put-block-list-unstaged-400",
        "a commit naming a block nobody staged: `400 InvalidBlockList`",
        flat.raw("PUT", list_url.clone())
            .body(block_list(&[&block_id(0), &base64(b"block-9")])),
    )?;

    session.capture(
        flat,
        "put-block-list-201",
        "the commit: it answers like a whole-object write, with an entity tag, a last-modified, \
         a CRC64 and the version it made",
        flat.raw("PUT", list_url.clone())
            .body(block_list(&[&block_id(0), &block_id(1)])),
    )?;

    session.capture(
        flat,
        "get-block-list-after-the-commit",
        "the committed listing after that commit: it describes the object as well, with an \
         entity tag, a last-modified and `x-ms-blob-content-length`",
        flat.raw("GET", format!("{list_url}&blocklisttype=all")),
    )?;

    session.capture(
        flat,
        "put-block-list-lost-create-409",
        "a commit under `If-None-Match: *` to a key that already holds something: `409 \
         BlobAlreadyExists`, like a whole-object write, and not the `412` the reference states",
        flat.raw("PUT", list_url.clone())
            .header("if-none-match", "*")
            .body(block_list(&[&block_id(0)])),
    )?;

    session.capture(
        flat,
        "put-block-list-condition-412",
        "a commit under `If-Match` with an entity tag the object does not have: `412 \
         ConditionNotMet`",
        flat.raw("PUT", list_url.clone())
            .header("if-match", "\"0x8DF0000000000000\"")
            .body(block_list(&[&block_id(0)])),
    )?;

    // A whole-object write to the same key, which discards what is staged.
    session.seed(flat, &key, b"whole")?;
    session.capture(
        flat,
        "get-block-list-both-empty",
        "the listing after a whole-object write to the same key: both sections empty, because a \
         whole-object write discards what was staged",
        flat.raw("GET", format!("{list_url}&blocklisttype=all")),
    )?;

    // An identifier whose base64 spells the three characters that a URL and a
    // document each treat as their own.
    let escaped = format!("{PREFIX}multipart/escaped.bin");
    let escaped_url = flat.url(&escaped);
    let escaped_id = base64(&[0xFB, 0xFF]);
    // A blob whose blocks are all staged is in no listing, so emptying the
    // prefix cannot reach it. Writing the key whole discards what is staged,
    // and the write leaves a key that a listing does report.
    session.seed(flat, &escaped, b"whole")?;
    session.send(flat.raw("DELETE", escaped_url.clone()))?;
    let staged = session.send(
        flat.raw(
            "PUT",
            format!(
                "{escaped_url}?comp=block&blockid={}",
                test_support::percent_encode(&escaped_id)
            ),
        )
        .body(b"one".to_vec()),
    )?;
    if staged.status != 201 {
        return Err(format!("the escaped block was not staged: {}", staged.status_line).into());
    }
    session.capture(
        flat,
        "get-block-list-escaped-identifier",
        "an identifier holding `+`, `/` and `=`: the request escapes them and the document \
         writes them back as they are",
        flat.raw(
            "GET",
            format!("{escaped_url}?comp=blocklist&blocklisttype=all"),
        ),
    )?;
    // The same again, so that emptying the prefix removes this key too.
    session.seed(flat, &escaped, b"whole")?;

    session.capture(
        flat,
        "get-block-list-absent-404",
        "a key that holds nothing at all, staged or committed: `404 BlobNotFound`",
        flat.raw(
            "GET",
            format!(
                "{}?comp=blocklist&blocklisttype=all",
                flat.url(&format!("{PREFIX}multipart/absent.bin"))
            ),
        ),
    )?;

    let snapshot_key = format!("{PREFIX}multipart/snapshot.bin");
    session.seed(hierarchical, &snapshot_key, b"1")?;
    session.capture(
        hierarchical,
        "snapshot-hierarchical-409",
        "an account with a hierarchical namespace has no snapshots, and names the feature it \
         refuses",
        hierarchical.raw(
            "PUT",
            format!("{}?comp=snapshot", hierarchical.url(&snapshot_key)),
        ),
    )?;

    Ok(())
}

fn block_list(ids: &[&str]) -> Vec<u8> {
    let mut body = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
    for id in ids {
        body.push_str(&format!("<Latest>{id}</Latest>"));
    }
    body.push_str("</BlockList>");
    body.into_bytes()
}

// ------------------------------------------------- requests the crate makes

fn list(account: &Account, list: &PhysicalList<'_>) -> Result<Request, Box<dyn std::error::Error>> {
    let blobs = account.blobs();
    let mut buf = vec![0; layered::list_requirements(&blobs, list, &now())?];
    Ok(encoded(blobs.encode_list(&mut buf, list, &now())?))
}

fn get(account: &Account, get: &PhysicalGet<'_>) -> Result<Request, Box<dyn std::error::Error>> {
    let blobs = account.blobs();
    let mut buf = vec![0; layered::get_requirements(&blobs, get, &now())?];
    Ok(encoded(blobs.encode_get(&mut buf, get, &now())?))
}

fn put(
    account: &Account,
    put: &PhysicalPut<'_>,
    content: &[u8],
) -> Result<Request, Box<dyn std::error::Error>> {
    let blobs = account.blobs();
    let payload = Payload::Slice(content);
    let mut buf = vec![0; layered::put_requirements(&blobs, put, payload, &now())?];
    Ok(encoded(blobs.encode_put(&mut buf, put, payload, &now())?))
}

fn delete(
    account: &Account,
    delete: &PhysicalDelete<'_>,
) -> Result<Request, Box<dyn std::error::Error>> {
    let blobs = account.blobs();
    let mut buf = vec![0; layered::delete_requirements(&blobs, delete, &now())?];
    Ok(encoded(blobs.encode_delete(&mut buf, delete, &now())?))
}
