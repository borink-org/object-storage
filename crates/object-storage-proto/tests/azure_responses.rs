//! Azure response interpretation fixtures.
//!
//! Every head here that says what the service sends is one of the responses
//! recorded under `tests/fixtures/azure-get`, read back through the head
//! reader. A head written out in this file is one the service does not send:
//! an arithmetic the reader must still refuse, or a value the recording
//! identity cannot provoke. Each of those says so.

mod recorded;

use recorded::Recorded;

use borink_object_storage_proto::{
    Blobs, BodyWindow, Classification, ConditionKind, Container, Error, Failure, FailureClass,
    GetHeadOutcome, GetKind, GetShape, ObjectMeta, RequestedRange, ResponseFault, ResponseHead,
    ServiceErrorKind, classify_error, layered,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account", "container").unwrap(),
        "token",
    )
    .unwrap()
}

fn accept<'h>(shape: GetShape, head: ResponseHead<'h>) -> Result<GetHeadOutcome<'h>, Error> {
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
    let recorded = Recorded::load("azure-get/get-whole");
    let GetHeadOutcome::Body { meta, body, .. } =
        accept(GetShape::default(), recorded.head()).unwrap()
    else {
        panic!("expected a body");
    };
    assert_eq!(
        meta,
        ObjectMeta {
            size: Some(30),
            e_tag: recorded.header("etag"),
            last_modified: recorded.header("last-modified"),
            version: recorded.header("x-ms-version-id"),
            // Ranges cover the stored representation, so an encoding is
            // surfaced rather than rejected.
            content_encoding: Some(b"gzip"),
        }
    );
    assert_eq!(
        body,
        BodyWindow {
            object_offset: 0,
            expected_len: Some(30),
            object_size: Some(30),
        }
    );
    assert!(meta.last_modified.and_then(layered::http_date_ms).is_some());
}

#[test]
fn a_metadata_plan_completes_without_a_body() {
    let shape = GetShape {
        kind: GetKind::Metadata,
        ..GetShape::default()
    };
    // The service answers a metadata plan with the head of the read and no
    // body at all, under the length the body would have had.
    let recorded = Recorded::load("azure-get/head-metadata");
    assert_eq!(
        accept(shape, recorded.head()),
        Ok(GetHeadOutcome::Complete {
            meta: ObjectMeta {
                size: Some(30),
                e_tag: recorded.header("etag"),
                last_modified: recorded.header("last-modified"),
                version: recorded.header("x-ms-version-id"),
                content_encoding: Some(b"gzip"),
            }
        })
    );
}

#[test]
fn conditional_statuses_need_the_condition_that_explains_them() {
    // The service answers `If-None-Match` on an object that has not changed
    // with a `304` that names the condition it failed and carries no entity
    // tag of its own, so the outcome carries none either.
    let not_modified = Recorded::load("azure-get/get-not-modified");
    assert_eq!(not_modified.header("etag"), None);
    assert_eq!(
        not_modified.header("x-ms-error-code"),
        Some(b"ConditionNotMet".as_slice())
    );
    assert_eq!(
        accept(conditional(ConditionKind::IfNoneMatch), not_modified.head()),
        Ok(GetHeadOutcome::NotModified { e_tag: None })
    );
    // A `304` that does carry one hands it on. The recorded responses hold no
    // such head, and the type carries the tag when a service sends it.
    assert_eq!(
        accept(
            conditional(ConditionKind::IfNoneMatch),
            ResponseHead::from_headers(304, [("ETag", b"\"etag\"".as_slice())]),
        ),
        Ok(GetHeadOutcome::NotModified {
            e_tag: Some(b"\"etag\"")
        })
    );

    let failed = Recorded::load("azure-get/get-precondition-failed");
    assert_eq!(
        accept(conditional(ConditionKind::IfMatch), failed.head()),
        Ok(GetHeadOutcome::PreconditionFailed)
    );

    // Nothing in an unconditional plan explains either status.
    assert_eq!(
        accept(GetShape::default(), not_modified.head()),
        Err(Error::Response(ResponseFault::Status))
    );
    assert_eq!(
        accept(GetShape::default(), failed.head()),
        Err(Error::Response(ResponseFault::Status))
    );
}

#[test]
fn ranged_and_unranged_plans_must_be_answered_in_kind() {
    let bounded = ranged(RequestedRange::Bounded { start: 2, end: 6 });
    // The whole-object head, under a plan that asked for part of it.
    assert_eq!(
        accept(bounded, Recorded::load("azure-get/get-whole").head()),
        Err(Error::Response(ResponseFault::Range))
    );
    // The ranged head, under a plan that asked for the whole object.
    assert_eq!(
        accept(
            GetShape::default(),
            Recorded::load("azure-get/get-range").head()
        ),
        Err(Error::Response(ResponseFault::Range))
    );
    // A 206 that names no range leaves the head missing a value it needs. The
    // service always names one, so this head is written here.
    assert_eq!(
        accept(bounded, ResponseHead::new(206)),
        Err(Error::Response(ResponseFault::Head))
    );
}

#[test]
fn enforces_maximal_satisfaction_of_the_requested_range() {
    // The request is served whole: four bytes from the third, out of thirty.
    let bounded = ranged(RequestedRange::Bounded { start: 2, end: 6 });
    let recorded = Recorded::load("azure-get/get-range");
    assert_eq!(
        recorded.header("content-range"),
        Some(b"bytes 2-5/30".as_slice())
    );
    assert!(accept(bounded, recorded.head()).is_ok());

    // A request whose end is past the end of the object clamps at the size,
    // and that is still maximal satisfaction of it.
    let past = ranged(RequestedRange::Bounded { start: 28, end: 64 });
    let recorded = Recorded::load("azure-get/get-range-past-the-end");
    assert_eq!(
        recorded.header("content-range"),
        Some(b"bytes 28-29/30".as_slice())
    );
    assert!(accept(past, recorded.head()).is_ok());

    // A service that returned less than it could is refused. Azure returns
    // what it can, so these heads are written here.
    let head = |value: &'static [u8]| ResponseHead::from_headers(206, [("Content-Range", value)]);
    assert!(accept(ranged(RequestedRange::Offset(2)), head(b"bytes 2-9/10")).is_ok());
    for short in [b"bytes 2-4/10".as_slice(), b"bytes 3-5/10"] {
        assert_eq!(
            accept(bounded, head(short)),
            Err(Error::Response(ResponseFault::Range)),
            "{short:?}"
        );
    }
    assert_eq!(
        accept(ranged(RequestedRange::Offset(2)), head(b"bytes 2-8/10")),
        Err(Error::Response(ResponseFault::Range))
    );
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
        assert_eq!(
            accept(
                bounded,
                ResponseHead::from_headers(206, [("Content-Range", value)])
            ),
            Err(Error::Response(ResponseFault::Head)),
            "{value:?}"
        );
    }
    assert_eq!(
        accept(
            bounded,
            ResponseHead::from_headers(
                206,
                [
                    ("Content-Range", b"bytes 2-5/10".as_slice()),
                    ("Content-Length", b"9"),
                ],
            ),
        ),
        Err(Error::Response(ResponseFault::Head))
    );
}

#[test]
fn a_416_carries_the_object_size_when_azure_states_it() {
    let shape = ranged(RequestedRange::Offset(40));
    let recorded = Recorded::load("azure-get/get-range-not-satisfiable");
    assert_eq!(
        recorded.header("content-range"),
        Some(b"bytes */30".as_slice())
    );
    assert_eq!(
        accept(shape, recorded.head()),
        Ok(GetHeadOutcome::RangeNotSatisfiable {
            object_size: Some(30)
        })
    );
    // A `416` that states no range names no size either.
    assert_eq!(
        accept(shape, ResponseHead::new(416)),
        Ok(GetHeadOutcome::RangeNotSatisfiable { object_size: None })
    );
}

#[test]
fn every_other_status_is_a_service_failure_a_scheduler_can_branch_on() {
    // A read of a key that holds nothing is not an error: it is the answer.
    let recorded = Recorded::load("azure-get/get-missing");
    assert_eq!(
        recorded.header("x-ms-error-code"),
        Some(b"BlobNotFound".as_slice())
    );
    assert_eq!(
        accept(GetShape::default(), recorded.head()),
        Ok(GetHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NotFound)
        })
    );

    // A 404 that names the container separates it from a missing object.
    let recorded = Recorded::load("azure-get/get-container-missing");
    assert_eq!(
        recorded.header("x-ms-error-code"),
        Some(b"ContainerNotFound".as_slice())
    );
    assert_eq!(
        accept(GetShape::default(), recorded.head()),
        Ok(GetHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        })
    );

    // A token the service could not read at all. A read the identity may not
    // make is a `403`, which only a write can provoke here: see
    // `azure_put.rs`.
    let recorded = Recorded::load("azure-get/get-unauthenticated");
    let outcome = accept(GetShape::default(), recorded.head());
    assert!(
        matches!(
            outcome,
            Ok(GetHeadOutcome::ServiceFailure(Failure {
                status: 401,
                class: FailureClass::Auth,
                kind: Some(ServiceErrorKind::Unauthorized),
                ..
            }))
        ),
        "{outcome:?}"
    );
    // A code this crate does not know is still decisive: Azure repeats the
    // header in the body, so there is nothing more to read.
    let mut future = ResponseHead::new(404);
    future.error_code = Some(b"FutureAzureCode");
    assert_eq!(
        accept(GetShape::default(), future),
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
            ResponseHead::from_headers(status, [("x-ms-request-id", b"request-123".as_slice())]);
        head.error_code = Some(code);
        assert!(
            matches!(
                accept(GetShape::default(), head),
                Ok(GetHeadOutcome::ServiceFailure(Failure {
                    status: got_status,
                    class: got_class,
                    kind: got_kind,
                    request_id: Some(b"request-123"),
                })) if got_status == status && got_class == class && got_kind == kind
            ),
            "{status} {code:?}"
        );
    }
}

/// Every refusal in the recorded corpus, classified from the head alone.
///
/// The service names the error in a header on every one of these, so a host
/// never reads the body to learn what happened. The table below covers the
/// codes that this corpus cannot provoke.
#[test]
fn classifies_every_recorded_refusal() {
    for (file, code, expected) in [
        (
            "azure-get/get-missing",
            "BlobNotFound",
            ServiceErrorKind::NotFound,
        ),
        (
            "azure-get/get-precondition-failed",
            "ConditionNotMet",
            ServiceErrorKind::Precondition,
        ),
        (
            "azure-get/get-range-not-satisfiable",
            "InvalidRange",
            ServiceErrorKind::RangeNotSatisfiable,
        ),
        (
            "azure-get/get-container-missing",
            "ContainerNotFound",
            ServiceErrorKind::NoSuchContainer,
        ),
        (
            "azure-put/put-container-missing",
            "ContainerNotFound",
            ServiceErrorKind::NoSuchContainer,
        ),
        (
            "azure-put/put-refused",
            "AuthorizationPermissionMismatch",
            ServiceErrorKind::Unauthorized,
        ),
        (
            "azure-delete/delete-refused",
            "AuthorizationPermissionMismatch",
            ServiceErrorKind::Unauthorized,
        ),
        (
            "azure-put/put-lost-the-race-to-create",
            "BlobAlreadyExists",
            ServiceErrorKind::AlreadyExists,
        ),
        (
            "azure-delete/delete-missing",
            "BlobNotFound",
            ServiceErrorKind::NotFound,
        ),
    ] {
        let recorded = Recorded::load(file);
        assert_eq!(
            recorded.header("x-ms-error-code"),
            Some(code.as_bytes()),
            "{file}"
        );
        assert_eq!(
            classify_error(&recorded.head(), b"", false),
            Classification::Classified(expected),
            "{file}"
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
        let head = ResponseHead::from_headers(400, [("x-ms-error-code", code.as_bytes())]);
        assert_eq!(
            classify_error(&head, b"", false),
            Classification::Classified(expected),
            "{code}"
        );
    }
}

#[test]
fn falls_back_to_the_xml_error_body() {
    // The body the service sends beside a `404`, read on its own.
    let recorded = Recorded::load("azure-get/get-missing");
    assert_eq!(
        classify_error(&ResponseHead::new(404), &recorded.body(), false),
        Classification::Classified(ServiceErrorKind::NotFound)
    );

    let head = ResponseHead::new(404);
    // A complete body naming a code this crate does not know, and a body the
    // host's cap cut short, are different answers. Neither is a body the
    // service sends today, so both are written here.
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
        let head =
            ResponseHead::from_headers(status, [("x-ms-request-id", b"request-123".as_slice())]);
        let expected = match status {
            404 => FailureClass::Other,
            403 => FailureClass::Auth,
            _ => FailureClass::Server,
        };
        assert!(
            matches!(
                blobs.accept_get_head(GetShape::default(), head),
                Ok(GetHeadOutcome::NeedErrorBody(Failure {
                    status: got_status,
                    class,
                    kind: None,
                    request_id: Some(b"request-123"),
                })) if got_status == status && class == expected
            ),
            "{status}"
        );
    }
}

#[test]
fn the_error_body_names_an_error_the_head_left_out() {
    let blobs = blobs();
    let missing = need_error_body(&blobs, 404);
    assert_eq!(
        blobs.accept_error_body(
            missing.status,
            missing.request_id,
            b"<Error><Code>ContainerNotFound</Code></Error>"
        ),
        GetHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        }
    );

    // A code that arrives in the body refines the category, as a code in the
    // header would have.
    let refused = need_error_body(&blobs, 400);
    assert!(matches!(
        blobs.accept_error_body(
            refused.status,
            refused.request_id,
            b"<Error><Code>ServerBusy</Code></Error>"
        ),
        GetHeadOutcome::ServiceFailure(Failure {
            class: FailureClass::Throttled,
            kind: Some(ServiceErrorKind::Throttled),
            ..
        })
    ));

    // A host that could read no body still gets a final outcome, with the
    // error unnamed.
    assert_eq!(
        blobs.accept_error_body(missing.status, missing.request_id, b""),
        GetHeadOutcome::NotFound { kind: None }
    );

    // A head that named the error is final without a body read.
    assert_eq!(
        blobs.accept_get_head(
            GetShape::default(),
            ResponseHead::from_headers(404, [("x-ms-error-code", b"BlobNotFound".as_slice())]),
        ),
        Ok(GetHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NotFound)
        })
    );
}

// The failure of a head that asks for the error body, which the finisher takes
// apart rather than the outcome itself.
fn need_error_body<'h>(blobs: &Blobs<'_>, status: u16) -> Failure<'h> {
    match blobs.accept_get_head(GetShape::default(), ResponseHead::new(status)) {
        Ok(GetHeadOutcome::NeedErrorBody(failure)) => failure,
        other => panic!("unexpected outcome: {other:?}"),
    }
}
