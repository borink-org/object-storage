//! Azure removal encoding and response interpretation.

use borink_object_storage_proto::{
    Blobs, ConditionKind, Container, DeleteHeadOutcome, DeleteKind, DeleteShape, Error, Failure,
    FailureClass, InvalidPlan, Method, PhysicalDelete, ResponseFault, ResponseHead,
    ServiceErrorKind, Timestamps, layered,
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

fn conditional(condition: ConditionKind) -> DeleteShape {
    DeleteShape {
        condition,
        ..DeleteShape::default()
    }
}

#[test]
fn a_removal_names_the_object_and_carries_no_content() {
    let blobs = blobs();
    let delete = PhysicalDelete::new("directory/object.txt");
    let mut buf = vec![0; layered::delete_requirements(&blobs, &delete, &now()).unwrap()];
    let request = blobs.encode_delete(&mut buf, &delete, &now()).unwrap();

    assert_eq!(request.method(), Method::Delete);
    assert_eq!(
        request.url(),
        "https://account.blob.core.windows.net/container/directory/object.txt"
    );
    assert!(request.payload().is_empty());

    // A removal sends no content, so it states no length and no blob type.
    let headers: Vec<_> = request.headers().collect();
    assert_eq!(
        headers,
        [
            ("authorization", "Bearer token"),
            ("x-ms-date", "Sat, 22 Aug 2026 12:00:00 GMT"),
            ("x-ms-version", "2026-04-06"),
        ]
    );
}

#[test]
fn a_conditional_removal_sends_the_condition_header() {
    let blobs = blobs();
    let delete = PhysicalDelete::from_shape(
        conditional(ConditionKind::IfMatch),
        "object.bin",
        Some(b"\"etag\""),
    );
    let mut buf = vec![0; layered::delete_requirements(&blobs, &delete, &now()).unwrap()];
    let request = blobs.encode_delete(&mut buf, &delete, &now()).unwrap();
    assert!(request.headers().any(|h| h == ("if-match", "\"etag\"")));
}

#[test]
fn a_removal_plan_is_validated_before_any_byte_is_written() {
    let blobs = blobs();
    for (delete, expected) in [
        (PhysicalDelete::new(""), InvalidPlan::Key),
        (
            PhysicalDelete {
                condition: ConditionKind::IfMatch,
                condition_value: None,
                ..PhysicalDelete::new("object.bin")
            },
            InvalidPlan::Condition,
        ),
    ] {
        assert_eq!(
            blobs.encode_delete(&mut [0; 512], &delete, &now()).err(),
            Some(Error::InvalidPlan(expected))
        );
    }

    // The exact requirement is exact: one byte less does not encode.
    let delete = PhysicalDelete::new("object.bin");
    let required = layered::delete_requirements(&blobs, &delete, &now()).unwrap();
    let mut buf = vec![0; required - 1];
    assert_eq!(
        blobs
            .encode_delete(&mut buf, &delete, &now())
            .unwrap_err()
            .capacity()
            .unwrap()
            .required,
        required
    );
}

#[test]
fn an_accepted_removal_reports_no_metadata() {
    assert_eq!(
        blobs().accept_delete_head(DeleteShape::default(), ResponseHead::new(202)),
        Ok(DeleteHeadOutcome::Accepted)
    );

    // A removal answers 202, never another success status.
    assert_eq!(
        blobs().accept_delete_head(DeleteShape::default(), ResponseHead::new(200)),
        Err(Error::Response(ResponseFault::Status))
    );
}

#[test]
fn a_failed_condition_needs_the_condition_that_explains_it() {
    let blobs = blobs();
    assert_eq!(
        blobs.accept_delete_head(conditional(ConditionKind::IfMatch), ResponseHead::new(412)),
        Ok(DeleteHeadOutcome::PreconditionFailed)
    );
    assert_eq!(
        blobs.accept_delete_head(DeleteShape::default(), ResponseHead::new(412)),
        Err(Error::Response(ResponseFault::Status))
    );
}

#[test]
fn removing_an_object_that_is_not_there_is_an_outcome_not_an_error() {
    let blobs = blobs();
    let mut head = ResponseHead::new(404);
    head.error_code = Some(b"BlobNotFound");
    assert_eq!(
        blobs.accept_delete_head(DeleteShape::default(), head),
        Ok(DeleteHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NotFound)
        })
    );

    // With no code in the head, the body names it instead.
    let unnamed = blobs
        .accept_delete_head(DeleteShape::default(), ResponseHead::new(404))
        .unwrap();
    let DeleteHeadOutcome::NeedErrorBody(failure) = unnamed else {
        panic!("unexpected outcome: {unnamed:?}");
    };
    assert_eq!(
        blobs.accept_delete_error_body(
            failure.status,
            failure.request_id,
            b"<Error><Code>ContainerNotFound</Code></Error>"
        ),
        DeleteHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        }
    );
}

#[test]
fn a_removal_says_what_it_takes_with_it() {
    let blobs = blobs();
    for (kind, expected) in [
        (DeleteKind::Object, None),
        (DeleteKind::ObjectAndSnapshots, Some("include")),
        (DeleteKind::SnapshotsOnly, Some("only")),
    ] {
        let delete = PhysicalDelete {
            kind,
            ..PhysicalDelete::new("object.bin")
        };
        let mut buf = vec![0; layered::delete_requirements(&blobs, &delete, &now()).unwrap()];
        let request = blobs.encode_delete(&mut buf, &delete, &now()).unwrap();
        let sent = request
            .headers()
            .find(|(name, _)| *name == "x-ms-delete-snapshots")
            .map(|(_, value)| value);
        assert_eq!(sent, expected, "{kind:?}");
    }

    // The header is written into the caller's buffer with the rest of the
    // head, so naming the snapshots costs exactly its own bytes.
    let plain = PhysicalDelete::new("object.bin");
    let widened = PhysicalDelete {
        kind: DeleteKind::ObjectAndSnapshots,
        ..plain
    };
    assert_eq!(
        layered::delete_requirements(&blobs, &widened, &now()).unwrap()
            - layered::delete_requirements(&blobs, &plain, &now()).unwrap(),
        "x-ms-delete-snapshots".len() + "include".len()
    );
}

#[test]
fn an_object_with_snapshots_is_refused_rather_than_widened() {
    // A plan that names the object alone sends no `x-ms-delete-snapshots`, so
    // Azure refuses a base blob that has them. The code is not one this crate
    // classifies, so it lands on the status alone, where an unknown code
    // belongs.
    let mut head = ResponseHead::new(409);
    head.error_code = Some(b"SnapshotsPresent");
    head.request_id = Some(b"request-123");
    assert!(matches!(
        blobs().accept_delete_head(DeleteShape::default(), head),
        Ok(DeleteHeadOutcome::ServiceFailure(Failure {
            status: 409,
            class: FailureClass::Other,
            kind: None,
            request_id: Some(b"request-123"),
        }))
    ));
}

#[test]
fn describes_what_happened_to_the_removal() {
    assert_eq!(
        DeleteHeadOutcome::Accepted.to_string(),
        "the service accepted the removal"
    );
    assert_eq!(
        DeleteHeadOutcome::NotFound { kind: None }.to_string(),
        "the object does not exist"
    );
}
