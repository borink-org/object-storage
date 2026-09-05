use std::str;
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

use test_support::azure::{self, LIVE_PREFIX};
use test_support::{base64, percent_encode};

use borink_object_storage_proto::{
    Blobs, ConditionKind, Container, DeleteHeadOutcome, DeleteKind, DeleteShape, EntryKind,
    GetHeadOutcome, GetKind, GetShape, ListEntry, ListHeadOutcome, Method, Payload, PhysicalDelete,
    PhysicalGet, PhysicalList, PhysicalPut, PutHeadOutcome, PutShape, RequestedRange, ResponseHead,
    ServiceErrorKind, Timestamps, layered,
};

// `#[ignore]` is built into Rust's test harness: ordinary test runs compile but
// skip these tests, while `cargo test -- --ignored` executes them.

// The object the read tests read. The suite writes it itself, once per run,
// so a container that holds nothing is enough to run against.
const CONTENTS: &[u8] = b"0123456789-azure-live-reference";

#[derive(Debug)]
struct Fixture {
    endpoint: String,
    container: String,
    // The read reference: `Fixture::with_reference` puts it in place.
    key: String,
    // The write tests own this key. It is never the read reference above.
    put_key: String,
    // The listing tests own everything under this prefix, and empty it before
    // each test so that what a page holds is what the test wrote.
    list_prefix: String,
    // The multipart probes own everything under this prefix, and remove what
    // they wrote.
    multipart_prefix: String,
    // Whether the account under test has a hierarchical namespace. The suite
    // runs against both kinds, and some tests only apply to one of them.
    hierarchical: bool,
    token: String,
}

impl Fixture {
    // The account `test_support::azure` names, and the keys under its live
    // prefix that this suite owns. The keys are constants rather than
    // settings because nothing else may write under that prefix, and the
    // recorder's prefix never overlaps it.
    fn from_env() -> Self {
        let account = azure::Account::under_test();
        Self {
            endpoint: account.endpoint,
            container: account.container,
            key: format!("{LIVE_PREFIX}reference/a key+é.txt"),
            put_key: format!("{LIVE_PREFIX}write/a key+é.bin"),
            list_prefix: format!("{LIVE_PREFIX}list/"),
            multipart_prefix: format!("{LIVE_PREFIX}multipart/"),
            hierarchical: account.hierarchical,
            token: azure::token(),
        }
    }

    // The fixture with the read reference in place: the object is written if
    // a read finds nothing at its key, once per run. Its content is the
    // constant the read tests assert, so a reference another run wrote is as
    // good as this one's.
    fn with_reference() -> Self {
        static SEEDED: Once = Once::new();
        let fixture = Self::from_env();
        SEEDED.call_once(|| {
            if read(&fixture, METADATA, None).unwrap().outcome == Outcome::NotFound {
                let owner = Fixture {
                    put_key: fixture.key.clone(),
                    ..clone(&fixture)
                };
                assert_eq!(
                    write(&owner, PutShape::default(), None, CONTENTS)
                        .unwrap()
                        .outcome,
                    WriteOutcome::Created,
                    "the read reference could not be written"
                );
            }
        });
        fixture
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
    let result = read(&Fixture::with_reference(), GetShape::default(), None).unwrap();
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
    let result = read(&Fixture::with_reference(), shape, None).unwrap();
    assert_eq!(result.outcome, Outcome::Body);
    assert_eq!(result.body, &CONTENTS[2..11]);
    assert_eq!(result.size, Some(CONTENTS.len() as u64));
}

#[test]
#[ignore = "requires Azure credentials"]
fn heads_the_blob() {
    let result = read(&Fixture::with_reference(), METADATA, None).unwrap();
    assert_eq!(result.outcome, Outcome::Complete);
    assert!(result.body.is_empty());
    assert_eq!(result.size, Some(CONTENTS.len() as u64));
    assert!(result.e_tag.is_some());
}

#[test]
#[ignore = "requires Azure credentials"]
fn applies_if_match() {
    let fixture = Fixture::with_reference();
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
    let fixture = Fixture::with_reference();
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

// The write half of the suite. These tests own the write key and overwrite
// it, so they never touch the object the read tests assert on.

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

// The read helper reads the reference object; the write tests need the same
// reads against the key they own.
fn read_put_key(fixture: &Fixture, shape: GetShape) -> ReadResult {
    let swapped = Fixture {
        key: fixture.put_key.clone(),
        ..clone(fixture)
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
    // A hierarchical account has no snapshots, as the probe near the end of
    // this file measured.
    if fixture.hierarchical {
        return;
    }
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
    if fixture.hierarchical {
        return;
    }
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
type Page = (Vec<Entry>, Option<String>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: EntryKind,
    key: String,
    size: Option<u64>,
    e_tag: Option<String>,
    last_modified: Option<u64>,
}

// Returns the response body of one listing request, as it came off the wire.
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

// Reads one page into an array that holds it whole: as many entries as the
// plan asked for, or the most the service writes when it asked for none.
fn page(fixture: &Fixture, plan: &PhysicalList<'_>) -> Result<Page, Box<dyn std::error::Error>> {
    let blobs = fixture.blobs();
    let mut body = fetch(fixture, plan)?;
    let room = plan.max_results.map_or(5000, |most| most as usize);
    let mut into = vec![ListEntry::default(); room];
    let page = blobs.fill_listing(&mut body, &mut into)?;
    let entries = into[..page.filled]
        .iter()
        .map(|entry| Entry {
            kind: entry.kind,
            key: entry.key.to_owned(),
            size: entry.size,
            e_tag: entry.e_tag.map(str::to_owned),
            last_modified: entry
                .last_modified
                .map(str::as_bytes)
                .and_then(layered::http_date_ms),
        })
        .collect();
    Ok((entries, page.next_marker.map(str::to_owned)))
}

// Every key under the prefix, page by page, the way a caller walks a listing.
fn walk(fixture: &Fixture, delimited: bool) -> Vec<Entry> {
    let mut marker: Option<String> = None;
    let mut all = Vec::new();
    loop {
        let plan = PhysicalList {
            marker: marker.as_deref(),
            delimited,
            ..PhysicalList::new(&fixture.list_prefix)
        };
        let (entries, next) = page(fixture, &plan).unwrap();
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
        multipart_prefix: fixture.multipart_prefix.clone(),
        hierarchical: fixture.hierarchical,
        token: fixture.token.clone(),
    }
}

// Removes everything under the prefix, so the tests below start from nothing.
fn empty(fixture: &Fixture) {
    // A hierarchical account lists its directories as entries beside the
    // objects under them. It refuses to delete a directory that still holds
    // anything, with a 409. Deleting the longest key first puts every child
    // before the directory that holds it.
    let mut entries = walk(fixture, false);
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.key.len()));
    for entry in entries {
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
    assert_eq!(walk(fixture, false), []);
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
/// This also checks that the last page of a real listing can be read at all.
/// Azure closes it with an empty `<NextMarker />`, written with a space before
/// the slash. A reader that matched the tag byte for byte would fault on every
/// listing that ends.
#[test]
#[ignore = "requires Azure credentials"]
fn lists_every_key_under_the_prefix() {
    let fixture = Fixture::from_env();
    seed_listing(&fixture);

    let prefix = &fixture.list_prefix;
    let mut expected = vec![
        (EntryKind::Object, format!("{prefix}a.txt"), Some(8)),
        (EntryKind::Object, format!("{prefix}b.txt"), Some(10)),
        (EntryKind::Object, format!("{prefix}nested/c.txt"), Some(1)),
    ];
    // Measured on the hierarchical account: an undelimited listing reports the
    // directory that holds `nested/c.txt` as an entry of its own. It is named
    // without a trailing separator and has no length. A flat account never
    // writes this shape. It sorts by its name like any other entry.
    if fixture.hierarchical {
        expected.insert(2, (EntryKind::Directory, format!("{prefix}nested"), None));
    }

    let entries = walk(&fixture, false);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.kind, entry.key.clone(), entry.size))
            .collect::<Vec<_>>(),
        expected
    );
    // Every object has an entity tag and a last-modified time in the listing,
    // so a caller need not HEAD each one. A directory is not an object, and
    // this crate reports no entity tag for one.
    for entry in &entries {
        assert_eq!(
            entry.e_tag.is_some(),
            entry.kind != EntryKind::Directory,
            "{}",
            entry.key
        );
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

    let entries = walk(&fixture, true);
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

/// Measures what a listing writes inside a group of keys.
///
/// The reader takes only the name of a group and skips whatever else the
/// element holds, so this records what it skips, by walking the entry's
/// properties the way a caller can. The response is printed whole; run with
/// `--nocapture` to see it.
///
/// Measured: a flat account writes a `<Name>` and nothing else. A
/// hierarchical account keeps a directory for the group and writes the
/// directory's own `<Properties>` block after the name, the same block an
/// undelimited listing gives that directory as a `<Blob>`: its creation time,
/// last-modified and entity tag, `<ResourceType>directory</ResourceType>`, a
/// length of zero and the content headers empty. So on such an account a
/// delimited listing is where a directory's own timestamps and entity tag
/// come from, and a caller who wants them reads them from `ListEntry::raw`.
#[test]
#[ignore = "requires Azure credentials"]
fn a_group_of_keys_carries_what_the_account_keeps_for_it() {
    let fixture = Fixture::from_env();
    seed_listing(&fixture);

    let plan = PhysicalList {
        delimited: true,
        ..PhysicalList::new(&fixture.list_prefix)
    };
    let raw = raw_page(&fixture, &plan);
    raw.show(if fixture.hierarchical {
        "List Blobs, delimited, hierarchical account"
    } else {
        "List Blobs, delimited, flat account"
    });
    assert_eq!(raw.status, 200);

    let blobs = fixture.blobs();
    let mut body = raw.body.clone();
    let mut into = [ListEntry::default(); 8];
    let page = blobs.fill_listing(&mut body, &mut into).unwrap();
    let group = into[..page.filled]
        .iter()
        .find(|entry| entry.kind == EntryKind::Prefix)
        .expect("the level below, reported as a group");
    assert_eq!(group.key, format!("{}nested/", fixture.list_prefix));
    let properties: Vec<(String, String)> = group
        .properties()
        .map(|(name, value)| {
            (
                String::from_utf8_lossy(name).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            )
        })
        .collect();
    println!("--- the group's properties: {properties:?}");
    let names: Vec<&str> = properties.iter().map(|(name, _)| name.as_str()).collect();
    if fixture.hierarchical {
        let value = |wanted: &str| {
            properties
                .iter()
                .find(|(name, _)| name == wanted)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(names[0], "Name");
        assert_eq!(value("ResourceType"), Some("directory"));
        assert_eq!(value("Content-Length"), Some("0"));
        for stamp in ["Creation-Time", "Last-Modified", "Etag"] {
            assert!(
                value(stamp).is_some_and(|value| !value.is_empty()),
                "the group carries no {stamp}"
            );
        }
    } else {
        assert_eq!(names, ["Name"]);
    }
}

/// The two ways a listing is split, against the same three objects: the
/// service splitting it into pages, and the caller's array splitting each page
/// into rounds. Neither may lose a key or report one twice.
#[test]
#[ignore = "requires Azure credentials"]
fn a_listing_split_into_pages_reads_the_same_keys() {
    let fixture = Fixture::from_env();
    seed_listing(&fixture);
    let whole = walk(&fixture, false);
    // Three objects, and on a hierarchical account the directory that holds
    // one of them.
    assert_eq!(whole.len(), if fixture.hierarchical { 4 } else { 3 });

    // One entry per page, so the service names a next page twice.
    let mut marker: Option<String> = None;
    let mut paged = Vec::new();
    let mut pages = 0;
    loop {
        let plan = PhysicalList {
            marker: marker.as_deref(),
            max_results: Some(1),
            ..PhysicalList::new(&fixture.list_prefix)
        };
        let (entries, next) = page(&fixture, &plan).unwrap();
        pages += 1;
        paged.extend(entries);
        match next {
            Some(next) => marker = Some(next),
            None => break,
        }
        assert!(pages < 10, "a page of one is not making progress");
    }
    assert_eq!(paged, whole);
    assert!(
        pages >= whole.len(),
        "{} entries, one per page: {pages} pages",
        whole.len()
    );
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
/// cannot parse rather than reading it. A condition written that way has no
/// effect at all, and quoting becomes required.
#[test]
#[ignore = "requires Azure credentials"]
fn an_entity_tag_from_a_listing_conditions_a_read_quoted_or_not() {
    let fixture = Fixture::from_env();
    seed_listing(&fixture);
    let entry = walk(&fixture, false).remove(0);
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

    // Measured: Azure accepts the listed tag unquoted.
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

/// Measures what a listed name looks like when Azure marks it as encoded. That
/// is why `xml::decode_percent` may refuse a `%` that is not an escape.
///
/// Measured. Azure refuses the C0 controls in a name outright, with 400, but
/// stores `U+FFFE` and `U+FFFF`, which XML 1.0 forbids in a document. It lists
/// such a name with `Encoded="true"` and encodes the whole of it, including
/// the separators between its segments. The name below comes back as
/// `borink-object-storage%2Flive%2Flist%2F100%25-%EF%BF%BE-name.txt`.
/// So every `%` in an encoded name begins an escape.
#[test]
#[ignore = "requires Azure credentials"]
fn an_encoded_name_is_encoded_whole_and_comes_back_whole() {
    let fixture = Fixture::from_env();
    empty(&fixture);

    // What XML forbids and Azure does hold. The control characters it forbids
    // and Azure does not hold are
    // `the_control_characters_azure_refuses_are_the_ones_this_crate_refuses`,
    // which can still send them because it writes its own request.
    //
    // The percent is the byte in question: in an encoded name it must be
    // written `%25`.
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
        xml.contains("100%25-") && xml.contains("live%2Flist%2F"),
        "an encoded name is encoded whole, separators included, so every `%` \
         in one is an escape. If this fails, xml::decode_percent must stop \
         refusing a `%` that is not one: {xml}"
    );

    // And the name comes back as it was written.
    let listed: Vec<String> = walk(&fixture, false)
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

    // Exactly the limit, reached two ways: with two-byte characters, and with
    // half as many four-byte characters, after one two-byte character when
    // the prefix leaves an odd number of units to fill.
    let widest = format!("{}{}", fixture.list_prefix, "é".repeat(1024 - prefix));
    let deepest = format!(
        "{}{}{}",
        fixture.list_prefix,
        "a".repeat((1024 - prefix) % 2),
        "🦀".repeat((1024 - prefix) / 2)
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

    // The prefix ends in the separator, so it carries as many segments as it
    // has separators, and that many fewer joined after it make a name of
    // exactly `total`.
    let own = fixture.list_prefix.matches('/').count();
    let of_segments = |total: usize| {
        let tail = vec!["s"; total - own].join("/");
        format!("{}{tail}", fixture.list_prefix)
    };
    // `addressable` refuses a name past the boundary this is looking for, so
    // the request is written by hand: the point is where the service draws the
    // line, not where this crate believes it is. Every byte of these names is
    // one a URL keeps, so the key and its encoded form are the same text.
    let takes = |total: usize| {
        let key = of_segments(total);
        assert_eq!(key.matches('/').count() + 1, total);
        assert!(key.encode_utf16().count() <= 1024, "{total} is too long");
        let status = raw_put(&fixture, &key);
        let took = (200..300).contains(&status);
        if took {
            assert!(raw_delete(&fixture, &key) < 300, "left {total} behind");
        }
        took
    };

    // The two accounts have different limits, so the search starts below both
    // and above both rather than at either answer.
    let (mut taken, mut refused) = (own + 1, 494);
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

    println!("--- the largest name taken has {taken} segments");
    let expected = if fixture.hierarchical {
        MAX_SEGMENTS_HIERARCHICAL
    } else {
        MAX_SEGMENTS
    };
    assert_eq!(
        taken, expected,
        "the largest name Azure takes has {taken} segments, not {expected}. \
         Correct the constant here and the segment rule in `addressable`"
    );
}

// Measured by the bisection above, and the number `addressable` holds.
const MAX_SEGMENTS: usize = 255;

// The same limit measured on a hierarchical account. There a name is a path
// through directories the service keeps, not a name that happens to hold
// separators. It is a quarter of the flat limit. `addressable` enforces
// neither yet, because the limit depends on the kind of account, and this
// crate is not told which kind it is talking to.
const MAX_SEGMENTS_HIERARCHICAL: usize = 61;

/// Measures what Azure does with the slashes this crate leaves literal in the
/// URL path. A hierarchical-namespace path needs them literal.
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
    for edge in [
        "trailing/",
        "double//slash",
        "space /x",
        "a.b/c",
        "..leading",
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

    let listed: Vec<String> = walk(&fixture, false)
        .into_iter()
        .map(|entry| entry.key)
        .collect();
    println!("--- the names the account stored: {listed:?}");
    for key in &created {
        // Measured on the hierarchical account, where a name is a path: an
        // empty segment is removed, so `double//slash` is stored as
        // `double/slash`. That is a name the caller did not write, and the
        // rule `addressable` enforces for a flat account does not cover it.
        // So the two accounts disagree about which keys are addressable. This
        // test records that, and no code acts on it yet.
        if fixture.hierarchical && key.contains("//") {
            assert!(
                !listed.contains(key) && listed.contains(&key.replace("//", "/")),
                "{key:?} was not stored with its empty segment removed, as \
                 was measured before. The listing reports {listed:?}"
            );
            continue;
        }
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
/// Measured: Azure refuses `U+0001`, `U+000B`, `U+000C`, `U+000E` and
/// `U+007F` with 400. This crate refuses every ASCII control character on that
/// evidence, which is wider than the measurement. So this checks the rest of
/// the class, including the three that XML itself allows.
///
/// It also checks one just outside the class, so the rule is no wider than the
/// service's. `U+0085` is a control character that is not an ASCII one, and
/// neither of its bytes is.
#[test]
#[ignore = "requires Azure credentials"]
fn the_control_characters_azure_refuses_are_the_ones_this_crate_refuses() {
    let fixture = Fixture::from_env();

    for escaped in ["%01", "%09", "%0A", "%0D", "%1F", "%7F"] {
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

    // Just outside the class: a C1 control, whose bytes are both above a
    // space, so no byte of it is an ASCII control. This crate allows it.
    let escaped = "%C2%85";
    let key = format!("{}c-{escaped}.txt", fixture.list_prefix);
    let status = raw_put(&fixture, &key);
    if (200..300).contains(&status) {
        assert!(raw_delete(&fixture, &key) < 300, "left {key} behind");
    }
    assert!(
        (200..300).contains(&status),
        "Azure refused {escaped}, which is outside the class this crate \
         refuses, so the rule is narrower than the service's and has to \
         grow: status {status}"
    );
}

/// What the service does with a marker that this crate passed through.
///
/// A marker is the service's own text and this crate checks only that it is
/// not empty, so what a wrong one costs is the service's answer, not ours.
/// Measured on 2026-09-01: a marker that is not one at all is 400
/// `InvalidQueryParameterValue`, and the body names `marker` as the parameter;
/// a real marker with its last bytes cut off is 400 `InvalidInput`. Neither is
/// a code this crate maps, so both arrive as `ServiceFailure` with no `kind`,
/// which is right: a 400 is the caller's bug and is not retryable.
///
/// A marker that is stale is not a case at all. The one below is read twice,
/// pages from the same place both times, and never expires within a run.
#[test]
#[ignore = "requires Azure credentials"]
fn a_marker_the_service_did_not_write_is_refused_by_it() {
    let fixture = Fixture::from_env();
    let blobs = fixture.blobs();
    let refused = list_status(&fixture, Some("not-a-marker"));
    assert_eq!(refused.0, 400);
    assert_eq!(
        blobs.accept_list_error_body(refused.0, None, &refused.1),
        ListHeadOutcome::ServiceFailure(borink_object_storage_proto::Failure {
            status: 400,
            class: borink_object_storage_proto::FailureClass::Other,
            kind: None,
            request_id: None,
        }),
        "a code this crate maps would name the error here"
    );

    // A marker the service wrote reads the same page however often it is used.
    let plan = PhysicalList {
        max_results: Some(1),
        ..PhysicalList::new(&fixture.list_prefix)
    };
    seed_listing(&fixture);
    let (_, marker) = page(&fixture, &plan).unwrap();
    let marker = marker.expect("a first page of one entry names the next");
    let again = PhysicalList {
        marker: Some(&marker),
        ..plan
    };
    assert_eq!(
        page(&fixture, &again).unwrap().0,
        page(&fixture, &again).unwrap().0,
        "a marker names a place in the container, not a session"
    );

    // The same marker with its last bytes cut off is not a marker.
    let cut = &marker[..marker.len() - 2];
    assert_eq!(list_status(&fixture, Some(cut)).0, 400);
}

/// The most entries a page reports, when the plan asks for more than the
/// service gives.
///
/// Measured on 2026-09-01: Azure answers 200 and echoes `<MaxResults>5000`,
/// as `PhysicalList::max_results` documents. A caller that asks for
/// more is not refused; it is answered with the maximum and a marker.
#[test]
#[ignore = "requires Azure credentials"]
fn a_page_larger_than_the_service_gives_is_answered_with_its_maximum() {
    let fixture = Fixture::from_env();
    let plan = PhysicalList {
        max_results: Some(99_999),
        ..PhysicalList::new(&fixture.list_prefix)
    };
    seed_listing(&fixture);

    let body = fetch(&fixture, &plan).expect("the service answers rather than refusing");
    let echoed = String::from_utf8(body).unwrap();
    assert!(
        echoed.contains("<MaxResults>5000</MaxResults>"),
        "the service no longer reports its own maximum: {echoed}"
    );
}

// One listing request, as the status and the body that answered it. The
// listing helpers above read a page; this one is for the answers that are not
// pages.
fn list_status(fixture: &Fixture, marker: Option<&str>) -> (u16, Vec<u8>) {
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let blobs = fixture.blobs();
    let plan = PhysicalList {
        marker,
        ..PhysicalList::new(&fixture.list_prefix)
    };
    let mut buf = vec![0; layered::list_requirements(&blobs, &plan, &now).unwrap()];
    let request = blobs.encode_list(&mut buf, &plan, &now).unwrap();
    let mut outgoing = ureq::get(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
    let status = incoming.status().as_u16();
    (status, incoming.body_mut().read_to_vec().unwrap())
}

// The multipart half of the suite: what Put Block, Put Block List and Get
// Block List do. This crate does not support them yet, and these probes
// measure them first.
//
// Each probe writes its own request the way `snapshot` does. The URL and the
// head come from a plan this crate encodes, and the query, the extra headers
// and the body are written here. The multipart types will be designed against
// these measurements, so every probe asserts the answer it measured and
// prints the response it read. What Azure sent, byte for byte, is recorded by
// `tests/azure-record` under `fixtures/azure-multipart`; a probe here says
// whether it still sends that.

// One whole response, for a probe to assert on and print.
#[derive(Debug)]
struct Raw {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Raw {
    // Keeps a response whole: the status, the headers in the order they
    // arrived, and the body.
    fn read(mut incoming: ureq::http::Response<ureq::Body>) -> Self {
        let status = incoming.status().as_u16();
        let headers = incoming
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = incoming.body_mut().read_to_vec().unwrap_or_default();
        Self {
            status,
            headers,
            body,
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(found, _)| found == name)
            .map(|(_, value)| value.as_str())
    }

    fn error_code(&self) -> Option<&str> {
        self.header("x-ms-error-code")
    }

    // Prints the head and the body, for whoever runs a probe to read what
    // the service answered. Run the suite with `--nocapture` to see them.
    fn show(&self, what: &str) {
        println!("--- {what}: {}", self.status);
        for (name, value) in &self.headers {
            // No response holds the token, and the request identifiers reveal
            // nothing about the account, so every header is safe to print.
            println!("{name}: {value}");
        }
        if !self.body.is_empty() {
            println!("{}", String::from_utf8_lossy(&self.body));
        }
        println!("---");
    }
}

// Sends a request this crate cannot encode, for a key it can.
fn raw(
    fixture: &Fixture,
    method: Method,
    key: &str,
    query: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Raw {
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let blobs = fixture.blobs();
    let plan = PhysicalDelete::new(key);
    let mut buf = vec![0; layered::delete_requirements(&blobs, &plan, &now).unwrap()];
    let request = blobs.encode_delete(&mut buf, &plan, &now).unwrap();

    let url = format!("{}{query}", request.url());
    // The head this crate wrote, plus whatever the probe adds to it.
    let head: Vec<(&str, &str)> = request.headers().chain(extra.iter().copied()).collect();
    // In ureq a request with a body and one without are different types, so
    // the header loop is written twice.
    let incoming = match method {
        Method::Put => {
            let mut outgoing = ureq::put(&url);
            for (name, value) in head {
                outgoing = outgoing.header(name, value);
            }
            outgoing
                .config()
                .http_status_as_error(false)
                .build()
                .send(body)
                .unwrap()
        }
        _ => {
            let mut outgoing = match method {
                Method::Get => ureq::get(&url),
                Method::Head => ureq::head(&url),
                _ => ureq::delete(&url),
            };
            for (name, value) in head {
                outgoing = outgoing.header(name, value);
            }
            outgoing
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .unwrap()
        }
    };
    Raw::read(incoming)
}

// Returns the eight-character identifier of one part. Every identifier
// decodes to the same number of bytes, which Azure requires within one
// upload.
fn block_id(index: u32) -> String {
    base64(&index.to_be_bytes())
}

fn stage(fixture: &Fixture, key: &str, id: &str, content: &[u8]) -> Raw {
    raw(
        fixture,
        Method::Put,
        key,
        &format!("?comp=block&blockid={}", percent_encode(id)),
        &[],
        content,
    )
}

fn commit(fixture: &Fixture, key: &str, ids: &[&str], extra: &[(&str, &str)]) -> Raw {
    let mut body = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
    for id in ids {
        body.push_str(&format!("<Latest>{id}</Latest>"));
    }
    body.push_str("</BlockList>");
    raw(
        fixture,
        Method::Put,
        key,
        "?comp=blocklist",
        extra,
        body.as_bytes(),
    )
}

// `kind` is `all`, `committed` or `uncommitted`.
fn block_list(fixture: &Fixture, key: &str, kind: &str) -> Raw {
    raw(
        fixture,
        Method::Get,
        key,
        &format!("?comp=blocklist&blocklisttype={kind}"),
        &[],
        &[],
    )
}

// Returns the names in one section of a Get Block List document, in order,
// each with its size. This is the test's own reading of the document. So the
// probes assert on what the service wrote, not on what this crate's future
// reader will make of it.
fn blocks(body: &[u8], section: &str) -> Vec<(String, u64)> {
    let document = String::from_utf8(body.to_vec()).unwrap();
    let Some(start) = document.find(&format!("<{section}>")) else {
        return Vec::new();
    };
    let end = document[start..].find(&format!("</{section}>")).unwrap() + start;
    document[start..end]
        .split("<Block>")
        .skip(1)
        .map(|block| {
            let field = |name: &str| {
                let at = block.find(&format!("<{name}>")).unwrap() + name.len() + 2;
                block[at..at + block[at..].find('<').unwrap()].to_owned()
            };
            (field("Name"), field("Size").parse().unwrap())
        })
        .collect()
}

// Returns a key under the multipart prefix. Each probe uses its own suffix.
fn scratch(fixture: &Fixture, suffix: &str) -> String {
    format!("{}{suffix}", fixture.multipart_prefix)
}

// Removes what a probe wrote, whether or not it was ever committed.
fn discard(fixture: &Fixture, key: &str) {
    let owner = Fixture {
        put_key: key.to_owned(),
        ..clone(fixture)
    };
    match remove(&owner, DeleteShape::default(), None).unwrap() {
        RemoveOutcome::Accepted | RemoveOutcome::NotFound(_) => {}
        outcome => panic!("{key}: {outcome:?}"),
    }
}

fn contents(fixture: &Fixture, key: &str) -> ReadResult {
    let owner = Fixture {
        key: key.to_owned(),
        ..clone(fixture)
    };
    read(&owner, GetShape::default(), None).unwrap()
}

/// Measures what a staged block is, before and after the commit that makes it
/// part of an object.
///
/// This settles four things. A block list of a key with staged blocks and no
/// committed blob is a 200 with an empty committed section. Its head has no
/// entity tag and no last-modified. A block list of a key that holds nothing is
/// a 404 `BlobNotFound`. The commit answers with the same values a whole
/// write does. And the object holds the blocks in the order the commit named
/// them, not the order they were staged in.
#[test]
#[ignore = "requires Azure credentials"]
fn a_block_is_invisible_until_the_commit_names_it() {
    let fixture = Fixture::from_env();
    let key = scratch(&fixture, "invisible.bin");
    discard(&fixture, &key);
    let (first, second) = (block_id(1), block_id(2));

    let staged = stage(&fixture, &key, &second, b"second");
    staged.show("Put Block");
    assert_eq!(staged.status, 201);
    // The service reports a checksum of the staged content. Measured under
    // service version 2026-04-06: the checksum is CRC64, and no `Content-MD5`
    // comes back.
    assert!(staged.header("x-ms-content-crc64").is_some());
    assert!(staged.header("content-md5").is_none());
    assert!(staged.header("etag").is_none());
    assert!(staged.header("last-modified").is_none());
    assert_eq!(stage(&fixture, &key, &first, b"first").status, 201);

    let all = block_list(&fixture, &key, "all");
    all.show("Get Block List, all, uncommitted only");
    assert_eq!(all.status, 200);
    assert_eq!(blocks(&all.body, "CommittedBlocks"), []);
    // Measured: the uncommitted blocks come back ordered by identifier, not
    // in the order they were staged. The second block above was staged first
    // and is listed second. So an identifier that sorts the way the parts are
    // numbered lists them in the order the object will hold them. The base64
    // of a big-endian number sorts that way.
    assert_eq!(
        blocks(&all.body, "UncommittedBlocks"),
        [(first.clone(), 5), (second.clone(), 6)]
    );
    // No blob exists yet, so there is nothing for these to describe.
    assert!(all.header("etag").is_none());
    assert!(all.header("last-modified").is_none());
    assert!(all.body.contains(&b'<'));

    // Measured, and not what the plan assumed. A committed listing of a key
    // that holds staged blocks and no blob is a 200 with an empty section.
    // Only a key that holds nothing at all answers 404. So whether a blob
    // exists must be read from the listing's contents.
    let committed = block_list(&fixture, &key, "committed");
    committed.show("Get Block List, committed, before the commit");
    assert_eq!((committed.status, committed.error_code()), (200, None));
    assert_eq!(blocks(&committed.body, "CommittedBlocks"), []);
    assert!(committed.header("etag").is_none());

    let absent = block_list(&fixture, &scratch(&fixture, "never-written.bin"), "all");
    absent.show("Get Block List, all, on a key that holds nothing");
    assert_eq!(
        (absent.status, absent.error_code()),
        (404, Some("BlobNotFound"))
    );

    // The commit names the parts in the order the object should hold them,
    // which is not the order they were staged in.
    let commit = commit(&fixture, &key, &[&first, &second], &[]);
    commit.show("Put Block List");
    assert_eq!(commit.status, 201);
    assert!(commit.header("etag").is_some());
    assert!(commit.header("last-modified").is_some());
    // `x-ms-version-id` is present when the account keeps versions. The flat
    // test account does and the hierarchical one cannot, so its presence
    // depends on the account, not on the operation.
    println!(
        "--- x-ms-version-id: {:?}",
        commit.header("x-ms-version-id")
    );
    assert_eq!(contents(&fixture, &key).body, b"firstsecond");

    let after = block_list(&fixture, &key, "all");
    after.show("Get Block List, all, after the commit");
    assert_eq!(after.status, 200);
    assert_eq!(
        blocks(&after.body, "CommittedBlocks"),
        [(first, 5), (second, 6)]
    );
    assert_eq!(blocks(&after.body, "UncommittedBlocks"), []);
    // A committed listing describes the blob, so its head holds the blob's
    // entity tag and length.
    assert_eq!(after.header("etag"), commit.header("etag"));
    assert!(after.header("x-ms-blob-content-length").is_some());

    discard(&fixture, &key);
}

/// Measures the two conditions a commit can be made under, and the status each
/// refusal answers with.
///
/// The lost create is the one that matters. If it answers 409
/// `BlobAlreadyExists` like `Put Blob` does, the commit can use the same
/// status handling as a write. The documentation says 412.
#[test]
#[ignore = "requires Azure credentials"]
fn a_commit_that_a_condition_refuses_names_what_refused_it() {
    let fixture = Fixture::from_env();
    let key = scratch(&fixture, "conditional.bin");
    discard(&fixture, &key);
    let id = block_id(1);

    // An object to lose the race against.
    assert_eq!(stage(&fixture, &key, &id, b"first").status, 201);
    assert_eq!(commit(&fixture, &key, &[&id], &[]).status, 201);
    let e_tag = contents(&fixture, &key).e_tag.unwrap();

    assert_eq!(stage(&fixture, &key, &id, b"again").status, 201);
    let lost = commit(&fixture, &key, &[&id], &[("if-none-match", "*")]);
    lost.show("Put Block List, If-None-Match: *, on an object that exists");
    assert_eq!(
        (lost.status, lost.error_code()),
        (409, Some("BlobAlreadyExists")),
        "a lost create is the conflict Put Blob reports, not the 412 the documentation states"
    );

    let stale = commit(&fixture, &key, &[&id], &[("if-match", "\"0x0\"")]);
    stale.show("Put Block List, a stale If-Match");
    assert_eq!(
        (stale.status, stale.error_code()),
        (412, Some("ConditionNotMet"))
    );

    // The object's real tag lets the commit through. That shows the two
    // refusals above came from the condition and not from the request.
    assert_eq!(
        commit(&fixture, &key, &[&id], &[("if-match", &e_tag)]).status,
        201
    );
    assert_eq!(contents(&fixture, &key).body, b"again");

    discard(&fixture, &key);
}

/// Measures three block lists that Azure may refuse, and the code each refusal
/// answers with. The error mapping will be written from these.
#[test]
#[ignore = "requires Azure credentials"]
fn the_block_lists_azure_refuses_and_the_codes_it_refuses_them_with() {
    let fixture = Fixture::from_env();
    let key = scratch(&fixture, "refused.bin");
    discard(&fixture, &key);

    // Identifiers that decode to different lengths. Azure requires one length
    // for the whole upload. Measured: it refuses the second identifier when
    // it is staged, before any block list names it. So the core never has to
    // check the lengths itself, and this is the code a stage is refused with.
    let short = base64(b"ab");
    let long = base64(b"abcd");
    let first = stage(&fixture, &key, &short, b"x");
    first.show("Put Block, a four-character identifier");
    assert_eq!(first.status, 201);
    let second = stage(&fixture, &key, &long, b"y");
    second.show("Put Block, an identifier of another length beside it");
    assert_eq!(
        (second.status, second.error_code()),
        (400, Some("InvalidBlobOrBlock"))
    );

    // A commit that names a block nobody staged.
    let absent = commit(&fixture, &key, &[&short, &block_id(9)], &[]);
    absent.show("Put Block List, a block that was never staged");
    assert_eq!(
        (absent.status, absent.error_code()),
        (400, Some("InvalidBlockList"))
    );

    // A commit that names no blocks. Whether Azure refuses this or creates an
    // empty object decides whether a plan may have no parts.
    let empty = commit(&fixture, &key, &[], &[]);
    empty.show("Put Block List, naming no blocks");
    assert_eq!(empty.status, 201, "an empty commit creates an empty object");
    assert_eq!(contents(&fixture, &key).size, Some(0));

    discard(&fixture, &key);
}

/// Measures whether an escaped block identifier is read back as written.
///
/// The identifiers this crate writes are base64, which holds `+`, `/` and
/// `=`. All three have a meaning in a query, so all three are escaped. This
/// checks that Azure reads the escaped form back as the identifier. It also
/// checks what Azure does with an empty block.
#[test]
#[ignore = "requires Azure credentials"]
fn an_identifier_that_a_query_cannot_carry_is_escaped_and_read_back() {
    let fixture = Fixture::from_env();
    let key = scratch(&fixture, "escaped.bin");
    discard(&fixture, &key);

    // Bytes chosen so that the base64 holds all three characters that have a
    // meaning in a query.
    let id = base64(&[0xFB, 0xFF]);
    assert!(
        id.contains('+') && id.contains('/') && id.contains('='),
        "{id}"
    );
    let staged = stage(&fixture, &key, &id, b"x");
    staged.show("Put Block, an escaped identifier");
    assert_eq!(staged.status, 201);

    let all = block_list(&fixture, &key, "uncommitted");
    all.show("Get Block List, uncommitted, an escaped identifier");
    assert_eq!(blocks(&all.body, "UncommittedBlocks"), [(id.clone(), 1)]);
    assert_eq!(commit(&fixture, &key, &[&id], &[]).status, 201);
    assert_eq!(contents(&fixture, &key).size, Some(1));

    // Measured: Azure refuses to stage an empty block. An empty object is
    // made by a commit that names no blocks, which the probe above measures.
    let nothing = stage(&fixture, &key, &block_id(7), b"");
    nothing.show("Put Block, no content at all");
    assert_eq!(
        (nothing.status, nothing.error_code()),
        (400, Some("InvalidHeaderValue"))
    );

    discard(&fixture, &key);
}

/// Measures what staging the same identifier twice does. If the last content
/// wins, a retried part need not be given a new identifier.
#[test]
#[ignore = "requires Azure credentials"]
fn staging_one_identifier_twice_keeps_the_content_staged_last() {
    let fixture = Fixture::from_env();
    let key = scratch(&fixture, "restaged.bin");
    discard(&fixture, &key);
    let id = block_id(1);

    assert_eq!(stage(&fixture, &key, &id, b"first attempt").status, 201);
    assert_eq!(stage(&fixture, &key, &id, b"second").status, 201);
    let staged = block_list(&fixture, &key, "uncommitted");
    staged.show("Get Block List, uncommitted, one identifier staged twice");
    assert_eq!(blocks(&staged.body, "UncommittedBlocks"), [(id.clone(), 6)]);

    assert_eq!(commit(&fixture, &key, &[&id], &[]).status, 201);
    assert_eq!(contents(&fixture, &key).body, b"second");

    discard(&fixture, &key);
}

/// Measures what happens to an upload that is never committed. There is no
/// abort operation on Azure, and this shows why none is needed.
///
/// Blocks staged against a key that already holds an object are invisible to
/// every read of it. A whole-object write to that key discards them.
#[test]
#[ignore = "requires Azure credentials"]
fn an_upload_that_is_never_committed_is_ended_by_a_whole_object_write() {
    let fixture = Fixture::from_env();
    let key = scratch(&fixture, "abandoned.bin");
    let owner = Fixture {
        put_key: key.clone(),
        ..clone(&fixture)
    };
    discard(&fixture, &key);

    assert_eq!(
        write(&owner, PutShape::default(), None, b"the object")
            .unwrap()
            .outcome,
        WriteOutcome::Created
    );
    let id = block_id(1);
    assert_eq!(stage(&fixture, &key, &id, b"never committed").status, 201);

    // A staged block changes nothing about the object.
    assert_eq!(contents(&fixture, &key).body, b"the object");
    let staged = block_list(&fixture, &key, "all");
    staged.show("Get Block List, all, on an object with blocks staged against it");
    // An object written whole is not made of blocks, so its committed section
    // is empty however large the object is.
    assert_eq!(blocks(&staged.body, "CommittedBlocks"), []);
    assert_eq!(blocks(&staged.body, "UncommittedBlocks"), [(id, 15)]);

    // A whole-object write to the key discards them.
    assert_eq!(
        write(&owner, PutShape::default(), None, b"written whole")
            .unwrap()
            .outcome,
        WriteOutcome::Created
    );
    let after = block_list(&fixture, &key, "all");
    after.show("Get Block List, all, after a whole-object write");
    assert_eq!(blocks(&after.body, "UncommittedBlocks"), []);

    discard(&fixture, &key);
}

/// Resumes an upload from the blocks the service says it holds.
///
/// A host needs this after a crash. The parts it already staged are named in
/// the uncommitted section, so it stages only the missing ones and commits
/// the whole sequence.
#[test]
#[ignore = "requires Azure credentials"]
fn an_upload_is_resumed_from_the_blocks_the_service_still_holds() {
    let fixture = Fixture::from_env();
    let key = scratch(&fixture, "resumed.bin");
    discard(&fixture, &key);
    let ids: Vec<String> = (1..=3).map(block_id).collect();

    assert_eq!(stage(&fixture, &key, &ids[0], b"one ").status, 201);
    assert_eq!(stage(&fixture, &key, &ids[1], b"two ").status, 201);

    // What a host that lost its state asks the service.
    let held = block_list(&fixture, &key, "uncommitted");
    let staged: Vec<String> = blocks(&held.body, "UncommittedBlocks")
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(staged, ids[..2]);

    for id in &ids {
        if !staged.contains(id) {
            assert_eq!(stage(&fixture, &key, id, b"three").status, 201);
        }
    }
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    assert_eq!(commit(&fixture, &key, &ids, &[]).status, 201);
    assert_eq!(contents(&fixture, &key).body, b"one two three");

    discard(&fixture, &key);
}

/// Lists the objects a multipart upload made, two entries at a time, on
/// whichever account the suite is running against.
///
/// The listing tests above own their own prefix. This test lists the
/// multipart prefix instead, and checks how a delimited listing reports the
/// level below on both kinds of account.
#[test]
#[ignore = "requires Azure credentials"]
fn a_listing_of_the_multipart_prefix_reports_what_the_commits_wrote() {
    let fixture = Fixture::from_env();
    let keys = ["listed/a.bin", "listed/b.bin"];
    for suffix in keys {
        let key = scratch(&fixture, suffix);
        discard(&fixture, &key);
        let id = block_id(1);
        assert_eq!(stage(&fixture, &key, &id, b"0123456789").status, 201);
        assert_eq!(commit(&fixture, &key, &[&id], &[]).status, 201);
    }

    let prefix = format!("{}listed/", fixture.multipart_prefix);
    let listed = Fixture {
        list_prefix: prefix.clone(),
        ..clone(&fixture)
    };
    let entries = walk(&listed, false);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.kind, entry.key.clone(), entry.size))
            .collect::<Vec<_>>(),
        keys.map(|suffix| (
            EntryKind::Object,
            format!("{prefix}{}", &suffix[7..]),
            Some(10)
        ))
        .to_vec()
    );

    // The level above, delimited.
    let above = Fixture {
        list_prefix: fixture.multipart_prefix.clone(),
        ..clone(&fixture)
    };
    let groups: Vec<(EntryKind, String)> = walk(&above, true)
        .into_iter()
        .filter(|entry| entry.key.starts_with(&prefix) || entry.key == prefix.trim_end_matches('/'))
        .map(|entry| (entry.kind, entry.key))
        .collect();
    println!("--- the level above, delimited: {groups:?}");
    // Measured on both accounts: a delimited listing reports the level below
    // as a group, with the delimiter at the end of its name. That is true
    // whether or not the account keeps a directory for it. Only an
    // undelimited listing on a hierarchical account reports a directory
    // entry, and that name has no delimiter. The two shapes never appear in
    // the same page.
    assert_eq!(groups, [(EntryKind::Prefix, prefix)]);

    for suffix in keys {
        discard(&fixture, &scratch(&fixture, suffix));
    }
}

/// Measures what a hierarchical account does with snapshots.
///
/// `DeleteKind` already writes `x-ms-delete-snapshots`. The flat account
/// accepts that header, and this checks whether a hierarchical account
/// refuses it.
#[test]
#[ignore = "requires Azure credentials"]
fn a_hierarchical_account_answers_for_the_snapshots_a_delete_asks_about() {
    let fixture = Fixture::from_env();
    if !fixture.hierarchical {
        return;
    }
    let key = scratch(&fixture, "snapshotted.bin");
    let owner = Fixture {
        put_key: key.clone(),
        ..clone(&fixture)
    };
    discard(&fixture, &key);
    assert_eq!(
        write(&owner, PutShape::default(), None, b"snapshot me")
            .unwrap()
            .outcome,
        WriteOutcome::Created
    );

    // Measured: a hierarchical account has no snapshots at all. It refuses to
    // take one with 409 and a code that names the missing feature. So the two
    // snapshot tests above only run on the flat account.
    let snapshot = raw(&fixture, Method::Put, &key, "?comp=snapshot", &[], b"");
    snapshot.show("Snapshot Blob, on a hierarchical account");
    assert_eq!(
        (snapshot.status, snapshot.error_code()),
        (
            409,
            Some("FeatureNotYetSupportedForHierarchicalNamespaceAccounts")
        )
    );

    // The delete header that mentions snapshots is still accepted, so
    // `DeleteKind` need not be refused before the request is sent.
    let removed = remove(
        &owner,
        DeleteShape {
            kind: DeleteKind::ObjectAndSnapshots,
            ..DeleteShape::default()
        },
        None,
    )
    .unwrap();
    println!(
        "--- Delete Blob asking for the snapshots too, on a hierarchical account: {removed:?}"
    );
    assert_eq!(removed, RemoveOutcome::Accepted);
}

// Sends a listing request whose query this crate cannot write.
// `PhysicalList::prefix` is a `&str`, so it cannot hold a byte that is not
// UTF-8, and a `%` in it is escaped as `%25`. The probe below needs the
// escape to reach the service as written. The head comes from a plan this
// crate does encode.
fn raw_list(fixture: &Fixture, query: &str) -> Raw {
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let blobs = fixture.blobs();
    let plan = PhysicalList::new("");
    let mut buf = vec![0; layered::list_requirements(&blobs, &plan, &now).unwrap()];
    let request = blobs.encode_list(&mut buf, &plan, &now).unwrap();

    let url = format!(
        "{}/{}?restype=container&comp=list{query}",
        fixture.endpoint, fixture.container
    );
    let mut outgoing = ureq::get(&url);
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
    Raw::read(incoming)
}

// Fetches one page the way `fetch` does, but keeps the whole response rather
// than the body alone, so that a probe can print it in the form a fixture
// records.
fn raw_page(fixture: &Fixture, plan: &PhysicalList<'_>) -> Raw {
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let blobs = fixture.blobs();
    let mut buf = vec![0; layered::list_requirements(&blobs, plan, &now).unwrap()];
    let request = blobs.encode_list(&mut buf, plan, &now).unwrap();
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
    Raw::read(incoming)
}

/// Measures why the reader may treat a body that is not UTF-8 as a fault.
///
/// The reader checks the whole body before it reads anything from it, and
/// refuses one that is not valid UTF-8. That relies on the service, not on
/// XML. A key is percent-encoded bytes in a path. Nothing in the wire format
/// stops a percent escape from naming a byte that is not UTF-8. This measures
/// the two ways such a byte could get into a page.
///
/// Measured: Azure refuses the key outright, and replaces the byte in a query
/// value with `U+FFFD` before echoing it. So a byte that is not UTF-8 never
/// reaches a listing body. One that does is a protocol violation rather than
/// a key some caller holds.
#[test]
#[ignore = "requires Azure credentials"]
fn a_listing_body_is_always_utf_8() {
    let fixture = Fixture::from_env();

    // The key. `ObjectKey` is a `&str` and cannot hold these bytes, so the
    // request is written by hand, like every probe of a key this crate
    // refuses.
    for escaped in ["%80", "%FF", "%C3%28", "%ED%A0%80", "%F4%90%80%80"] {
        let key = format!("{}nonutf8-{escaped}.txt", fixture.list_prefix);
        let status = raw_put(&fixture, &key);
        if (200..300).contains(&status) {
            assert!(raw_delete(&fixture, &key) < 300, "left {key} behind");
        }
        assert_eq!(
            status, 400,
            "Azure stored a key that is not UTF-8 ({escaped}), so a listing \
             can carry one and the reader may not refuse a body for it"
        );
    }

    // The query. The prefix is echoed into the page, which is the only way a
    // caller's bytes reach the body without being a key first.
    let echoed = raw_list(&fixture, "&prefix=nonutf8-%80-probe");
    echoed.show("List Blobs, a prefix that is not UTF-8");
    assert_eq!(echoed.status, 200);
    assert!(
        str::from_utf8(&echoed.body).is_ok(),
        "the body Azure wrote is not UTF-8"
    );
    // U+FFFD, where the byte was.
    assert!(
        echoed
            .body
            .windows(3)
            .any(|window| window == [0xEF, 0xBF, 0xBD]),
        "Azure echoed the byte as something other than a replacement character"
    );

    // A control character is valid UTF-8 but XML forbids it. Azure refuses it
    // rather than writing it into the page.
    let refused = raw_list(&fixture, "&prefix=nonutf8-%01-probe");
    refused.show("List Blobs, a prefix XML cannot carry");
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, Some("InvalidQueryParameterValue"))
    );
}
