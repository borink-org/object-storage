use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage::{
    Blobs, ConditionKind, Container, GetHead, GetHeadOutcome, GetKind, GetShape, PhysicalGet,
    RequestedRange, Timestamps, layered,
};

// `#[ignore]` is built into Rust's test harness: ordinary test runs compile but
// skip these tests, while `cargo test -- --ignored` executes them.
const CONTENTS: &[u8] = b"0123456789-azure-get-reference";

#[derive(Debug)]
struct Fixture {
    endpoint: String,
    container: String,
    key: String,
    token: String,
}

impl Fixture {
    fn from_env() -> Self {
        Self {
            endpoint: env::var("AZURE_STORAGE_ENDPOINT").unwrap(),
            container: env::var("AZURE_STORAGE_CONTAINER").unwrap(),
            key: env::var("AZURE_BLOB_KEY").unwrap(),
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
    let get = PhysicalGet {
        key: &fixture.key,
        condition_value,
        shape,
    };
    let mut buf = vec![0; layered::requirements(&blobs, &get, &now)?];
    let request = blobs.encode_get(&mut buf, &get, &now)?;
    let mut outgoing = match request.method() {
        "GET" => ureq::get(request.url()),
        "HEAD" => ureq::head(request.url()),
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
    let head = GetHead::from_headers(
        incoming.status().as_u16(),
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    let (outcome, size, e_tag) = match blobs.accept_get_head(shape, head)? {
        GetHeadOutcome::Body { meta, .. } => (Outcome::Body, meta.size, meta.e_tag),
        GetHeadOutcome::Complete(meta) => (Outcome::Complete, meta.size, meta.e_tag),
        GetHeadOutcome::NotModified { .. } => (Outcome::NotModified, None, None),
        GetHeadOutcome::PreconditionFailed => (Outcome::PreconditionFailed, None, None),
        GetHeadOutcome::NotFound => (Outcome::NotFound, None, None),
        GetHeadOutcome::RangeNotSatisfiable { .. } => (Outcome::RangeNotSatisfiable, None, None),
        GetHeadOutcome::ServiceFailure { status, .. } => {
            (Outcome::ServiceFailure(status), None, None)
        }
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
    condition_kind: ConditionKind::None,
};

fn conditional(kind: ConditionKind) -> GetShape {
    GetShape {
        condition_kind: kind,
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
