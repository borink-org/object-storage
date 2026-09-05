//! Records the responses that `crates/object-storage-proto/tests/fixtures`
//! holds, by asking Azure for them.
//!
//! Every file under that directory is one response as a real storage account
//! sent it. This program puts the objects that provoke each response into a
//! container, sends the request, and writes what came back. Run it to add a
//! response to the corpus, and run it again when a new service version is
//! worth recording: the corpus is then a diff, not a rewrite.
//!
//! See `docs/AZURE-FIXTURES.md` for what to set and how to run it.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage_proto::{
    Blobs, Container, ListEntry, PhysicalList, Timestamps, WireRequest, layered,
};

mod corpus;
mod wire;

use wire::{Request, Response};

/// Everything the corpus writes lives under this prefix, on both accounts.
///
/// It is a constant rather than a setting, because a recorded name is part of
/// the response and so part of what the tests read back. A run that used
/// another prefix would rewrite every file for no reason.
pub const PREFIX: &str = "borink-object-storage/fixtures/";

fn main() {
    if let Err(error) = run() {
        eprintln!("azure-record: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Two identities, because the same request gets a different answer under
    // each and both answers belong in the corpus. See `Account::account_scoped`.
    let token = var("AZURE_STORAGE_ACCESS_TOKEN")?;
    let account_token = var("AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT")?;
    let container = var("AZURE_STORAGE_CONTAINER")?;
    let flat = Account {
        endpoint: var("AZURE_STORAGE_ENDPOINT")?,
        container: container.clone(),
        token: token.clone(),
        account_token: account_token.clone(),
        identity: CONTAINER_SCOPED,
        hierarchical: false,
    };
    let hierarchical = Account {
        endpoint: var("AZURE_HIERARCHICAL_ENDPOINT")?,
        container,
        token,
        account_token,
        identity: CONTAINER_SCOPED,
        hierarchical: true,
    };

    let mut session = Session::new(fixtures_dir()?);
    // Both accounts are recorded in one run, because a group holds files from
    // each and the notes beside them name every file in the group.
    session.empty(&flat)?;
    session.empty(&hierarchical)?;
    corpus::record(&mut session, &flat, &hierarchical)?;
    session.empty(&flat)?;
    session.empty(&hierarchical)?;
    session.write_notes()?;

    println!("azure-record: wrote {} responses", session.written);
    Ok(())
}

fn var(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("{name} is not set").into())
}

// The corpus sits beside the tests that read it, not beside this program.
fn fixtures_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or("the workspace root is not above this crate")?;
    Ok(root.join("crates/object-storage-proto/tests/fixtures"))
}

/// The identity that may write in one container of the account, and read the
/// whole blob service.
pub const CONTAINER_SCOPED: &str = "container-scoped";

/// The identity that may read and write anywhere in the account.
pub const ACCOUNT_SCOPED: &str = "account-scoped";

/// One storage account to record from, as one identity sees it.
pub struct Account {
    endpoint: String,
    container: String,
    token: String,
    // The other identity's token, so that any account can hand out its
    // account-scoped self without carrying the environment around.
    account_token: String,
    /// Which identity this view holds, as the recorded notes name it.
    pub identity: &'static str,
    /// Whether this account has a hierarchical namespace. The two kinds answer
    /// some requests differently, and the corpus holds both answers.
    pub hierarchical: bool,
}

impl Account {
    /// The account's own name, as the recorded notes name it.
    pub fn name(&self) -> &str {
        self.endpoint
            .trim_start_matches("https://")
            .split('.')
            .next()
            .unwrap_or(&self.endpoint)
    }

    /// The crate's view of this account, which encodes its requests.
    pub fn blobs(&self) -> Blobs<'_> {
        Blobs::new(
            Container::new(&self.endpoint, &self.container).unwrap(),
            &self.token,
        )
        .unwrap()
    }

    /// The account with a token that is not one. The service refuses it before
    /// it looks at any identity at all.
    pub fn unauthorized(&self) -> Self {
        Self {
            token: "not-a-token".to_owned(),
            identity: "no valid token",
            ..self.in_container(&self.container)
        }
    }

    /// The same account, seen by the identity that may write anywhere in it.
    ///
    /// Azure settles the grant before it looks for the container, so a write to
    /// a container that is not there answers whichever of the two the identity
    /// has earned: this one is told the container is missing, and the
    /// container-scoped one is refused. Neither answer is a fact about writes,
    /// so the corpus records both.
    pub fn account_scoped(&self) -> Self {
        Self {
            token: self.account_token.clone(),
            identity: ACCOUNT_SCOPED,
            ..self.in_container(&self.container)
        }
    }

    /// The same account, addressed at another container of it.
    pub fn in_container(&self, container: &str) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            container: container.to_owned(),
            token: self.token.clone(),
            account_token: self.account_token.clone(),
            identity: self.identity,
            hierarchical: self.hierarchical,
        }
    }

    /// The URL of `key` in this container, with the key percent-encoded.
    pub fn url(&self, key: &str) -> String {
        format!(
            "{}/{}/{}",
            self.endpoint,
            self.container,
            key.split('/')
                .map(path_segment)
                .collect::<Vec<_>>()
                .join("/")
        )
    }

    /// A request this program writes itself, for an operation the crate does
    /// not encode.
    pub fn raw(&self, method: &str, url: String) -> Request {
        Request::new(method, url)
            .header("authorization", format!("Bearer {}", self.token))
            .header("x-ms-version", borink_object_storage_proto::VERSION)
            .header("x-ms-date", http_date())
    }
}

// Percent-encodes one path segment: a key may hold any character, and the
// request target may hold only some.
fn path_segment(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The current time, as the crate states it in a request head.
pub fn now() -> Timestamps {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs();
    Timestamps::from_unix(unix)
}

fn http_date() -> String {
    // The crate writes this date into every head it encodes, so the one place
    // that spells it is the crate. A raw request borrows it from an encoded
    // one rather than spelling it a second time.
    let blobs = Blobs::new(
        Container::new("https://account.blob.core.windows.net", "container").unwrap(),
        "token",
    )
    .unwrap();
    let list = PhysicalList::new("");
    let mut buf = vec![0; layered::list_requirements(&blobs, &list, &now()).unwrap()];
    let request = blobs.encode_list(&mut buf, &list, &now()).unwrap();
    request
        .headers()
        .find(|(name, _)| *name == "x-ms-date")
        .map(|(_, value)| value.to_owned())
        .expect("an encoded head states the date")
}

/// One recorded response, and what the notes beside it say.
struct Recorded {
    file: String,
    target: String,
    shows: String,
    account: String,
    identity: String,
}

/// A group of recorded responses: one directory of the corpus.
struct Group {
    dir: &'static str,
    heading: &'static str,
    preamble: &'static str,
    recorded: Vec<Recorded>,
}

/// One run of the recorder.
pub struct Session {
    dir: PathBuf,
    groups: Vec<Group>,
    /// The `date` header of the first response, which dates the whole run.
    date: Option<String>,
    written: usize,
}

impl Session {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            groups: Vec::new(),
            date: None,
            written: 0,
        }
    }

    /// Declares a group, and the note that goes above its table.
    pub fn group(&mut self, dir: &'static str, heading: &'static str, preamble: &'static str) {
        self.groups.push(Group {
            dir,
            heading,
            preamble,
            recorded: Vec::new(),
        });
    }

    /// Sends `request` and writes the response into the group last declared.
    ///
    /// `shows` is the line the notes carry for this file. Say what the
    /// response demonstrates, not what the request asked for: the request is
    /// written out beside it.
    pub fn capture(
        &mut self,
        account: &Account,
        file: &str,
        shows: &str,
        request: Request,
    ) -> Result<Response, Box<dyn std::error::Error>> {
        let response = wire::send(&request)?;
        if self.date.is_none() {
            self.date = response
                .header("date")
                .map(|value| String::from_utf8_lossy(value).into_owned());
        }

        let group = self.groups.last_mut().ok_or("no group was declared")?;
        let path = self.dir.join(group.dir).join(format!("{file}.http"));
        fs::create_dir_all(path.parent().expect("a file is in a directory"))?;
        fs::write(&path, serialize(&response))?;
        self.written += 1;

        group.recorded.push(Recorded {
            file: format!("{file}.http"),
            target: target(&request, &account.container),
            shows: shows.to_owned(),
            account: account.name().to_owned(),
            identity: account.identity.to_owned(),
        });
        Ok(response)
    }

    /// Sends a request without recording it: a seed, or a step towards the
    /// state that a later request is recorded against.
    pub fn send(&self, request: Request) -> Result<Response, Box<dyn std::error::Error>> {
        wire::send(&request)
    }

    /// Stores `content` under `key`, and fails if the account refuses it.
    pub fn seed(
        &self,
        account: &Account,
        key: &str,
        content: &[u8],
    ) -> Result<Response, Box<dyn std::error::Error>> {
        self.seed_with(account, key, content, &[])
    }

    /// Stores `content` under `key` with extra request headers, which is how
    /// an object gets the properties that a listing then reports.
    pub fn seed_with(
        &self,
        account: &Account,
        key: &str,
        content: &[u8],
        headers: &[(&str, &str)],
    ) -> Result<Response, Box<dyn std::error::Error>> {
        let mut request = account
            .raw("PUT", account.url(key))
            .header("x-ms-blob-type", "BlockBlob")
            .body(content.to_vec());
        for (name, value) in headers {
            request = request.header(name, *value);
        }
        let response = wire::send(&request)?;
        if response.status != 201 {
            return Err(format!(
                "{key} was not stored: {}\n{}",
                response.status_line,
                String::from_utf8_lossy(&response.body)
            )
            .into());
        }
        Ok(response)
    }

    /// Removes everything under [`PREFIX`], so that a run records the objects
    /// it seeded and nothing a previous run left behind.
    ///
    /// A hierarchical account refuses to remove a directory that still holds
    /// something, so the longest key goes first.
    pub fn empty(&self, account: &Account) -> Result<(), Box<dyn std::error::Error>> {
        let blobs = account.blobs();
        let mut marker: Option<String> = None;
        let mut keys: Vec<String> = Vec::new();
        loop {
            let list = PhysicalList {
                marker: marker.as_deref(),
                ..PhysicalList::new(PREFIX)
            };
            let mut buf = vec![0; layered::list_requirements(&blobs, &list, &now())?];
            let response = wire::send(&encoded(blobs.encode_list(&mut buf, &list, &now())?))?;
            if response.status != 200 {
                return Err(format!(
                    "listing {PREFIX} on {}: {}",
                    account.name(),
                    response.status_line
                )
                .into());
            }

            let mut body = response.body.clone();
            let mut entries = vec![ListEntry::default(); 5000];
            let page = blobs.fill_listing(&mut body, &mut entries)?;
            keys.extend(
                entries[..page.filled]
                    .iter()
                    .map(|entry| entry.key.to_owned()),
            );
            match page.next_marker {
                Some(next) => marker = Some(next.to_owned()),
                None => break,
            }
        }

        keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
        for key in keys {
            let response = wire::send(&account.raw("DELETE", account.url(&key)))?;
            if !matches!(response.status, 202 | 404) {
                return Err(format!("removing {key}: {}", response.status_line).into());
            }
        }
        Ok(())
    }

    // Writes the notes that name every file the run produced.
    fn write_notes(&self) -> Result<(), Box<dyn std::error::Error>> {
        let date = self.date.as_deref().unwrap_or("an unrecorded date");
        for group in &self.groups {
            let mut notes = format!("# {}\n\n{}\n\n", group.heading, group.preamble);
            notes.push_str(&format!(
                "Every file here is one response as the account sent it on {date}, under service \
                 version `{}`. `tests/azure-record` seeded the objects, sent the request and wrote \
                 what came back: the status line, the headers in the order they arrived, a blank \
                 line, and the body, byte-order mark included and to the last byte. A body that \
                 arrived in chunks is joined; the header that records the framing is kept as it \
                 arrived. Nothing in them is a secret. A request identifier names a request that \
                 is over, and the accounts hold nothing but this suite's own keys.\n\n\
                 Do not edit these files. `docs/AZURE-FIXTURES.md` says how to record them \
                 again.\n\n",
                borink_object_storage_proto::VERSION
            ));
            notes.push_str(
                "| file | request | account | identity | what it shows |\n|---|---|---|---|---|\n",
            );
            for recorded in &group.recorded {
                notes.push_str(&format!(
                    "| `{}` | `{}` | `{}` | {} | {} |\n",
                    recorded.file,
                    recorded.target,
                    recorded.account,
                    recorded.identity,
                    recorded.shows
                ));
            }
            fs::write(self.dir.join(group.dir).join("README.md"), notes)?;
        }
        Ok(())
    }
}

/// Turns a request the crate encoded into one this program can send.
pub fn encoded(request: WireRequest<'_>) -> Request {
    let mut out = Request::new(request.method().as_str(), request.url());
    for (name, value) in request.headers() {
        out = out.header(name, value);
    }
    out.body = request.payload().bytes().unwrap_or_default().to_vec();
    out
}

// The request as the notes name it: the method and the target, with the
// account and container taken off so that what is left is the operation.
fn target(request: &Request, container: &str) -> String {
    let rest = request
        .url
        .split_once("://")
        .map_or(request.url.as_str(), |(_, rest)| rest);
    let path = rest.split_once('/').map_or("", |(_, path)| path);
    // A request that names another container keeps that name, so the path is
    // taken off only when it is the container this run records into.
    let path = path.strip_prefix(container).unwrap_or(path);
    format!("{} /{}", request.method, path.trim_start_matches('/'))
}

// The file: the status line, the headers, a blank line, and the body.
fn serialize(response: &Response) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(response.status_line.as_bytes());
    out.push(b'\n');
    for (name, value) in &response.headers {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.push(b'\n');
    }
    out.push(b'\n');
    out.extend_from_slice(&response.body);
    out
}
