//! Azure response interpretation fixtures.

use borink_object_storage::{
    Blobs, BodyWindow, Classification, ConditionKind, Container, Error, FailureClass, GetHead,
    GetHeadOutcome, GetKind, GetShape, ObjectMeta, RequestedRange, ServiceErrorKind,
    classify_error, layered,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account", "container").unwrap(),
        "token",
    )
    .unwrap()
}

fn accept<'h>(shape: GetShape, head: GetHead<'h>) -> Result<GetHeadOutcome<'h>, Error> {
    blobs().accept_get_head(shape, head)
}

fn ranged(range: RequestedRange) -> GetShape {
    GetShape {
        range,
        ..GetShape::default()
    }
}

fn conditional(condition: ConditionKind) -> GetShape {
    GetShape {
        condition,
        ..GetShape::default()
    }
}

#[test]
fn accepts_a_whole_object_read() {
    let head = GetHead::from_headers(
        200,
        [
            ("Content-Length", b"8".as_slice()),
            ("ETag", b"\"etag\""),
            ("Last-Modified", b"Fri, 24 May 2013 00:00:00 GMT"),
            ("Content-Encoding", b"gzip"),
        ],
    );
    let GetHeadOutcome::Body { meta, body, .. } = accept(GetShape::default(), head).unwrap() else {
        panic!("expected a body");
    };
    assert_eq!(
        meta,
        ObjectMeta {
            size: Some(8),
            e_tag: Some(b"\"etag\""),
            last_modified: Some(b"Fri, 24 May 2013 00:00:00 GMT"),
            version: None,
            // Ranges cover the stored representation, so an encoding is
            // surfaced rather than rejected.
            content_encoding: Some(b"gzip"),
        }
    );
    assert_eq!(
        body,
        BodyWindow {
            object_offset: 0,
            expected_len: Some(8),
            object_size: Some(8),
        }
    );
    assert_eq!(
        meta.last_modified.and_then(layered::http_date_ms),
        Some(1_369_353_600_000)
    );
}

#[test]
fn a_metadata_plan_completes_without_a_body() {
    let shape = GetShape {
        kind: GetKind::Metadata,
        ..GetShape::default()
    };
    let head = GetHead::from_headers(200, [("Content-Length", b"8".as_slice())]);
    assert_eq!(
        accept(shape, head),
        Ok(GetHeadOutcome::Complete {
            meta: ObjectMeta {
                size: Some(8),
                ..ObjectMeta::default()
            }
        })
    );
}

#[test]
fn conditional_statuses_need_the_condition_that_explains_them() {
    let not_modified = GetHead::from_headers(304, [("ETag", b"\"etag\"".as_slice())]);
    assert_eq!(
        accept(conditional(ConditionKind::IfNoneMatch), not_modified),
        Ok(GetHeadOutcome::NotModified {
            e_tag: Some(b"\"etag\"")
        })
    );
    assert_eq!(
        accept(conditional(ConditionKind::IfMatch), GetHead::new(412)),
        Ok(GetHeadOutcome::PreconditionFailed)
    );

    // Nothing in an unconditional plan explains either status.
    assert!(matches!(
        accept(GetShape::default(), not_modified),
        Err(Error::Protocol(_))
    ));
    assert!(matches!(
        accept(GetShape::default(), GetHead::new(412)),
        Err(Error::Protocol(_))
    ));
}

#[test]
fn ranged_and_unranged_plans_must_be_answered_in_kind() {
    let bounded = ranged(RequestedRange::Bounded { start: 2, end: 6 });
    assert!(matches!(
        accept(
            bounded,
            GetHead::from_headers(200, [("Content-Length", b"10".as_slice())]),
        ),
        Err(Error::ResponseMismatch(_))
    ));
    assert!(matches!(
        accept(
            GetShape::default(),
            GetHead::from_headers(206, [("Content-Range", b"bytes 0-9/10".as_slice())]),
        ),
        Err(Error::ResponseMismatch(_))
    ));
    assert!(matches!(
        accept(bounded, GetHead::new(206)),
        Err(Error::ResponseMismatch(_))
    ));
}

#[test]
fn enforces_maximal_satisfaction_of_the_requested_range() {
    let bounded = ranged(RequestedRange::Bounded { start: 2, end: 6 });
    let head = |value: &'static [u8]| GetHead::from_headers(206, [("Content-Range", value)]);

    // The request is served whole, and a request past EOF clamps at the size.
    assert!(accept(bounded, head(b"bytes 2-5/10")).is_ok());
    assert!(accept(bounded, head(b"bytes 2-3/4")).is_ok());
    assert!(accept(ranged(RequestedRange::Offset(2)), head(b"bytes 2-9/10")).is_ok());

    for short in [b"bytes 2-4/10".as_slice(), b"bytes 3-5/10"] {
        assert!(
            matches!(
                accept(bounded, head(short)),
                Err(Error::ResponseMismatch(_))
            ),
            "{short:?}"
        );
    }
    assert!(matches!(
        accept(ranged(RequestedRange::Offset(2)), head(b"bytes 2-8/10")),
        Err(Error::ResponseMismatch(_))
    ));
}

#[test]
fn rejects_content_ranges_that_are_not_arithmetically_sound() {
    let bounded = ranged(RequestedRange::Bounded { start: 2, end: 6 });
    for value in [
        b"bytes 5-2/10".as_slice(),
        b"bytes 2-10/10",
        b"bytes 2-5",
        b"bytes 2-x/10",
        // `bytes */N` answers a 416 and nothing else.
        b"bytes */10",
    ] {
        assert!(
            matches!(
                accept(
                    bounded,
                    GetHead::from_headers(206, [("Content-Range", value)])
                ),
                Err(Error::Protocol(_))
            ),
            "{value:?}"
        );
    }
    assert!(matches!(
        accept(
            bounded,
            GetHead::from_headers(
                206,
                [
                    ("Content-Range", b"bytes 2-5/10".as_slice()),
                    ("Content-Length", b"9"),
                ],
            ),
        ),
        Err(Error::Protocol(_))
    ));
}

#[test]
fn a_416_carries_the_object_size_when_azure_states_it() {
    let shape = ranged(RequestedRange::Offset(40));
    assert_eq!(
        accept(
            shape,
            GetHead::from_headers(416, [("Content-Range", b"bytes */10".as_slice())]),
        ),
        Ok(GetHeadOutcome::RangeNotSatisfiable {
            object_size: Some(10)
        })
    );
    assert_eq!(
        accept(shape, GetHead::new(416)),
        Ok(GetHeadOutcome::RangeNotSatisfiable { object_size: None })
    );
}

#[test]
fn every_other_status_is_a_service_failure_a_scheduler_can_branch_on() {
    // A 404 that names the container separates it from a missing object.
    let mut missing = GetHead::new(404);
    missing.error_code = Some(b"ContainerNotFound");
    assert_eq!(
        accept(GetShape::default(), missing),
        Ok(GetHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        })
    );
    // A code this crate does not know is still decisive: Azure repeats the
    // header in the body, so there is nothing more to read.
    missing.error_code = Some(b"FutureAzureCode");
    assert_eq!(
        accept(GetShape::default(), missing),
        Ok(GetHeadOutcome::NotFound { kind: None })
    );
    for (status, code, class, kind) in [
        (
            403,
            b"AuthenticationFailed".as_slice(),
            FailureClass::Auth,
            Some(ServiceErrorKind::Unauthorized),
        ),
        (
            500,
            b"InternalError",
            FailureClass::Server,
            Some(ServiceErrorKind::Service),
        ),
        (302, b"FutureAzureCode", FailureClass::Redirect, None),
        (400, b"FutureAzureCode", FailureClass::Other, None),
        // Azure's own code refines an otherwise unhelpful status, and the
        // outcome keeps it so that no caller classifies the response twice.
        (
            400,
            b"ServerBusy",
            FailureClass::Throttled,
            Some(ServiceErrorKind::Throttled),
        ),
    ] {
        let mut head =
            GetHead::from_headers(status, [("x-ms-request-id", b"request-123".as_slice())]);
        head.error_code = Some(code);
        assert!(
            matches!(
                accept(GetShape::default(), head),
                Ok(GetHeadOutcome::ServiceFailure {
                    status: got_status,
                    class: got_class,
                    kind: got_kind,
                    request_id: Some(b"request-123"),
                    ..
                }) if got_status == status && got_class == class && got_kind == kind
            ),
            "{status} {code:?}"
        );
    }
}

#[test]
fn classifies_the_offline_azure_response_corpus() {
    let cases = [
        ("BlobNotFound", ServiceErrorKind::NotFound),
        ("ResourceNotFound", ServiceErrorKind::NotFound),
        ("ContainerNotFound", ServiceErrorKind::NoSuchContainer),
        ("BlobAlreadyExists", ServiceErrorKind::AlreadyExists),
        ("ConditionNotMet", ServiceErrorKind::Precondition),
        ("TargetConditionNotMet", ServiceErrorKind::Precondition),
        ("InvalidRange", ServiceErrorKind::RangeNotSatisfiable),
        ("ServerBusy", ServiceErrorKind::Throttled),
        ("OperationTimedOut", ServiceErrorKind::Timeout),
        ("AuthenticationFailed", ServiceErrorKind::Unauthorized),
        (
            "AuthorizationPermissionMismatch",
            ServiceErrorKind::Unauthorized,
        ),
        ("InternalError", ServiceErrorKind::Service),
        ("ServiceUnavailable", ServiceErrorKind::Service),
    ];

    for (code, expected) in cases {
        let head = GetHead::from_headers(400, [("x-ms-error-code", code.as_bytes())]);
        assert_eq!(
            classify_error(&head, b"", false),
            Classification::Classified(expected),
            "{code}"
        );
    }
}

#[test]
fn falls_back_to_the_xml_error_body() {
    let head = GetHead::new(404);
    assert_eq!(
        classify_error(&head, b"<Error><Code>BlobNotFound</Code></Error>", false),
        Classification::Classified(ServiceErrorKind::NotFound)
    );
    // A complete body naming a code this crate does not know, and a body the
    // host's cap cut short, are different answers.
    assert_eq!(
        classify_error(&head, b"<Error><Code>FutureAzureCode</Code>", false),
        Classification::Unknown
    );
    assert_eq!(
        classify_error(&head, b"<Error><Cod", true),
        Classification::Incomplete
    );
}

#[test]
fn a_head_without_a_code_asks_for_the_error_body() {
    let blobs = blobs();
    for status in [404u16, 403, 500] {
        let head = GetHead::from_headers(status, [("x-ms-request-id", b"request-123".as_slice())]);
        let expected = match status {
            404 => FailureClass::Other,
            403 => FailureClass::Auth,
            _ => FailureClass::Server,
        };
        assert!(
            matches!(
                blobs.accept_get_head(GetShape::default(), head),
                Ok(GetHeadOutcome::NeedErrorBody {
                    status: got_status,
                    class,
                    request_id: Some(b"request-123"),
                    ..
                }) if got_status == status && class == expected
            ),
            "{status}"
        );
    }
}

#[test]
fn the_error_body_names_an_error_the_head_left_out() {
    let blobs = blobs();
    let missing = blobs
        .accept_get_head(GetShape::default(), GetHead::new(404))
        .unwrap();
    assert_eq!(
        blobs.accept_error_body(missing, b"<Error><Code>ContainerNotFound</Code></Error>"),
        GetHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        }
    );

    // A code that arrives in the body refines the category, as a code in the
    // header would have.
    let refused = blobs
        .accept_get_head(GetShape::default(), GetHead::new(400))
        .unwrap();
    assert!(matches!(
        blobs.accept_error_body(refused, b"<Error><Code>ServerBusy</Code></Error>"),
        GetHeadOutcome::ServiceFailure {
            class: FailureClass::Throttled,
            kind: Some(ServiceErrorKind::Throttled),
            ..
        }
    ));

    // A host that could read no body still gets a final outcome, with the
    // error unnamed.
    assert_eq!(
        blobs.accept_error_body(missing, b""),
        GetHeadOutcome::NotFound { kind: None }
    );

    // Every other outcome is already final.
    let named = blobs
        .accept_get_head(
            GetShape::default(),
            GetHead::from_headers(404, [("x-ms-error-code", b"BlobNotFound".as_slice())]),
        )
        .unwrap();
    assert_eq!(
        blobs.accept_error_body(named, b"<Error><Code>ContainerNotFound</Code></Error>"),
        named
    );
}
