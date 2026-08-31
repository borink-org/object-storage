use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage_proto::{
    Blobs, ConditionKind, Container, DeleteHeadOutcome, DeleteKind, DeleteShape, EntryKind,
    FailureClass, Fill, GetHeadOutcome, GetKind, GetShape, ListHeadOutcome, Method, Payload,
    PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut, PutHeadOutcome, PutShape,
    RequestedRange, ResponseHead, ServiceErrorKind, Timestamps, layered,
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
fn page(
    fixture: &Fixture,
    plan: &PhysicalList<'_>,
    room: usize,
) -> Result<Page, Box<dyn std::error::Error>> {
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
    let mut body = incoming.body_mut().read_to_vec()?;
    if let Some(len) = expected_len {
        assert_eq!(body.len() as u64, len, "the head sized the body");
    }

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

/// Settles what a listing of a container this credential was not granted
/// answers, which is not what the container's existence would suggest.
///
/// The role assignment is scoped to one container, so Azure refuses the
/// request before it says whether any other container is there: the answer is
/// 403, not the 404 that `ListHeadOutcome::NotFound` describes. A credential
/// scoped to one container therefore cannot observe `NotFound` at all, and a
/// caller that treats "not found" as the way a listing fails would never see
/// this.
///
/// If the grant is ever widened to the storage account, this becomes the 404
/// instead, and the assertion below is what will say so.
#[test]
#[ignore = "requires Azure credentials"]
fn listing_a_container_outside_the_grant_is_refused_before_it_is_looked_for() {
    let fixture = Fixture::from_env();
    let outside = Fixture {
        container: format!("{}-absent", fixture.container),
        ..clone(&fixture)
    };
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let blobs = outside.blobs();
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
    let outcome = blobs.accept_list_head(head).unwrap();
    let ListHeadOutcome::ServiceFailure(failure) = outcome else {
        panic!(
            "a container outside the grant is refused, not reported absent; \
             if this is now NotFound the grant was widened, and the \
             documentation on this test must say so: {outcome:?}"
        );
    };
    assert_eq!(failure.status, 403);
    assert_eq!(failure.class, FailureClass::Auth);
    assert_eq!(failure.kind, Some(ServiceErrorKind::Unauthorized));
    // The refusal is decisive from the head alone, as every other one is.
    assert!(failure.request_id.is_some());
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
