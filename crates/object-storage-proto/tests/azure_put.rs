//! Azure write encoding and response interpretation.
//!
//! Every head here that says what the service answers a write with is one of
//! the responses recorded under `tests/fixtures/azure-put`. A head written out
//! in this file is one the service does not send to the identity that recorded
//! them, and says so.

mod recorded;

use recorded::Recorded;

use borink_object_storage_proto::{
    Blobs, ConditionKind, Container, Error, Failure, FailureClass, InvalidPlan, Method, ObjectMeta,
    Payload, PhysicalPut, PutHeadOutcome, PutShape, ResponseFault, ResponseHead, ServiceErrorKind,
    Timestamps, layered,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "container").unwrap(),
        "token",
    )
    .unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

fn conditional(condition: ConditionKind) -> PutShape {
    PutShape { condition }
}

#[test]
fn a_write_states_the_content_length_and_borrows_the_content() {
    let blobs = blobs();
    let put = PhysicalPut::new("directory/object.txt");
    let content = [7u8; 4096];
    let mut buf =
        vec![0; layered::put_requirements(&blobs, &put, Payload::Slice(&content), &now()).unwrap()];
    let request = blobs
        .encode_put(&mut buf, &put, Payload::Slice(&content), &now())
        .unwrap();

    assert_eq!(request.method(), Method::Put);
    assert_eq!(
        request.url(),
        "https://account.blob.core.windows.net/container/directory/object.txt"
    );
    let headers: Vec<_> = request.headers().collect();
    assert!(headers.contains(&("x-ms-blob-type", "BlockBlob")));
    assert!(headers.contains(&("content-length", "4096")));
    assert!(headers.contains(&("authorization", "Bearer token")));
    assert!(headers.contains(&("x-ms-date", "Sat, 22 Aug 2026 12:00:00 GMT")));

    // The content is not copied into the buffer: the request points at it.
    let sent = request.payload().bytes().unwrap();
    assert_eq!(sent.as_ptr(), content.as_ptr());
    assert_eq!(sent.len(), 4096);
}

#[test]
fn an_empty_write_states_a_zero_length() {
    let blobs = blobs();
    let put = PhysicalPut::new("empty.bin");
    let mut buf =
        vec![0; layered::put_requirements(&blobs, &put, Payload::Slice(b""), &now()).unwrap()];
    let request = blobs
        .encode_put(&mut buf, &put, Payload::Slice(b""), &now())
        .unwrap();

    assert!(request.headers().any(|h| h == ("content-length", "0")));
    assert!(request.payload().is_empty());
}

#[test]
fn a_conditional_write_sends_the_condition_header() {
    let blobs = blobs();
    let create = PhysicalPut {
        key: "object.bin",
        condition: ConditionKind::IfNoneMatch,
        condition_value: Some(b"*"),
    };
    let mut buf = vec![
        0;
        layered::put_requirements(&blobs, &create, Payload::Slice(b"one"), &now())
            .unwrap()
    ];
    let request = blobs
        .encode_put(&mut buf, &create, Payload::Slice(b"one"), &now())
        .unwrap();
    assert!(request.headers().any(|h| h == ("if-none-match", "*")));

    let replace = PhysicalPut::from_shape(
        conditional(ConditionKind::IfMatch),
        "object.bin",
        Some(b"\"etag\""),
    );
    let mut buf = vec![
        0;
        layered::put_requirements(&blobs, &replace, Payload::Slice(b"one"), &now())
            .unwrap()
    ];
    let request = blobs
        .encode_put(&mut buf, &replace, Payload::Slice(b"one"), &now())
        .unwrap();
    assert!(request.headers().any(|h| h == ("if-match", "\"etag\"")));
}

#[test]
fn the_requirement_grows_with_the_stated_length_but_not_with_the_content() {
    let blobs = blobs();
    let put = PhysicalPut::new("object.bin");
    let short = layered::put_requirements(&blobs, &put, Payload::Slice(&[0; 9]), &now()).unwrap();
    let long =
        layered::put_requirements(&blobs, &put, Payload::Slice(&[0; 1_000_000]), &now()).unwrap();

    // "9" is one byte, "1000000" is seven, and nothing else differs.
    assert_eq!(long - short, 6);

    // The exact requirement is exact: one byte less does not encode.
    let mut buf = vec![0; short - 1];
    assert_eq!(
        blobs
            .encode_put(&mut buf, &put, Payload::Slice(&[0; 9]), &now())
            .unwrap_err()
            .capacity()
            .unwrap()
            .required,
        short
    );
}

#[test]
fn a_write_plan_is_validated_before_any_byte_is_written() {
    let blobs = blobs();
    for (put, expected) in [
        (PhysicalPut::new(""), InvalidPlan::Key),
        (
            PhysicalPut {
                key: "object.bin",
                condition: ConditionKind::IfMatch,
                condition_value: None,
            },
            InvalidPlan::Condition,
        ),
        (
            PhysicalPut {
                key: "object.bin",
                condition: ConditionKind::None,
                condition_value: Some(b"\"etag\""),
            },
            InvalidPlan::Condition,
        ),
    ] {
        assert_eq!(
            blobs
                .encode_put(&mut [0; 512], &put, Payload::Slice(b"one"), &now())
                .err(),
            Some(Error::InvalidPlan(expected))
        );
    }

    // A key longer than Azure accepts is refused by character count, not by
    // byte count: these are two-byte characters.
    let long = "\u{e9}".repeat(1025);
    assert_eq!(
        blobs
            .encode_put(
                &mut [0; 4096],
                &PhysicalPut::new(&long),
                Payload::Slice(b"one"),
                &now()
            )
            .err(),
        Some(Error::InvalidPlan(InvalidPlan::Key))
    );
}

#[test]
fn a_stored_object_reports_the_metadata_azure_returned() {
    let recorded = Recorded::load("azure-put/put-created");
    let Ok(PutHeadOutcome::Created { meta, .. }) =
        blobs().accept_put_head(PutShape::default(), recorded.head())
    else {
        panic!("201 stores the object");
    };
    assert_eq!(
        meta,
        ObjectMeta {
            // A write never reports a size: it is the length you sent. The
            // `content-length` on the response is the length of the response.
            size: None,
            e_tag: recorded.header("etag"),
            last_modified: recorded.header("last-modified"),
            // The account keeps versions, so the write names the one it made.
            version: recorded.header("x-ms-version-id"),
            ..ObjectMeta::default()
        }
    );
    assert!(meta.version.is_some());

    // An object of no bytes is an object, and is stored the same way.
    let empty = Recorded::load("azure-put/put-created-empty");
    assert!(matches!(
        blobs().accept_put_head(PutShape::default(), empty.head()),
        Ok(PutHeadOutcome::Created { .. })
    ));
}

#[test]
fn a_failed_condition_needs_the_condition_that_explains_it() {
    let blobs = blobs();
    let recorded = Recorded::load("azure-put/put-precondition-failed");
    assert_eq!(
        recorded.header("x-ms-error-code"),
        Some(b"ConditionNotMet".as_slice())
    );
    assert_eq!(
        blobs.accept_put_head(conditional(ConditionKind::IfMatch), recorded.head()),
        Ok(PutHeadOutcome::PreconditionFailed)
    );

    // Nothing in an unconditional write explains a 412.
    assert_eq!(
        blobs.accept_put_head(PutShape::default(), recorded.head()),
        Err(Error::Response(ResponseFault::Status))
    );

    // A write answers 201, never another success status.
    assert_eq!(
        blobs.accept_put_head(PutShape::default(), ResponseHead::new(200)),
        Err(Error::Response(ResponseFault::Status))
    );
}

#[test]
fn a_lost_race_to_create_names_the_object_that_already_exists() {
    let recorded = Recorded::load("azure-put/put-lost-the-race-to-create");
    assert_eq!(
        recorded.header("x-ms-error-code"),
        Some(b"BlobAlreadyExists".as_slice())
    );
    let outcome = blobs().accept_put_head(conditional(ConditionKind::IfNoneMatch), recorded.head());
    assert!(
        matches!(
            outcome,
            Ok(PutHeadOutcome::ServiceFailure(Failure {
                status: 409,
                class: FailureClass::Other,
                kind: Some(ServiceErrorKind::AlreadyExists),
                request_id: Some(_),
            }))
        ),
        "{outcome:?}"
    );
}

/// The same write to the same container that is not there, under each of the
/// two identities the corpus is recorded with.
///
/// Azure settles the grant before it looks for the container, so what comes
/// back says which grant the sender holds. An identity that may write only in
/// one container is refused and never reaches the `404`; one that may write
/// anywhere in the account is told the container is missing. Neither answer is
/// a fact about writes, and a caller of this crate can meet either.
#[test]
fn a_write_to_a_missing_container_reports_the_container() {
    let blobs = blobs();

    let refused = Recorded::load("azure-put/put-refused");
    assert_eq!(
        refused.header("x-ms-error-code"),
        Some(b"AuthorizationPermissionMismatch".as_slice())
    );
    let outcome = blobs.accept_put_head(PutShape::default(), refused.head());
    assert!(
        matches!(
            outcome,
            Ok(PutHeadOutcome::ServiceFailure(Failure {
                status: 403,
                class: FailureClass::Auth,
                kind: Some(ServiceErrorKind::Unauthorized),
                ..
            }))
        ),
        "{outcome:?}"
    );

    let missing = Recorded::load("azure-put/put-container-missing");
    assert_eq!(
        missing.header("x-ms-error-code"),
        Some(b"ContainerNotFound".as_slice())
    );
    assert_eq!(
        blobs.accept_put_head(PutShape::default(), missing.head()),
        Ok(PutHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        })
    );

    // With no code in the head, the body names it instead.
    let unnamed = blobs
        .accept_put_head(PutShape::default(), ResponseHead::new(404))
        .unwrap();
    let PutHeadOutcome::NeedErrorBody(failure) = unnamed else {
        panic!("unexpected outcome: {unnamed:?}");
    };
    assert_eq!(failure.kind, None);
    assert_eq!(
        blobs.accept_put_error_body(
            failure.status,
            failure.request_id,
            b"<Error><Code>ContainerNotFound</Code></Error>"
        ),
        PutHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        }
    );
    assert_eq!(
        blobs.accept_put_error_body(failure.status, failure.request_id, b""),
        PutHeadOutcome::NotFound { kind: None }
    );
}

#[test]
fn a_refused_write_carries_the_category_and_the_request_id() {
    let blobs = blobs();
    let mut head = ResponseHead::new(503);
    head.request_id = Some(b"request-123");
    let outcome = blobs.accept_put_head(PutShape::default(), head).unwrap();
    let PutHeadOutcome::NeedErrorBody(failure) = outcome else {
        panic!("unexpected outcome: {outcome:?}");
    };
    assert_eq!(
        blobs.accept_put_error_body(
            failure.status,
            failure.request_id,
            b"<Error><Code>ServerBusy</Code></Error>"
        ),
        PutHeadOutcome::ServiceFailure(Failure {
            status: 503,
            class: FailureClass::Throttled,
            kind: Some(ServiceErrorKind::Throttled),
            request_id: Some(b"request-123"),
        })
    );
}

#[test]
fn describes_what_happened_to_the_write() {
    assert_eq!(
        PutHeadOutcome::PreconditionFailed.to_string(),
        "the condition on the write did not hold"
    );
    assert_eq!(
        PutHeadOutcome::NotFound { kind: None }.to_string(),
        "the container does not exist"
    );
}

#[test]
fn streamed_content_writes_the_same_head_without_the_bytes() {
    let blobs = blobs();
    let put = PhysicalPut::new("object.bin");
    let content = [7u8; 4096];
    let streamed = Payload::Streamed { len: 4096 };

    let mut held =
        vec![0; layered::put_requirements(&blobs, &put, Payload::Slice(&content), &now()).unwrap()];
    let mut sent = vec![0; layered::put_requirements(&blobs, &put, streamed, &now()).unwrap()];
    let borrowed = blobs
        .encode_put(&mut held, &put, Payload::Slice(&content), &now())
        .unwrap();
    let streaming = blobs.encode_put(&mut sent, &put, streamed, &now()).unwrap();

    // The head is what the service sees, and it does not know the difference.
    assert_eq!(borrowed.url(), streaming.url());
    assert_eq!(
        borrowed.headers().collect::<Vec<_>>(),
        streaming.headers().collect::<Vec<_>>()
    );

    // Only the content differs: one is lent, the other is the host's to send.
    assert_eq!(borrowed.payload().bytes(), Some(content.as_slice()));
    assert_eq!(streaming.payload().bytes(), None);
    assert_eq!(streaming.payload().len(), 4096);
}

#[test]
fn a_streamed_payload_is_refused_at_the_same_length_as_a_held_one() {
    let blobs = blobs();
    let put = PhysicalPut::new("object.bin");
    // 5000 MiB is the most Azure writes in one request, so this cannot be a
    // slice on any machine. Only the streamed form can state it at all.
    let too_long = Payload::Streamed {
        len: 5000 * 1024 * 1024 + 1,
    };
    assert_eq!(
        blobs
            .encode_put(&mut [0; 512], &put, too_long, &now())
            .err(),
        Some(Error::InvalidPlan(InvalidPlan::PayloadTooLarge))
    );

    let longest = Payload::Streamed {
        len: 5000 * 1024 * 1024,
    };
    assert!(layered::put_requirements(&blobs, &put, longest, &now()).is_ok());
}
