//! Azure write encoding and response interpretation.

use borink_object_storage::{
    Blobs, ConditionKind, Container, Error, FailureClass, InvalidPlan, ObjectMeta, PhysicalPut,
    PutHeadOutcome, PutShape, ResponseHead, ServiceErrorKind, Timestamps, layered,
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
    let mut buf = vec![0; layered::put_requirements(&blobs, &put, &content, &now()).unwrap()];
    let request = blobs.encode_put(&mut buf, &put, &content, &now()).unwrap();

    assert_eq!(request.method(), "PUT");
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
    assert_eq!(request.body().as_ptr(), content.as_ptr());
    assert_eq!(request.body().len(), 4096);
}

#[test]
fn an_empty_write_states_a_zero_length() {
    let blobs = blobs();
    let put = PhysicalPut::new("empty.bin");
    let mut buf = vec![0; layered::put_requirements(&blobs, &put, b"", &now()).unwrap()];
    let request = blobs.encode_put(&mut buf, &put, b"", &now()).unwrap();

    assert!(request.headers().any(|h| h == ("content-length", "0")));
    assert!(request.body().is_empty());
}

#[test]
fn a_conditional_write_sends_the_condition_header() {
    let blobs = blobs();
    let create = PhysicalPut {
        key: "object.bin",
        condition: ConditionKind::IfNoneMatch,
        condition_value: Some(b"*"),
    };
    let mut buf = vec![0; layered::put_requirements(&blobs, &create, b"one", &now()).unwrap()];
    let request = blobs.encode_put(&mut buf, &create, b"one", &now()).unwrap();
    assert!(request.headers().any(|h| h == ("if-none-match", "*")));

    let replace = PhysicalPut::from_shape(
        conditional(ConditionKind::IfMatch),
        "object.bin",
        Some(b"\"etag\""),
    );
    let mut buf = vec![0; layered::put_requirements(&blobs, &replace, b"one", &now()).unwrap()];
    let request = blobs
        .encode_put(&mut buf, &replace, b"one", &now())
        .unwrap();
    assert!(request.headers().any(|h| h == ("if-match", "\"etag\"")));
}

#[test]
fn the_requirement_grows_with_the_stated_length_but_not_with_the_content() {
    let blobs = blobs();
    let put = PhysicalPut::new("object.bin");
    let short = layered::put_requirements(&blobs, &put, &[0; 9], &now()).unwrap();
    let long = layered::put_requirements(&blobs, &put, &[0; 1_000_000], &now()).unwrap();

    // "9" is one byte, "1000000" is seven, and nothing else differs.
    assert_eq!(long - short, 6);

    // The exact requirement is exact: one byte less does not encode.
    let mut buf = vec![0; short - 1];
    assert_eq!(
        blobs
            .encode_put(&mut buf, &put, &[0; 9], &now())
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
            blobs.encode_put(&mut [0; 512], &put, b"one", &now()).err(),
            Some(Error::InvalidPlan(expected))
        );
    }

    // A key longer than Azure accepts is refused by character count, not by
    // byte count: these are two-byte characters.
    let long = "\u{e9}".repeat(1025);
    assert_eq!(
        blobs
            .encode_put(&mut [0; 4096], &PhysicalPut::new(&long), b"one", &now())
            .err(),
        Some(Error::InvalidPlan(InvalidPlan::Key))
    );
}

#[test]
fn a_stored_object_reports_the_metadata_azure_returned() {
    let head = ResponseHead::from_headers(
        201,
        [
            ("ETag", b"\"etag\"".as_slice()),
            ("Last-Modified", b"Fri, 24 May 2013 00:00:00 GMT"),
            ("x-ms-version-id", b"version-1"),
        ],
    );
    let Ok(PutHeadOutcome::Created { meta, .. }) =
        blobs().accept_put_head(PutShape::default(), head)
    else {
        panic!("201 stores the object");
    };
    assert_eq!(
        meta,
        ObjectMeta {
            // A write never reports a size: it is the length you sent.
            size: None,
            e_tag: Some(b"\"etag\""),
            last_modified: Some(b"Fri, 24 May 2013 00:00:00 GMT"),
            version: Some(b"version-1"),
            ..ObjectMeta::default()
        }
    );
}

#[test]
fn a_failed_condition_needs_the_condition_that_explains_it() {
    let blobs = blobs();
    assert_eq!(
        blobs.accept_put_head(conditional(ConditionKind::IfMatch), ResponseHead::new(412)),
        Ok(PutHeadOutcome::PreconditionFailed)
    );

    // Nothing in an unconditional write explains a 412.
    assert!(matches!(
        blobs.accept_put_head(PutShape::default(), ResponseHead::new(412)),
        Err(Error::ResponseMismatch(_))
    ));

    // A write answers 201, never another success status.
    assert!(matches!(
        blobs.accept_put_head(PutShape::default(), ResponseHead::new(200)),
        Err(Error::Protocol(_))
    ));
}

#[test]
fn a_lost_race_to_create_names_the_object_that_already_exists() {
    let mut head = ResponseHead::new(409);
    head.error_code = Some(b"BlobAlreadyExists");
    head.request_id = Some(b"request-123");
    assert!(matches!(
        blobs().accept_put_head(conditional(ConditionKind::IfNoneMatch), head),
        Ok(PutHeadOutcome::ServiceFailure {
            status: 409,
            class: FailureClass::Other,
            kind: Some(ServiceErrorKind::AlreadyExists),
            request_id: Some(b"request-123"),
            ..
        })
    ));
}

#[test]
fn a_write_to_a_missing_container_reports_the_container() {
    let blobs = blobs();
    let mut head = ResponseHead::new(404);
    head.error_code = Some(b"ContainerNotFound");
    assert_eq!(
        blobs.accept_put_head(PutShape::default(), head),
        Ok(PutHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        })
    );

    // With no code in the head, the body names it instead.
    let unnamed = blobs
        .accept_put_head(PutShape::default(), ResponseHead::new(404))
        .unwrap();
    assert!(matches!(unnamed, PutHeadOutcome::NeedErrorBody { .. }));
    assert_eq!(
        blobs.accept_put_error_body(unnamed, b"<Error><Code>ContainerNotFound</Code></Error>"),
        PutHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        }
    );
    assert_eq!(
        blobs.accept_put_error_body(unnamed, b""),
        PutHeadOutcome::NotFound { kind: None }
    );
}

#[test]
fn a_refused_write_carries_the_category_and_the_request_id() {
    let blobs = blobs();
    let mut head = ResponseHead::new(503);
    head.request_id = Some(b"request-123");
    let outcome = blobs.accept_put_head(PutShape::default(), head).unwrap();
    assert!(matches!(
        blobs.accept_put_error_body(outcome, b"<Error><Code>ServerBusy</Code></Error>"),
        PutHeadOutcome::ServiceFailure {
            status: 503,
            class: FailureClass::Throttled,
            kind: Some(ServiceErrorKind::Throttled),
            request_id: Some(b"request-123"),
            ..
        }
    ));
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
