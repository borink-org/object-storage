//! Azure bearer GET integration tests.

use borink_object_storage::{
    AzureErrorKind, Blobs, Container, Error, GetCondition, GetOptions, GetRange, RequestWorkspace,
    Response, Timestamps, VERSION, WorkspaceExtent,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "objects").unwrap(),
        "access-token",
    )
    .unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

#[test]
fn builds_a_bearer_get_in_caller_memory() {
    let blobs = blobs();
    let mut storage = [0; 256];
    let mut workspace = RequestWorkspace::new(&mut storage);
    let options = GetOptions::default();
    let now = now();
    let request = blobs
        .get_request(&mut workspace, "directory/a key+é", &options, &now)
        .unwrap();

    assert_eq!(request.method(), "GET");
    assert_eq!(
        request.url(),
        "https://account.blob.core.windows.net/objects/directory/a%20key%2B%C3%A9"
    );
    assert_eq!(
        request.headers().collect::<Vec<_>>(),
        [
            ("authorization", "Bearer access-token"),
            ("x-ms-date", "Sat, 22 Aug 2026 12:00:00 GMT"),
            ("x-ms-version", VERSION),
        ]
    );
}

#[test]
fn reports_the_exact_required_capacity() {
    let blobs = blobs();
    let options = GetOptions::default();
    let mut none = [];
    let error = blobs
        .get_request(
            &mut RequestWorkspace::new(&mut none),
            "object",
            &options,
            &now(),
        )
        .unwrap_err();
    let Error::Capacity(capacity) = error else {
        panic!("unexpected error: {error}");
    };
    assert_eq!(capacity.extent, WorkspaceExtent::Packed);

    let mut exact = vec![0; capacity.required];
    blobs
        .get_request(
            &mut RequestWorkspace::new(&mut exact),
            "object",
            &options,
            &now(),
        )
        .unwrap();
}

#[test]
fn classifies_response_metadata_and_errors() {
    let blobs = blobs();
    let options = GetOptions::default();
    let headers = [("Content-Length", "8"), ("ETag", "\"etag\"")];
    let meta = blobs
        .interpret_get(Response::new(200, &headers, b""), &options)
        .unwrap();
    assert_eq!(meta.size, 8);
    assert_eq!(meta.e_tag, Some("\"etag\""));
    for (status, expected) in [
        (404, AzureErrorKind::NotFound),
        (403, AzureErrorKind::Unauthorized),
        (304, AzureErrorKind::NotModified),
    ] {
        let Err(Error::Azure(error)) =
            blobs.interpret_get(Response::new(status, &[], b""), &options)
        else {
            panic!("expected an Azure error");
        };
        assert_eq!(error.kind(), expected);
    }
}

#[test]
fn adds_ranges_conditions_and_head() {
    let blobs = blobs();
    let mut storage = [0; 256];
    let options = GetOptions {
        range: Some(GetRange::Bounded(2..6)),
        condition: GetCondition::IfMatch("\"etag\""),
        head: true,
    };
    let mut workspace = RequestWorkspace::new(&mut storage);
    let now = now();
    let request = blobs
        .get_request(&mut workspace, "object", &options, &now)
        .unwrap();
    assert_eq!(request.method(), "HEAD");
    assert!(
        request
            .headers()
            .any(|header| header == ("range", "bytes=2-5"))
    );
    assert!(
        request
            .headers()
            .any(|header| header == ("if-match", "\"etag\""))
    );

    let headers = [
        ("Content-Range", "bytes 2-5/10"),
        ("ETag", "\"etag\""),
        ("x-ms-version-id", "version-1"),
        ("Last-Modified", "Fri, 24 May 2013 00:00:00 GMT"),
    ];
    let meta = blobs
        .interpret_get(Response::new(206, &headers, b""), &options)
        .unwrap();
    assert_eq!(meta.size, 10);
    assert_eq!(meta.e_tag, Some("\"etag\""));
    assert_eq!(meta.version, Some("version-1"));
    assert_eq!(meta.last_modified_ms, Some(1_369_353_600_000));
}

#[test]
fn rejects_values_that_could_change_the_http_request() {
    assert!(matches!(
        Container::new("file://account", "objects"),
        Err(Error::InvalidEndpoint)
    ));
    assert!(matches!(
        Container::new("https://account/path", "objects"),
        Err(Error::InvalidEndpoint)
    ));
    assert!(matches!(
        Container::new("https://tést.example", "objects"),
        Err(Error::InvalidEndpoint)
    ));
    assert!(matches!(
        Container::new("https://account", "object?restype=container"),
        Err(Error::InvalidContainer)
    ));
    let container = Container::new("https://account", "objects").unwrap();
    assert!(matches!(
        Blobs::new(container, "token\r\nheader"),
        Err(Error::InvalidToken)
    ));

    let options = GetOptions {
        condition: GetCondition::IfMatch("etag\r\nheader"),
        ..GetOptions::default()
    };
    assert_eq!(
        blobs().get_request_requirements("object", &options),
        Err(Error::InvalidCondition)
    );
    let range_start = 6;
    let range_end = 2;
    let options = GetOptions {
        range: Some(GetRange::Bounded(range_start..range_end)),
        ..GetOptions::default()
    };
    assert_eq!(
        blobs().get_request_requirements("object", &options),
        Err(Error::InvalidRange)
    );
    let options = GetOptions {
        range: Some(GetRange::Suffix(4)),
        ..GetOptions::default()
    };
    assert!(matches!(
        blobs().get_request_requirements("object", &options),
        Err(Error::Unsupported(_))
    ));
}
