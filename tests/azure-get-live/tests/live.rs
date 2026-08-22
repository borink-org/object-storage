use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage::{
    AzureErrorKind, Blobs, Container, Error, GetCondition, GetOptions, GetRange, RequestWorkspace,
    Response, Timestamps,
};
use object_store::ObjectStoreExt;
use object_store::azure::MicrosoftAzureBuilder;
use object_store::path::Path;

#[derive(Debug)]
struct OwnedGet {
    bytes: Vec<u8>,
    size: u64,
    last_modified_ms: Option<u64>,
    e_tag: Option<String>,
}

fn get(
    blobs: &Blobs<'_>,
    key: &str,
    options: &GetOptions<'_>,
) -> Result<OwnedGet, Box<dyn std::error::Error>> {
    let now = Timestamps::from_unix(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let required = blobs.get_request_requirements(key, options)?;
    let mut storage = vec![0; required.packed];
    let mut workspace = RequestWorkspace::new(&mut storage);
    let request = blobs.get_request(&mut workspace, key, options, &now)?;
    let mut outgoing = match request.method() {
        "GET" => ureq::get(request.url()),
        "HEAD" => ureq::head(request.url()),
        _ => unreachable!(),
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
    let headers = incoming
        .headers()
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let body = incoming.body_mut().read_to_vec()?;
    let headers = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let (size, last_modified_ms, e_tag) = {
        let meta = blobs.interpret_get(Response::new(status, &headers, &body), options)?;
        (
            meta.size,
            meta.last_modified_ms,
            meta.e_tag.map(str::to_owned),
        )
    };
    Ok(OwnedGet {
        bytes: body,
        size,
        last_modified_ms,
        e_tag,
    })
}

fn error_kind(error: &(dyn std::error::Error + 'static)) -> AzureErrorKind {
    let Error::Azure(error) = error.downcast_ref::<Error>().expect("core error") else {
        panic!("unexpected core error");
    };
    error.kind()
}

#[tokio::test]
#[ignore = "requires the manually configured Azure test account"]
async fn get_matches_object_store() {
    let account = env::var("BORINK_AZURE_ACCOUNT").unwrap();
    let container = env::var("BORINK_AZURE_CONTAINER").unwrap();
    let token = env::var("BORINK_AZURE_BEARER").unwrap();
    let endpoint = format!("https://{account}.blob.core.windows.net");
    let reference = MicrosoftAzureBuilder::new()
        .with_account(&account)
        .with_container_name(&container)
        .with_bearer_token_authorization(&token)
        .build()
        .unwrap();
    let blobs = Blobs::new(Container::new(&endpoint, &container).unwrap(), &token).unwrap();

    let prefix = "borink-object-storage/azure-get-reference";
    let location = Path::parse(format!("{prefix}/a key+é.txt")).unwrap();
    let empty = Path::parse(format!("{prefix}/empty.bin")).unwrap();
    let missing = Path::parse(format!("{prefix}/missing.bin")).unwrap();
    let contents = b"0123456789-azure-get-reference";
    let reference_meta = reference.head(&location).await.unwrap();
    let reference_body = reference
        .get(&location)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(reference_body.as_ref(), contents);

    let full = get(&blobs, location.as_ref(), &GetOptions::default()).unwrap();
    assert_eq!(full.bytes, contents);
    assert_eq!(full.size, reference_meta.size);
    assert_eq!(full.e_tag, reference_meta.e_tag);
    assert_eq!(
        full.last_modified_ms,
        Some(reference_meta.last_modified.timestamp_millis() as u64)
    );

    let range = get(
        &blobs,
        location.as_ref(),
        &GetOptions {
            range: Some(GetRange::Bounded(2..11)),
            ..GetOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        range.bytes,
        reference.get_range(&location, 2..11).await.unwrap()
    );
    assert_eq!(range.size, reference_meta.size);

    let e_tag = reference_meta.e_tag.as_deref().unwrap();
    let head = get(
        &blobs,
        location.as_ref(),
        &GetOptions {
            condition: GetCondition::IfMatch(e_tag),
            head: true,
            ..GetOptions::default()
        },
    )
    .unwrap();
    assert!(head.bytes.is_empty());
    assert_eq!(head.size, reference_meta.size);

    let stale = get(
        &blobs,
        location.as_ref(),
        &GetOptions {
            condition: GetCondition::IfMatch("\"stale\""),
            ..GetOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(error_kind(stale.as_ref()), AzureErrorKind::Precondition);

    assert!(
        get(&blobs, empty.as_ref(), &GetOptions::default())
            .unwrap()
            .bytes
            .is_empty()
    );
    let missing_error = get(&blobs, missing.as_ref(), &GetOptions::default()).unwrap_err();
    assert_eq!(error_kind(missing_error.as_ref()), AzureErrorKind::NotFound);
    assert!(matches!(
        reference.get(&missing).await.unwrap_err(),
        object_store::Error::NotFound { .. }
    ));
}
