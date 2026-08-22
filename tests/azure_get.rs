use borink_object_storage::{
    Blobs, Container, Error, RequestWorkspace, Response, Timestamps, VERSION, WorkspaceExtent,
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
    let now = now();
    let request = blobs
        .get_request(&mut workspace, "directory/a key+é", &now)
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
    let mut none = [];
    let error = blobs
        .get_request(&mut RequestWorkspace::new(&mut none), "object", &now())
        .unwrap_err();
    let Error::Capacity(capacity) = error else {
        panic!("unexpected error: {error}");
    };
    assert_eq!(capacity.extent, WorkspaceExtent::Packed);

    let mut exact = vec![0; capacity.required];
    blobs
        .get_request(&mut RequestWorkspace::new(&mut exact), "object", &now())
        .unwrap();
}

#[test]
fn returns_borrowed_response_bytes() {
    let blobs = blobs();
    let body = b"contents";
    let result = blobs.interpret_get(Response::new(200, body)).unwrap();
    assert!(core::ptr::eq(result.as_ptr(), body.as_ptr()));
}

#[test]
fn classifies_basic_response_errors() {
    let blobs = blobs();
    assert_eq!(
        blobs.interpret_get(Response::new(404, b"")),
        Err(Error::NotFound)
    );
    assert_eq!(
        blobs.interpret_get(Response::new(403, b"")),
        Err(Error::Unauthorized)
    );
}

#[test]
fn rejects_values_that_could_change_the_http_request() {
    // Azure endpoints require HTTP(S).
    assert!(matches!(
        Container::new("file://account", "objects"),
        Err(Error::InvalidEndpoint)
    ));
    // An endpoint is an origin, not a URL prefix.
    assert!(matches!(
        Container::new("https://account/path", "objects"),
        Err(Error::InvalidEndpoint)
    ));
    // The minimal validator only accepts ASCII origins.
    assert!(matches!(
        Container::new("https://tést.example", "objects"),
        Err(Error::InvalidEndpoint)
    ));
    // Container text cannot introduce a query string.
    assert!(matches!(
        Container::new("https://account", "object?restype=container"),
        Err(Error::InvalidContainer)
    ));
    let container = Container::new("https://account", "objects").unwrap();
    assert!(matches!(
        Blobs::new(container, "token\r\nheader"),
        Err(Error::InvalidToken)
    ));
}

#[test]
fn formats_azure_request_timestamps_without_allocation() {
    for (unix, expected) in [
        (0, "Thu, 01 Jan 1970 00:00:00 GMT"),
        (951_782_400, "Tue, 29 Feb 2000 00:00:00 GMT"),
        (1_369_353_600, "Fri, 24 May 2013 00:00:00 GMT"),
        (253_402_300_799, "Fri, 31 Dec 9999 23:59:59 GMT"),
    ] {
        let timestamp = Timestamps::from_unix(unix);
        assert_eq!(timestamp.unix(), unix);
        assert_eq!(timestamp.rfc1123(), expected);
    }
}
