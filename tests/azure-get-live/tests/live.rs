use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage::{
    Blobs, ConditionKind, Container, Error, GetKind, GetShape, PhysicalGet, RequestedRange,
    Response, Timestamps, layered,
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
    body: Vec<u8>,
    size: u64,
    e_tag: Option<String>,
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
    let status = incoming.status().as_u16();
    let (size, e_tag) = {
        let meta = blobs.interpret_get(
            Response::new(
                status,
                incoming.headers().iter().filter_map(|(name, value)| {
                    value.to_str().ok().map(|value| (name.as_str(), value))
                }),
            ),
            shape,
        )?;
        (meta.size, meta.e_tag.map(str::to_owned))
    };
    let body = incoming.body_mut().read_to_vec()?;
    Ok(ReadResult { body, size, e_tag })
}

fn error(result: Result<ReadResult, Box<dyn std::error::Error>>) -> Error {
    *result.unwrap_err().downcast::<Error>().expect("core error")
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
    assert_eq!(result.body, CONTENTS);
    assert_eq!(result.size, CONTENTS.len() as u64);
}

#[test]
#[ignore = "requires Azure credentials"]
fn gets_a_bounded_range() {
    let shape = GetShape {
        range: RequestedRange::Bounded { start: 2, end: 11 },
        ..GetShape::default()
    };
    let result = read(&Fixture::from_env(), shape, None).unwrap();
    assert_eq!(result.body, &CONTENTS[2..11]);
    assert_eq!(result.size, CONTENTS.len() as u64);
}

#[test]
#[ignore = "requires Azure credentials"]
fn heads_the_blob() {
    let result = read(&Fixture::from_env(), METADATA, None).unwrap();
    assert!(result.body.is_empty());
    assert_eq!(result.size, CONTENTS.len() as u64);
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
        error(read(&fixture, shape, Some(b"\"stale\""))),
        Error::Precondition
    );
}

#[test]
#[ignore = "requires Azure credentials"]
fn applies_if_none_match() {
    let fixture = Fixture::from_env();
    let e_tag = e_tag(&fixture);
    let shape = conditional(ConditionKind::IfNoneMatch);
    assert_eq!(
        error(read(&fixture, shape, Some(e_tag.as_bytes()))),
        Error::NotModified
    );
    assert_eq!(
        read(&fixture, shape, Some(b"\"stale\"")).unwrap().body,
        CONTENTS
    );
}
