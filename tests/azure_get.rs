use borink_object_storage::{Blobs, Container, Error, RequestWorkspace, Response, VERSION};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "objects").unwrap(),
        "access-token",
    )
    .unwrap()
}

#[test]
fn builds_a_bearer_get_in_caller_memory() {
    let blobs = blobs();
    let mut storage = [0; 256];
    let mut workspace = RequestWorkspace::new(&mut storage);
    let request = blobs
        .get_request(
            &mut workspace,
            "directory/a key+é",
            "Sat, 22 Aug 2026 12:00:00 GMT",
        )
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
        .get_request(
            &mut RequestWorkspace::new(&mut none),
            "object",
            "Sat, 22 Aug 2026 12:00:00 GMT",
        )
        .unwrap_err();
    let Error::BufferTooSmall { required, .. } = error else {
        panic!("unexpected error: {error}");
    };

    let mut exact = vec![0; required];
    blobs
        .get_request(
            &mut RequestWorkspace::new(&mut exact),
            "object",
            "Sat, 22 Aug 2026 12:00:00 GMT",
        )
        .unwrap();
}

#[test]
fn returns_borrowed_bytes_and_basic_errors() {
    let blobs = blobs();
    let body = b"contents";
    let result = blobs.interpret_get(Response::new(200, body)).unwrap();
    assert!(core::ptr::eq(result.as_ptr(), body.as_ptr()));
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
    assert!(matches!(
        Container::new("file://account", "objects"),
        Err(Error::InvalidEndpoint)
    ));
    assert!(matches!(
        Container::new("https://account/path", "objects"),
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
}
