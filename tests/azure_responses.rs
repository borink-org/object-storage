//! Azure response interpretation fixtures.

use borink_object_storage::{
    AzureError, AzureErrorKind, Blobs, Container, Error, GetOptions, Response,
};

fn classify(status: u16, headers: &[(&str, &str)], body: &[u8]) -> AzureError {
    let blobs = Blobs::new(
        Container::new("https://account", "container").unwrap(),
        "token",
    )
    .unwrap();
    let error = blobs
        .interpret_get(Response::new(status, headers, body), &GetOptions::default())
        .unwrap_err();
    let Error::Azure(error) = error else {
        panic!("unexpected error: {error}");
    };
    error
}

#[test]
fn classifies_the_offline_azure_response_corpus() {
    let cases = [
        ("BlobNotFound", AzureErrorKind::NotFound),
        ("ResourceNotFound", AzureErrorKind::NotFound),
        ("ContainerNotFound", AzureErrorKind::NoSuchContainer),
        ("BlobAlreadyExists", AzureErrorKind::AlreadyExists),
        ("ConditionNotMet", AzureErrorKind::Precondition),
        ("TargetConditionNotMet", AzureErrorKind::Precondition),
        ("ServerBusy", AzureErrorKind::Throttled),
        ("OperationTimedOut", AzureErrorKind::Timeout),
        ("AuthenticationFailed", AzureErrorKind::Unauthorized),
        (
            "AuthorizationPermissionMismatch",
            AzureErrorKind::Unauthorized,
        ),
        ("InternalError", AzureErrorKind::Service),
        ("ServiceUnavailable", AzureErrorKind::Service),
    ];

    for (code, expected) in cases {
        assert_eq!(
            classify(400, &[("x-ms-error-code", code)], b"").kind(),
            expected
        );
    }
    assert_eq!(
        classify(404, &[], b"<Error><Code>BlobNotFound</Code></Error>").kind(),
        AzureErrorKind::NotFound
    );
    assert_eq!(
        classify(404, &[("x-ms-error-code", "FutureAzureCode")], b"").kind(),
        AzureErrorKind::Unrecognized
    );
}

#[test]
fn preserves_the_azure_request_id() {
    let headers = [
        ("x-ms-error-code", "BlobNotFound"),
        ("x-ms-request-id", "request-123"),
    ];
    let error = classify(404, &headers, b"");
    assert_eq!(error.status(), 404);
    assert_eq!(error.request_id().as_str(), "request-123");
}

#[test]
fn classifies_status_when_azure_sends_no_code() {
    for (status, expected) in [
        (304, AzureErrorKind::NotModified),
        (412, AzureErrorKind::Precondition),
        (416, AzureErrorKind::RangeNotSatisfiable),
        (429, AzureErrorKind::Throttled),
        (500, AzureErrorKind::Service),
    ] {
        assert_eq!(classify(status, &[], b"").kind(), expected);
    }
}
