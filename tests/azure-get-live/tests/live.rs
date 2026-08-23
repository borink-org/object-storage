use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage::{
    Blobs, Container, Error, GetOptions, GetRange, RequestWorkspace, Response, Timestamps,
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
    options: &GetOptions<'_>,
) -> Result<ReadResult, Box<dyn std::error::Error>> {
    let now = Timestamps::from_unix(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let blobs = fixture.blobs();
    let required = blobs.get_request_requirements(&fixture.key, options)?;
    let mut storage = vec![0; required.packed];
    let mut workspace = RequestWorkspace::new(&mut storage);
    let request = blobs.get_request(&mut workspace, &fixture.key, options, &now)?;
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
            options,
        )?;
        (meta.size, meta.e_tag.map(str::to_owned))
    };
    let body = incoming.body_mut().read_to_vec()?;
    Ok(ReadResult { body, size, e_tag })
}

fn error(result: Result<ReadResult, Box<dyn std::error::Error>>) -> Error {
    *result.unwrap_err().downcast::<Error>().expect("core error")
}

#[test]
#[ignore = "requires Azure credentials"]
fn gets_the_complete_blob() {
    let result = read(&Fixture::from_env(), &GetOptions::default()).unwrap();
    assert_eq!(result.body, CONTENTS);
    assert_eq!(result.size, CONTENTS.len() as u64);
}

#[test]
#[ignore = "requires Azure credentials"]
fn gets_a_bounded_range() {
    let options = GetOptions {
        range: Some(GetRange::Bounded(2..11)),
        ..GetOptions::default()
    };
    let result = read(&Fixture::from_env(), &options).unwrap();
    assert_eq!(result.body, &CONTENTS[2..11]);
    assert_eq!(result.size, CONTENTS.len() as u64);
}

#[test]
#[ignore = "requires Azure credentials"]
fn heads_the_blob() {
    let options = GetOptions {
        head: true,
        ..GetOptions::default()
    };
    let result = read(&Fixture::from_env(), &options).unwrap();
    assert!(result.body.is_empty());
    assert_eq!(result.size, CONTENTS.len() as u64);
    assert!(result.e_tag.is_some());
}

#[test]
#[ignore = "requires Azure credentials"]
fn applies_if_match() {
    let fixture = Fixture::from_env();
    let e_tag = read(
        &fixture,
        &GetOptions {
            head: true,
            ..GetOptions::default()
        },
    )
    .unwrap()
    .e_tag
    .unwrap();
    let matching = GetOptions {
        if_match: Some(&e_tag),
        ..GetOptions::default()
    };
    assert_eq!(read(&fixture, &matching).unwrap().body, CONTENTS);

    let stale = GetOptions {
        if_match: Some("\"stale\""),
        ..GetOptions::default()
    };
    assert_eq!(error(read(&fixture, &stale)), Error::Precondition);
}

#[test]
#[ignore = "requires Azure credentials"]
fn applies_if_none_match() {
    let fixture = Fixture::from_env();
    let e_tag = read(
        &fixture,
        &GetOptions {
            head: true,
            ..GetOptions::default()
        },
    )
    .unwrap()
    .e_tag
    .unwrap();
    let matching = GetOptions {
        if_none_match: Some(&e_tag),
        ..GetOptions::default()
    };
    assert_eq!(error(read(&fixture, &matching)), Error::NotModified);

    let stale = GetOptions {
        if_none_match: Some("\"stale\""),
        ..GetOptions::default()
    };
    assert_eq!(read(&fixture, &stale).unwrap().body, CONTENTS);
}
