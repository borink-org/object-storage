use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage_proto::{
    Blobs, ConditionKind, Container, DeleteHeadOutcome, DeleteKind, DeleteShape, EntryKind, Fill,
    GetHeadOutcome, GetKind, GetShape, ListHeadOutcome, Method, Payload, PhysicalDelete,
    PhysicalGet, PhysicalList, PhysicalPut, PutHeadOutcome, PutShape, RequestedRange, ResponseHead,
    ServiceErrorKind, Timestamps, layered,
};

// `#[ignore]` is built into Rust's test harness: ordinary test runs compile but
// skip these tests, while `cargo test -- --ignored` executes them.
const CONTENTS: &[u8] = b"0123456789-azure-get-reference";

#[derive(Debug)]
struct Fixture {
    endpoint: String,
    container: String,
    key: String,
    // The write tests own this key. It is never the read reference above.
    put_key: String,
    // The listing tests own everything under this prefix, and empty it before
    // each test so that what a page holds is what the test wrote.
    list_prefix: String,
    token: String,
}

impl Fixture {
    fn from_env() -> Self {
        Self {
            endpoint: env::var("AZURE_STORAGE_ENDPOINT").unwrap(),
            container: env::var("AZURE_STORAGE_CONTAINER").unwrap(),
            key: env::var("AZURE_BLOB_KEY").unwrap(),
            put_key: env::var("AZURE_PUT_KEY").unwrap(),
            list_prefix: env::var("AZURE_LIST_PREFIX").unwrap(),
            token: env::var("AZURE_STORAGE_ACCESS_TOKEN").unwrap(),
        }
    }

    fn blobs(&self) -> Blobs<'_> {
        Blobs::new(
            Container::new(&self.endpoint, &self.container).unwrap(),
            &self.token,
        )
        .unwrap()
    }
}

#[derive(Debug)]
struct ReadResult {
    outcome: Outcome,
    body: Vec<u8>,
    size: Option<u64>,
    e_tag: Option<String>,
}

// The live suite asserts on the outcome algebra, so every response Azure
// actually sends has to be a value here rather than an error.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Body,
    Complete,
    NotModified,
    PreconditionFailed,
    NotFound,
    RangeNotSatisfiable,
    ServiceFailure(u16),
}

fn read(
    fixture: &Fixture,
    shape: GetShape,
    condition_value: Option<&[u8]>,
) -> Result<ReadResult, Box<dyn std::error::Error>> {
    let now = Timestamps::from_unix(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let blobs = fixture.blobs();
    // The scheduler path: a stored shape plus the bytes it needs.
    let get = PhysicalGet::from_shape(shape, &fixture.key, condition_value);
    let mut buf = vec![0; layered::get_requirements(&blobs, &get, &now)?];
    let request = blobs.encode_get(&mut buf, &get, &now)?;
    let mut outgoing = match request.method() {
        Method::Get => ureq::get(request.url()),
        Method::Head => ureq::head(request.url()),
        method => panic!("unexpected method {method}"),
    };
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        incoming.status().as_u16(),
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    let (outcome, size, e_tag) = match blobs.accept_get_head(shape, head)? {
        GetHeadOutcome::Body { meta, .. } => (Outcome::Body, meta.size, meta.e_tag),
        GetHeadOutcome::Complete { meta } => (Outcome::Complete, meta.size, meta.e_tag),
        GetHeadOutcome::NotModified { .. } => (Outcome::NotModified, None, None),
        GetHeadOutcome::PreconditionFailed => (Outcome::PreconditionFailed, None, None),
        GetHeadOutcome::NotFound { .. } => (Outcome::NotFound, None, None),
        GetHeadOutcome::RangeNotSatisfiable { .. } => (Outcome::RangeNotSatisfiable, None, None),
        GetHeadOutcome::ServiceFailure(failure) => {
            (Outcome::ServiceFailure(failure.status), None, None)
        }
        // Azure sends `x-ms-error-code` on every failure, so a live response
        // never asks for the body. This asserts that.
        outcome => panic!("unexpected outcome {outcome:?}"),
    };
    let e_tag = e_tag.map(|value| String::from_utf8(value.to_vec()).unwrap());
    let body = if outcome == Outcome::Body {
        incoming.body_mut().read_to_vec()?
    } else {
        Vec::new()
    };
    Ok(ReadResult {
        outcome,
        body,
        size,
        e_tag,
    })
}

const METADATA: GetShape = GetShape {
    kind: GetKind::Metadata,
    range: RequestedRange::Whole,
    condition: ConditionKind::None,
};

fn conditional(condition: ConditionKind) -> GetShape {
    GetShape {
        condition,
        ..GetShape::default()
    }
}

fn e_tag(fixture: &Fixture) -> String {
    read(fixture, METADATA, None).unwrap().e_tag.unwrap()
}

#[test]
#[ignore = "requires Azure credentials"]
fn gets_the_complete_blob() {
    let result = read(&Fixture::from_env(), GetShape::default(), None).unwrap();
    assert_eq!(result.outcome, Outcome::Body);
    assert_eq!(result.body, CONTENTS);
    assert_eq!(result.size, Some(CONTENTS.len() as u64));
}

#[test]
#[ignore = "requires Azure credentials"]
fn gets_a_bounded_range() {
    let shape = GetShape {
        range: RequestedRange::Bounded { start: 2, end: 11 },
        ..GetShape::default()
    };
    let result = read(&Fixture::from_env(), shape, None).unwrap();
    assert_eq!(result.outcome, Outcome::Body);
    assert_eq!(result.body, &CONTENTS[2..11]);
    assert_eq!(result.size, Some(CONTENTS.len() as u64));
}

#[test]
#[ignore = "requires Azure credentials"]
fn heads_the_blob() {
    let result = read(&Fixture::from_env(), METADATA, None).unwrap();
    assert_eq!(result.outcome, Outcome::Complete);
    assert!(result.body.is_empty());
    assert_eq!(result.size, Some(CONTENTS.len() as u64));
    assert!(result.e_tag.is_some());
}

#[test]
#[ignore = "requires Azure credentials"]
fn applies_if_match() {
    let fixture = Fixture::from_env();
    let e_tag = e_tag(&fixture);
    let shape = conditional(ConditionKind::IfMatch);
    assert_eq!(
        read(&fixture, shape, Some(e_tag.as_bytes())).unwrap().body,
        CONTENTS
    );
    assert_eq!(
        read(&fixture, shape, Some(b"\"stale\"")).unwrap().outcome,
        Outcome::PreconditionFailed
    );
}

#[test]
#[ignore = "requires Azure credentials"]
fn applies_if_none_match() {
    let fixture = Fixture::from_env();
    let e_tag = e_tag(&fixture);
    let shape = conditional(ConditionKind::IfNoneMatch);
    assert_eq!(
        read(&fixture, shape, Some(e_tag.as_bytes()))
            .unwrap()
            .outcome,
        Outcome::NotModified
    );
    assert_eq!(
        read(&fixture, shape, Some(b"\"stale\"")).unwrap().body,
        CONTENTS
    );
}

// The write half of the suite. These tests own `AZURE_PUT_KEY` and overwrite
// it, so they never touch the blob the read tests assert on.

#[derive(Debug, PartialEq, Eq)]
struct WriteResult {
    outcome: WriteOutcome,
    // Azure states its error code in a header on every failure this crate has
    // seen. A live response that asks for the body would disprove that, so the
    // tests assert on this rather than letting it pass unnoticed.
    head_was_decisive: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum WriteOutcome {
    Created,
    PreconditionFailed,
    NotFound(Option<ServiceErrorKind>),
    ServiceFailure(u16, Option<ServiceErrorKind>),
}

fn write(
    fixture: &Fixture,
    shape: PutShape,
    condition_value: Option<&[u8]>,
    content: &[u8],
) -> Result<WriteResult, Box<dyn std::error::Error>> {
    write_as(fixture, shape, condition_value, content, false)
}

// `stream` describes the same content as `Payload::Streamed`, which states the
// length without lending the bytes. The request head must be identical either
// way, and the host sends the same bytes.
fn write_as(
    fixture: &Fixture,
    shape: PutShape,
    condition_value: Option<&[u8]>,
    content: &[u8],
    stream: bool,
) -> Result<WriteResult, Box<dyn std::error::Error>> {
    let now = Timestamps::from_unix(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let blobs = fixture.blobs();
    let put = PhysicalPut::from_shape(shape, &fixture.put_key, condition_value);
    let described = if stream {
        Payload::Streamed {
            len: content.len() as u64,
        }
    } else {
        Payload::Slice(content)
    };
    let mut buf = vec![0; layered::put_requirements(&blobs, &put, described, &now)?];
    let request = blobs.encode_put(&mut buf, &put, described, &now)?;
    let mut outgoing = ureq::put(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        // A streamed payload carries no bytes, so the host sends its own.
        .send(request.payload().bytes().unwrap_or(content))?;
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        incoming.status().as_u16(),
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    let outcome = blobs.accept_put_head(shape, head)?;
    let head_was_decisive = !matches!(outcome, PutHeadOutcome::NeedErrorBody(_));
    // Read the body even when the head decided, so a run that disproves the
    // header claim still reports which error Azure named.
    let body = incoming.body_mut().read_to_vec().unwrap_or_default();
    let finished = match outcome {
        PutHeadOutcome::NeedErrorBody(failure) => {
            blobs.accept_put_error_body(failure.status, failure.request_id, &body)
        }
        outcome => outcome,
    };
    let outcome = match finished {
        PutHeadOutcome::Created { .. } => WriteOutcome::Created,
        PutHeadOutcome::PreconditionFailed => WriteOutcome::PreconditionFailed,
        PutHeadOutcome::NotFound { kind } => WriteOutcome::NotFound(kind),
        PutHeadOutcome::ServiceFailure(failure) => {
            WriteOutcome::ServiceFailure(failure.status, failure.kind)
        }
        outcome => panic!("unresolved outcome {outcome:?}"),
    };
    Ok(WriteResult {
        outcome,
        head_was_decisive,
    })
}

fn conditional_write(condition: ConditionKind) -> PutShape {
    PutShape { condition }
}

// Seeds the write key so the object exists, and returns its entity tag.
fn seed(fixture: &Fixture, content: &[u8]) -> String {
    assert_eq!(
        write(fixture, PutShape::default(), None, content)
            .unwrap()
            .outcome,
        WriteOutcome::Created
    );
    let shape = GetShape {
        kind: GetKind::Metadata,
        ..GetShape::default()
    };
    read_put_key(fixture, shape).e_tag.unwrap()
}

// The read helper reads `AZURE_BLOB_KEY`; the write tests need the same reads
// against the key they own.
fn read_put_key(fixture: &Fixture, shape: GetShape) -> ReadResult {
    let swapped = Fixture {
        endpoint: fixture.endpoint.clone(),
        container: fixture.container.clone(),
        key: fixture.put_key.clone(),
        put_key: fixture.put_key.clone(),
        list_prefix: fixture.list_prefix.clone(),
        token: fixture.token.clone(),
    };
    read(&swapped, shape, None).unwrap()
}

#[test]
#[ignore = "requires Azure credentials"]
fn writes_a_whole_object_and_reads_it_back() {
    let fixture = Fixture::from_env();
    let content = b"0123456789-azure-put-reference";
    seed(&fixture, content);

    let result = read_put_key(&fixture, GetShape::default());
    assert_eq!(result.outcome, Outcome::Body);
    assert_eq!(result.body, content);
    assert_eq!(result.size, Some(content.len() as u64));
}

/// Settles how Azure refuses a write whose object already exists.
///
/// The Azure documentation gets this wrong, which is why the test exists. Its
/// write-operations table gives 412 for an unmet `If-None-Match`, and the
/// `Put Blob` page names no failure status at all. The service answers 409
/// with `BlobAlreadyExists`, so a caller who followed the documentation would
/// branch on the wrong outcome.
#[test]
#[ignore = "requires Azure credentials"]
fn a_lost_race_to_create_is_a_conflict_that_names_the_object() {
    let fixture = Fixture::from_env();
    seed(&fixture, b"the object that is already there");

    let result = write(
        &fixture,
        conditional_write(ConditionKind::IfNoneMatch),
        Some(b"*"),
        b"the write that must lose",
    )
    .unwrap();

    assert_eq!(
        result.outcome,
        WriteOutcome::ServiceFailure(409, Some(ServiceErrorKind::AlreadyExists)),
        "a lost create is 409 BlobAlreadyExists, not a precondition failure; \
         if this fails, correct the documentation on PutHeadOutcome"
    );
    assert!(
        result.head_was_decisive,
        "x-ms-error-code named the conflict"
    );

    // The write that lost changed nothing.
    assert_eq!(
        read_put_key(&fixture, GetShape::default()).body,
        b"the object that is already there"
    );
}

/// The other conditional refusal, which the one above must not be confused
/// with: a stale entity tag is a precondition failure, and carries no error
/// kind of its own.
#[test]
#[ignore = "requires Azure credentials"]
fn a_stale_entity_tag_refuses_the_write_as_a_precondition() {
    let fixture = Fixture::from_env();
    let e_tag = seed(&fixture, b"the current object");

    assert_eq!(
        write(
            &fixture,
            conditional_write(ConditionKind::IfMatch),
            Some(b"\"stale\""),
            b"the write that must lose",
        )
        .unwrap()
        .outcome,
        WriteOutcome::PreconditionFailed
    );

    // The same write against the current tag wins, which proves the refusal
    // above came from the tag and not from the condition itself.
    assert_eq!(
        write(
            &fixture,
            conditional_write(ConditionKind::IfMatch),
            Some(e_tag.as_bytes()),
            b"the write that must win",
        )
        .unwrap()
        .outcome,
        WriteOutcome::Created
    );
    assert_eq!(
        read_put_key(&fixture, GetShape::default()).body,
        b"the write that must win"
    );
}

/// A streamed payload states a length without lending the bytes, so a host can
/// write from a file or a socket. The object it stores must be identical.
#[test]
#[ignore = "requires Azure credentials"]
fn writes_streamed_content_the_same_as_borrowed_content() {
    let fixture = Fixture::from_env();
    let content = b"0123456789-azure-put-streamed";

    seed(&fixture, b"something else entirely");
    assert_eq!(
        write_as(&fixture, PutShape::default(), None, content, true)
            .unwrap()
            .outcome,
        WriteOutcome::Created
    );

    let result = read_put_key(&fixture, GetShape::default());
    assert_eq!(result.body, content);
    assert_eq!(result.size, Some(content.len() as u64));
}

// The removal half of the suite, on the same key the write tests own.

#[derive(Debug, PartialEq, Eq)]
enum RemoveOutcome {
    Accepted,
    PreconditionFailed,
    NotFound(Option<ServiceErrorKind>),
    ServiceFailure(u16, Option<ServiceErrorKind>),
}

// This crate does not implement Snapshot Blob, so the test issues that one
// request itself. The URL comes from an encoded plan, so the key is escaped
// exactly as the crate escapes it rather than by a second implementation.
fn snapshot(fixture: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    let now = Timestamps::from_unix(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let blobs = fixture.blobs();
    let plan = PhysicalDelete::new(&fixture.put_key);
    let mut buf = vec![0; layered::delete_requirements(&blobs, &plan, &now)?];
    let request = blobs.encode_delete(&mut buf, &plan, &now)?;
    let url = format!("{}?comp=snapshot", request.url());
    let mut outgoing = ureq::put(&url);
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let response = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .send(b"".as_slice())?;
    assert_eq!(response.status().as_u16(), 201, "Snapshot Blob");
    Ok(())
}

fn remove(
    fixture: &Fixture,
    shape: DeleteShape,
    condition_value: Option<&[u8]>,
) -> Result<RemoveOutcome, Box<dyn std::error::Error>> {
    let now = Timestamps::from_unix(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let blobs = fixture.blobs();
    let delete = PhysicalDelete::from_shape(shape, &fixture.put_key, condition_value);
    let mut buf = vec![0; layered::delete_requirements(&blobs, &delete, &now)?];
    let request = blobs.encode_delete(&mut buf, &delete, &now)?;
    let mut outgoing = ureq::delete(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        incoming.status().as_u16(),
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    let outcome = blobs.accept_delete_head(shape, head)?;
    let body = incoming.body_mut().read_to_vec().unwrap_or_default();
    let finished = match outcome {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            blobs.accept_delete_error_body(failure.status, failure.request_id, &body)
        }
        outcome => outcome,
    };
    Ok(match finished {
        DeleteHeadOutcome::Accepted => RemoveOutcome::Accepted,
        DeleteHeadOutcome::PreconditionFailed => RemoveOutcome::PreconditionFailed,
        DeleteHeadOutcome::NotFound { kind } => RemoveOutcome::NotFound(kind),
        DeleteHeadOutcome::ServiceFailure(failure) => {
            RemoveOutcome::ServiceFailure(failure.status, failure.kind)
        }
        outcome => panic!("unresolved outcome {outcome:?}"),
    })
}

#[test]
#[ignore = "requires Azure credentials"]
fn removes_an_object_and_then_cannot_find_it() {
    let fixture = Fixture::from_env();
    seed(&fixture, b"the object to remove");

    assert_eq!(
        remove(&fixture, DeleteShape::default(), None).unwrap(),
        RemoveOutcome::Accepted
    );
    assert_eq!(
        read_put_key(&fixture, GetShape::default()).outcome,
        Outcome::NotFound
    );
}

/// Removing what is already gone is an outcome, not an error. Only the caller
/// knows whether it meant to.
#[test]
#[ignore = "requires Azure credentials"]
fn removing_an_absent_object_reports_that_it_is_absent() {
    let fixture = Fixture::from_env();
    seed(&fixture, b"the object to remove twice");
    assert_eq!(
        remove(&fixture, DeleteShape::default(), None).unwrap(),
        RemoveOutcome::Accepted
    );
    assert_eq!(
        remove(&fixture, DeleteShape::default(), None).unwrap(),
        RemoveOutcome::NotFound(Some(ServiceErrorKind::NotFound))
    );
}

#[test]
#[ignore = "requires Azure credentials"]
fn a_stale_entity_tag_refuses_the_removal() {
    let fixture = Fixture::from_env();
    let e_tag = seed(&fixture, b"the object to remove conditionally");

    assert_eq!(
        remove(
            &fixture,
            DeleteShape {
                condition: ConditionKind::IfMatch,
                ..DeleteShape::default()
            },
            Some(b"\"stale\"")
        )
        .unwrap(),
        RemoveOutcome::PreconditionFailed
    );
    // Still there, so the refusal removed nothing.
    assert_eq!(
        read_put_key(&fixture, GetShape::default()).body,
        b"the object to remove conditionally"
    );

    assert_eq!(
        remove(
            &fixture,
            DeleteShape {
                condition: ConditionKind::IfMatch,
                ..DeleteShape::default()
            },
            Some(e_tag.as_bytes())
        )
        .unwrap(),
        RemoveOutcome::Accepted
    );
}

/// Settles what Azure does with a removal that would leave snapshots behind,
/// and proves the plan can ask for them.
#[test]
#[ignore = "requires Azure credentials"]
fn an_object_with_snapshots_is_refused_until_the_plan_asks_for_them() {
    let fixture = Fixture::from_env();
    seed(&fixture, b"the object with a snapshot");
    snapshot(&fixture).unwrap();

    // Naming the object alone leaves its snapshot behind, so Azure refuses.
    assert_eq!(
        remove(&fixture, DeleteShape::default(), None).unwrap(),
        RemoveOutcome::ServiceFailure(409, None)
    );
    assert_eq!(
        read_put_key(&fixture, GetShape::default()).outcome,
        Outcome::Body
    );

    // Asking for them removes both.
    assert_eq!(
        remove(
            &fixture,
            DeleteShape {
                kind: DeleteKind::ObjectAndSnapshots,
                ..DeleteShape::default()
            },
            None
        )
        .unwrap(),
        RemoveOutcome::Accepted
    );
    assert_eq!(
        read_put_key(&fixture, GetShape::default()).outcome,
        Outcome::NotFound
    );
}

/// `SnapshotsOnly` keeps the object, which is the one accepted removal that
/// leaves the key readable afterwards.
#[test]
#[ignore = "requires Azure credentials"]
fn removing_only_the_snapshots_keeps_the_object() {
    let fixture = Fixture::from_env();
    seed(&fixture, b"the object that outlives its snapshot");
    snapshot(&fixture).unwrap();

    assert_eq!(
        remove(
            &fixture,
            DeleteShape {
                kind: DeleteKind::SnapshotsOnly,
                ..DeleteShape::default()
            },
            None
        )
        .unwrap(),
        RemoveOutcome::Accepted
    );
    assert_eq!(
        read_put_key(&fixture, GetShape::default()).body,
        b"the object that outlives its snapshot"
    );

    // With the snapshots gone, naming the object alone now succeeds.
    assert_eq!(
        remove(&fixture, DeleteShape::default(), None).unwrap(),
        RemoveOutcome::Accepted
    );
}

// The listing half of the suite. These tests own everything under
// `AZURE_LIST_PREFIX`, and empty it first, so a page holds exactly what the
// test wrote and nothing a previous run left behind.

// One page: what it held, and where the next one starts.
type Page = (Vec<Entry>, Option<Vec<u8>>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: EntryKind,
    key: String,
    size: Option<u64>,
    e_tag: Option<String>,
    last_modified: Option<u64>,
}

// One page, read into an array of `room` entries at a time. `room` is what
// proves the resuming path against a real body: a page that does not fit is
// read in as many rounds as it takes, and no round asks the service again.
// The response body of one listing request, as it came off the wire.
fn fetch(
    fixture: &Fixture,
    plan: &PhysicalList<'_>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let now = Timestamps::from_unix(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let blobs = fixture.blobs();
    let mut buf = vec![0; layered::list_requirements(&blobs, plan, &now)?];
    let request = blobs.encode_list(&mut buf, plan, &now)?;
    assert_eq!(request.method(), Method::Get);
    let mut outgoing = ureq::get(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        incoming.status().as_u16(),
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    let expected_len = match blobs.accept_list_head(head)? {
        ListHeadOutcome::Page { expected_len } => expected_len,
        outcome => panic!("unexpected outcome {outcome:?}"),
    };
    let body = incoming.body_mut().read_to_vec()?;
    if let Some(len) = expected_len {
        assert_eq!(body.len() as u64, len, "the head sized the body");
    }
    Ok(body)
}

fn page(
    fixture: &Fixture,
    plan: &PhysicalList<'_>,
    room: usize,
) -> Result<Page, Box<dyn std::error::Error>> {
    let blobs = fixture.blobs();
    let mut body = fetch(fixture, plan)?;

    let mut entries = Vec::new();
    let mut resume = None;
    loop {
        let mut into = vec![Default::default(); room];
        let fill = match resume {
            None => blobs.fill_listing(&mut body, &mut into)?,
            Some(at) => blobs.resume_listing(&mut body, at, &mut into)?,
        };
        let (filled, done) = match fill {
            Fill::Partial { filled, resume: at } => {
                resume = Some(at);
                (filled, None)
            }
            Fill::Page(page) => (page.filled, Some(page.next_marker.map(<[u8]>::to_vec))),
        };
        entries.extend(into[..filled].iter().map(|entry| {
            Entry {
                kind: entry.kind,
                key: entry.key.to_owned(),
                size: entry.size,
                e_tag: entry
                    .e_tag
                    .map(|value| String::from_utf8(value.to_vec()).unwrap()),
                last_modified: entry.last_modified.and_then(layered::http_date_ms),
            }
        }));
        if let Some(marker) = done {
            return Ok((entries, marker));
        }
    }
}

// Every key under the prefix, page by page, the way a caller walks a listing.
fn walk(fixture: &Fixture, delimited: bool, room: usize) -> Vec<Entry> {
    let mut marker: Option<Vec<u8>> = None;
    let mut all = Vec::new();
    loop {
        let plan = PhysicalList {
            marker: marker.as_deref(),
            delimited,
            ..PhysicalList::new(&fixture.list_prefix)
        };
        let (entries, next) = page(fixture, &plan, room).unwrap();
        all.extend(entries);
        match next {
            Some(next) => marker = Some(next),
            None => return all,
        }
    }
}

// Writes one object under the listing prefix.
fn place(fixture: &Fixture, suffix: &str, content: &[u8]) {
    let owner = Fixture {
        put_key: format!("{}{suffix}", fixture.list_prefix),
        ..clone(fixture)
    };
    assert_eq!(
        write(&owner, PutShape::default(), None, content)
            .unwrap()
            .outcome,
        WriteOutcome::Created
    );
}

fn clone(fixture: &Fixture) -> Fixture {
    Fixture {
        endpoint: fixture.endpoint.clone(),
        container: fixture.container.clone(),
        key: fixture.key.clone(),
        put_key: fixture.put_key.clone(),
        list_prefix: fixture.list_prefix.clone(),
        token: fixture.token.clone(),
    }
}

// Removes whatever is under the prefix, so the tests below start from nothing.
fn empty(fixture: &Fixture) {
    for entry in walk(fixture, false, 1000) {
        let owner = Fixture {
            put_key: entry.key.clone(),
            ..clone(fixture)
        };
        assert_eq!(
            remove(&owner, DeleteShape::default(), None).unwrap(),
            RemoveOutcome::Accepted,
            "{}",
            entry.key
        );
    }
    assert_eq!(walk(fixture, false, 1000), []);
}

// The three objects that the listing tests read.
fn seed_listing(fixture: &Fixture) {
    empty(fixture);
    place(fixture, "a.txt", b"01234567");
    place(fixture, "b.txt", b"0123456789");
    place(fixture, "nested/c.txt", b"0");
}

/// The undelimited page: every key under the prefix, with what the listing
/// says about each one.
///
/// This is also what settles that the last page of a real listing is read at
/// all. Azure closes it with an empty `<NextMarker />`, written with a space
/// before the slash, and a reader that matches the tag byte for byte faults on
/// every listing that ends.
#[test]
#[ignore = "requires Azure credentials"]
fn lists_every_key_under_the_prefix() {
    let fixture = Fixture::from_env();
    seed_listing(&fixture);

    let entries = walk(&fixture, false, 1000);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.kind, entry.key.as_str(), entry.size))
            .collect::<Vec<_>>(),
        [
            (
                EntryKind::Object,
                format!("{}a.txt", fixture.list_prefix).as_str(),
                Some(8)
            ),
            (
                EntryKind::Object,
                format!("{}b.txt", fixture.list_prefix).as_str(),
                Some(10)
            ),
            (
                EntryKind::Object,
                format!("{}nested/c.txt", fixture.list_prefix).as_str(),
                Some(1)
            ),
        ]
    );
    // Every object carries the two values that a listing is read for besides
    // the key, so a caller need not head each one.
    for entry in &entries {
        assert!(entry.e_tag.is_some(), "{}", entry.key);
        assert!(entry.last_modified.is_some(), "{}", entry.key);
    }
}

/// A delimited listing walks one level at a time: the objects at this level,
/// and the groups below it reported once each.
#[test]
#[ignore = "requires Azure credentials"]
fn a_delimited_listing_reports_the_level_below_as_a_group() {
    let fixture = Fixture::from_env();
    seed_listing(&fixture);

    let entries = walk(&fixture, true, 1000);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.kind, entry.key.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                EntryKind::Object,
                format!("{}a.txt", fixture.list_prefix).as_str()
            ),
            (
                EntryKind::Object,
                format!("{}b.txt", fixture.list_prefix).as_str()
            ),
            (
                EntryKind::Prefix,
                format!("{}nested/", fixture.list_prefix).as_str()
            ),
        ]
    );
    // A group is not an object: the service states no length for it.
    assert_eq!(entries[2].size, None);
}

/// The two ways a listing is split, against the same three objects: the
/// service splitting it into pages, and the caller's array splitting each page
/// into rounds. Neither may lose a key or report one twice.
#[test]
#[ignore = "requires Azure credentials"]
fn a_listing_split_into_pages_and_into_rounds_reads_the_same_keys() {
    let fixture = Fixture::from_env();
    seed_listing(&fixture);
    let whole = walk(&fixture, false, 1000);
    assert_eq!(whole.len(), 3);

    // One entry per page, so the service names a next page twice.
    let mut marker: Option<Vec<u8>> = None;
    let mut paged = Vec::new();
    let mut pages = 0;
    loop {
        let plan = PhysicalList {
            marker: marker.as_deref(),
            max_results: Some(1),
            ..PhysicalList::new(&fixture.list_prefix)
        };
        let (entries, next) = page(&fixture, &plan, 1000).unwrap();
        pages += 1;
        paged.extend(entries);
        match next {
            Some(next) => marker = Some(next),
            None => break,
        }
        assert!(pages < 10, "a page of one is not making progress");
    }
    assert_eq!(paged, whole);
    assert!(pages >= 3, "three objects, one per page: {pages} pages");

    // One entry per round, against whole pages. The body is read once and the
    // service is asked nothing extra.
    assert_eq!(walk(&fixture, false, 1), whole);
}

/// A listing lists a container; a container that is not there is the one thing
/// it does not find.
///
/// Reaching this at all takes a grant that encloses the container being named.
/// A credential scoped to one container is refused with 403 before Azure says
/// whether any other container exists, which is deliberate: a 404 there would
/// tell an unauthorized caller which containers are real. The test principal
/// therefore holds `Storage Blob Data Reader` on `blobServices/default` rather
/// than on the one container; writes stay scoped to the container.
#[test]
#[ignore = "requires Azure credentials"]
fn listing_a_container_that_is_not_there_reports_that() {
    let fixture = Fixture::from_env();
    let absent = Fixture {
        container: format!("{}-absent", fixture.container),
        ..clone(&fixture)
    };
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let blobs = absent.blobs();
    let plan = PhysicalList::new("");
    let mut buf = vec![0; layered::list_requirements(&blobs, &plan, &now).unwrap()];
    let request = blobs.encode_list(&mut buf, &plan, &now).unwrap();
    let mut outgoing = ureq::get(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        incoming.status().as_u16(),
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    assert_eq!(
        blobs.accept_list_head(head).unwrap(),
        ListHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        },
        "a 403 here means the grant no longer encloses the container: the \
         principal needs Storage Blob Data Reader on blobServices/default"
    );
}

/// Settles what an entity tag from a listing is worth as a condition, which is
/// what [`layered::quoted_etag`] rests on.
///
/// A listing writes the tag without the quotes that the `ETag` header carries,
/// and Azure accepts it that way: the unquoted tag conditions a read exactly
/// as the quoted one does. So `quoted_etag` is not a workaround for a service
/// that refuses the listed form, and its documentation must not claim to be
/// one.
///
/// The assertion that matters is the last: an unquoted tag that does not match
/// must still refuse the read. If it does not, Azure is discarding a header it
/// cannot parse rather than reading it, a condition written that way has no
/// effect at all, and quoting becomes required rather than merely correct.
#[test]
#[ignore = "requires Azure credentials"]
fn an_entity_tag_from_a_listing_conditions_a_read_quoted_or_not() {
    let fixture = Fixture::from_env();
    seed_listing(&fixture);
    let entry = walk(&fixture, false, 1000).remove(0);
    let listed = entry.e_tag.unwrap();
    assert!(
        !listed.starts_with('"'),
        "a listing quotes nothing: {listed}"
    );

    let owner = Fixture {
        key: entry.key.clone(),
        ..clone(&fixture)
    };
    let shape = conditional(ConditionKind::IfMatch);

    let mut quoted = [0; 64];
    let quoted = layered::quoted_etag(listed.as_bytes(), &mut quoted).unwrap();
    let result = read(&owner, shape, Some(quoted)).unwrap();
    assert_eq!(result.outcome, Outcome::Body);
    assert_eq!(result.body, b"01234567");

    // Measured: Azure reads the listed tag as it stands.
    let result = read(&owner, shape, Some(listed.as_bytes())).unwrap();
    assert_eq!(
        result.outcome,
        Outcome::Body,
        "Azure accepts the tag as the listing writes it; if this ever refuses \
         the read, quoted_etag becomes required and its documentation must \
         say so"
    );
    assert_eq!(result.body, b"01234567");

    // And reads it rather than discarding it: a tag that does not match still
    // refuses, whichever form it is written in.
    for stale in [b"\"0x0\"".as_slice(), b"0x0"] {
        assert_eq!(
            read(&owner, shape, Some(stale)).unwrap().outcome,
            Outcome::PreconditionFailed,
            "an unquoted tag that does not match must still refuse the read; \
             if this returns the body, Azure is discarding the condition \
             rather than reading it, and quoted_etag is required for a \
             condition to have any effect: {}",
            String::from_utf8_lossy(stale)
        );
    }
}

/// Holds what a listed name looks like when Azure says it is encoded, which is
/// what lets `xml::decode_percent` refuse a `%` that is not an escape.
///
/// Measured. Azure refuses the C0 controls in a name outright, with 400, but
/// takes `U+FFFE` and `U+FFFF`, which XML 1.0 forbids a document to hold. It
/// lists such a name with `Encoded="true"` and encodes the whole of it, down
/// to the separators between its segments: the name below comes back reading
/// `borink-object-storage%2Fazure-list-scratch%2F100%25-%EF%BF%BE-name.txt`.
/// So every `%` in an encoded name begins an escape.
#[test]
#[ignore = "requires Azure credentials"]
fn an_encoded_name_is_encoded_whole_and_comes_back_whole() {
    let fixture = Fixture::from_env();
    empty(&fixture);

    // What XML 1.0 forbids and Azure refuses to hold at all.
    for control in ['\u{1}', '\u{B}', '\u{C}', '\u{E}'] {
        let key = format!("{}c-{control}.txt", fixture.list_prefix);
        let owner = Fixture {
            put_key: key,
            ..clone(&fixture)
        };
        assert!(
            !matches!(
                write(&owner, PutShape::default(), None, b"x")
                    .unwrap()
                    .outcome,
                WriteOutcome::Created
            ),
            "Azure took {control:?} in a name; it refused one before, and a \
             name it holds is one this test should list"
        );
    }

    // What it forbids and Azure does hold. The percent is the byte in
    // question: in an encoded name it must be written `%25`.
    let key = format!("{}100%-{}-name.txt", fixture.list_prefix, '\u{fffe}');
    let owner = Fixture {
        put_key: key.clone(),
        ..clone(&fixture)
    };
    assert_eq!(
        write(&owner, PutShape::default(), None, b"x")
            .unwrap()
            .outcome,
        WriteOutcome::Created,
        "a name holding U+FFFE was stored before"
    );

    let plan = PhysicalList::new(&fixture.list_prefix);
    let body = fetch(&fixture, &plan).unwrap();
    let xml = String::from_utf8_lossy(&body).into_owned();
    assert!(xml.contains("Encoded=\"true\""), "{xml}");
    assert!(
        xml.contains("100%25-") && xml.contains("azure-list-scratch%2F"),
        "an encoded name is encoded whole, separators included, which is what \
         makes every `%` in one an escape. If this fails, xml::decode_percent \
         must stop refusing a `%` that is not one: {xml}"
    );

    // And the name comes back as it was written.
    let listed: Vec<String> = walk(&fixture, false, 1000)
        .into_iter()
        .map(|entry| entry.key)
        .collect();
    assert_eq!(listed, [key]);

    empty(&fixture);
}

/// Holds the boundary that `validate_put` was corrected to: Azure counts a
/// key in UTF-16 code units, so a character outside the basic plane counts
/// twice.
///
/// Measured. A name of 1024 two-byte characters is 1024 code units and is
/// taken; one of 541 four-byte characters is 1041 code units and is refused
/// with 400, though it is only 541 scalar values. A byte limit would have had
/// to fall between 2007 and 2041, which no limit does.
#[test]
#[ignore = "requires Azure credentials"]
fn a_key_is_as_long_as_its_utf_16_and_this_crate_counts_the_same() {
    let fixture = Fixture::from_env();
    empty(&fixture);

    let units = |key: &str| key.encode_utf16().count();
    let prefix = units(&fixture.list_prefix);

    // Exactly the limit, spelled two ways: all two-byte characters, and
    // four-byte characters that reach it in half as many.
    let widest = format!("{}{}", fixture.list_prefix, "é".repeat(1024 - prefix));
    let deepest = format!(
        "{}a{}",
        fixture.list_prefix,
        "🦀".repeat((1023 - prefix) / 2)
    );
    for key in [&widest, &deepest] {
        assert_eq!(units(key), 1024, "{key:?}");
        let owner = Fixture {
            put_key: key.clone(),
            ..clone(&fixture)
        };
        assert_eq!(
            write(&owner, PutShape::default(), None, b"x")
                .unwrap()
                .outcome,
            WriteOutcome::Created,
            "1024 UTF-16 code units is the limit, and {} scalar values reach \
             it here",
            key.chars().count()
        );
    }

    // One code unit more, and this crate refuses it rather than the service:
    // a plan that cannot become a request is an invalid plan.
    let over = format!("{}{}", fixture.list_prefix, "🦀".repeat(1024));
    let owner = Fixture {
        put_key: over,
        ..clone(&fixture)
    };
    let refused = write(&owner, PutShape::default(), None, b"x").unwrap_err();
    assert!(
        refused.to_string().contains("key"),
        "a key past the limit is refused before it is sent: {refused}"
    );

    empty(&fixture);
}

/// Finds the largest number of `/`-delimited path segments Azure takes, which
/// the documentation gives as 254 and which measurement does not agree with.
///
/// Measured: 255 segments is taken and 494 is refused with 400, so a limit is
/// real but is not the documented one. This bisects for it and holds the
/// answer, because `addressable` has to refuse what the service will not take.
#[test]
#[ignore = "requires Azure credentials"]
fn a_key_holds_the_segments_azure_says_it_may() {
    let fixture = Fixture::from_env();
    empty(&fixture);

    // The prefix carries two segments of its own, so `total - 2` more of them
    // joined by the separator make a name of exactly `total`.
    let of_segments = |total: usize| {
        let tail = vec!["s"; total - 2].join("/");
        format!("{}{tail}", fixture.list_prefix)
    };
    let takes = |total: usize| {
        let key = of_segments(total);
        assert_eq!(key.matches('/').count() + 1, total);
        assert!(key.encode_utf16().count() <= 1024, "{total} is too long");
        let owner = Fixture {
            put_key: key,
            ..clone(&fixture)
        };
        write(&owner, PutShape::default(), None, b"x")
            .unwrap()
            .outcome
            == WriteOutcome::Created
    };

    // Between the largest that was taken and the smallest that was refused.
    let (mut taken, mut refused) = (255, 494);
    assert!(takes(taken), "{taken} segments was taken before");
    assert!(!takes(refused), "{refused} segments was refused before");
    while taken + 1 < refused {
        let mid = (taken + refused) / 2;
        if takes(mid) {
            taken = mid;
        } else {
            refused = mid;
        }
    }

    assert_eq!(
        taken, MAX_SEGMENTS,
        "the largest name Azure takes has {taken} segments, not {MAX_SEGMENTS}. \
         Correct MAX_SEGMENTS here and the segment rule in `addressable`"
    );

    empty(&fixture);
}

// Measured by the bisection above.
const MAX_SEGMENTS: usize = 256;

/// Settles what Azure does with the slashes this crate leaves literal in the
/// URL path, which is what makes a hierarchical-namespace path work.
///
/// The question is not whether Azure takes these keys but whether it stores
/// them under the name it was given. A service that quietly folded `a//b` into
/// `a/b` would leave a caller naming an object that does not exist, and a
/// listing is the only thing that shows the stored name.
///
/// Measured: a trailing separator, a doubled one, a space before one, a dot
/// inside a segment and a leading pair of them all survive. The two that did
/// not are refused by `validate_put` now and cannot reach here: `dot.` was
/// stored as `dot`, and `dots/../up` wrote `up`, because a host resolves the
/// path before it sends it.
#[test]
#[ignore = "requires Azure credentials"]
fn a_key_that_leans_on_a_slash_is_stored_under_the_name_it_was_given() {
    let fixture = Fixture::from_env();
    empty(&fixture);

    let mut created = Vec::new();
    // `dotseg./x` is the one that says whether the dot Azure drops goes from
    // the end of a name or from the end of every segment. Only the first was
    // measured, and `addressable` refuses only the first.
    for edge in [
        "trailing/",
        "double//slash",
        "space /x",
        "a.b/c",
        "..leading",
        "dotseg./x",
    ] {
        let key = format!("{}{edge}", fixture.list_prefix);
        let owner = Fixture {
            put_key: key.clone(),
            ..clone(&fixture)
        };
        // A refusal is an answer too: it says Azure will not hold the name.
        if write(&owner, PutShape::default(), None, b"x")
            .unwrap()
            .outcome
            == WriteOutcome::Created
        {
            created.push(key);
        }
    }
    assert!(!created.is_empty(), "Azure took none of the slash edges");

    let listed: Vec<String> = walk(&fixture, false, 1000)
        .into_iter()
        .map(|entry| entry.key)
        .collect();
    for key in &created {
        assert!(
            listed.contains(key),
            "{key:?} was taken and stored under another name, so `addressable` \
             has a case it does not know about. The listing reports {listed:?}"
        );
    }

    // Removing them by their listed names is the other half: a name that can
    // be listed but not addressed would be just as bad.
    empty(&fixture);
}

// Writes one object under a key already percent-encoded by the caller, and
// reports the status. `addressable` refuses the keys below, so this crate
// cannot encode them and the URL is written here, the way `snapshot` writes
// its own. The headers come from a plan this crate does encode.
fn raw_put(fixture: &Fixture, escaped: &str) -> u16 {
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let blobs = fixture.blobs();
    let plan = PhysicalPut::new(&fixture.put_key);
    let content = Payload::Slice(b"x");
    let mut buf = vec![0; layered::put_requirements(&blobs, &plan, content, &now).unwrap()];
    let request = blobs.encode_put(&mut buf, &plan, content, &now).unwrap();

    let mut outgoing = ureq::put(&format!(
        "{}/{}/{escaped}",
        fixture.endpoint, fixture.container
    ));
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .send(b"x".as_slice())
        .unwrap()
        .status()
        .as_u16()
}

// The same for the removal, so a name this crate cannot encode cannot be left
// behind either.
fn raw_delete(fixture: &Fixture, escaped: &str) -> u16 {
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let blobs = fixture.blobs();
    let plan = PhysicalDelete::new(&fixture.put_key);
    let mut buf = vec![0; layered::delete_requirements(&blobs, &plan, &now).unwrap()];
    let request = blobs.encode_delete(&mut buf, &plan, &now).unwrap();

    let mut outgoing = ureq::delete(&format!(
        "{}/{}/{escaped}",
        fixture.endpoint, fixture.container
    ));
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap()
        .status()
        .as_u16()
}

/// Draws the line `addressable` draws around control characters in a key.
///
/// Measured before: Azure refuses `U+0001`, `U+000B`, `U+000C` and `U+000E`
/// with 400. This crate refuses every byte below a space on that evidence,
/// which is a wider claim than the measurement, so this checks the rest of the
/// class — including the three that XML itself allows, which are where a
/// narrower rule would have to stop.
///
/// It also checks the two just outside the class, so the rule is no wider than
/// the service's.
#[test]
#[ignore = "requires Azure credentials"]
fn the_control_characters_azure_refuses_are_the_ones_this_crate_refuses() {
    let fixture = Fixture::from_env();

    for escaped in ["%01", "%09", "%0A", "%0D", "%1F"] {
        let key = format!("{}c-{escaped}.txt", fixture.list_prefix);
        let status = raw_put(&fixture, &key);
        if (200..300).contains(&status) {
            assert!(raw_delete(&fixture, &key) < 300, "left {key} behind");
        }
        assert_eq!(
            status, 400,
            "Azure took {escaped} in a name, so `addressable` refuses a key it \\
             would have accepted and the rule must stop short of this byte"
        );
    }

    // Just outside the class: a delete and a C1 control, whose bytes are all
    // above a space. This crate allows both.
    for escaped in ["%7F", "%C2%85"] {
        let key = format!("{}c-{escaped}.txt", fixture.list_prefix);
        let status = raw_put(&fixture, &key);
        if (200..300).contains(&status) {
            assert!(raw_delete(&fixture, &key) < 300, "left {key} behind");
        }
        assert!(
            (200..300).contains(&status),
            "Azure refused {escaped}, which is outside the class this crate \\
             refuses, so the rule is narrower than the service's and has to \\
             grow: status {status}"
        );
    }
}
