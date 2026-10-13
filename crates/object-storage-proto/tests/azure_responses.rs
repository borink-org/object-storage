//! Azure response interpretation fixtures.

use borink_object_storage_proto::azure;
use borink_object_storage_proto::{
    Classification, ConditionKind, Error, GetHeadOutcome, GetShape, RequestedRange, ResponseFault,
    ResponseHead, ServiceErrorKind, classify_error,
};

fn accept<'h>(shape: GetShape, head: ResponseHead<'h>) -> Result<GetHeadOutcome<'h>, Error> {
    azure::accept_get_head(shape, head)
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
fn conditional_statuses_need_the_condition_that_explains_them() {
    let not_modified = ResponseHead::from_headers(304, [("ETag", b"\"etag\"".as_slice())]);
    assert_eq!(
        accept(conditional(ConditionKind::IfNoneMatch), not_modified),
        Ok(GetHeadOutcome::NotModified {
            e_tag: Some(b"\"etag\"")
        })
    );
    let failed =
        ResponseHead::from_headers(412, [("x-ms-error-code", b"ConditionNotMet".as_slice())]);
    assert_eq!(
        accept(conditional(ConditionKind::IfMatch), failed),
        Ok(GetHeadOutcome::PreconditionFailed)
    );

    // Nothing in an unconditional read explains a 304.
    assert_eq!(
        accept(GetShape::default(), not_modified),
        Err(Error::Response(ResponseFault::Status))
    );

    // A 412 that names no failed condition is the service failure it names.
    let lease =
        ResponseHead::from_headers(412, [("x-ms-error-code", b"LeaseIdMissing".as_slice())]);
    for shape in [GetShape::default(), conditional(ConditionKind::IfMatch)] {
        match accept(shape, lease) {
            Ok(GetHeadOutcome::ServiceFailure(failure)) => assert_eq!(failure.status, 412),
            other => panic!("{other:?}"),
        }
    }

    // A failed condition that Azure names only in the body is one too.
    let unnamed = accept(conditional(ConditionKind::IfMatch), ResponseHead::new(412)).unwrap();
    let GetHeadOutcome::NeedErrorBody(failure) = unnamed else {
        panic!("unexpected outcome: {unnamed:?}");
    };
    assert_eq!(
        azure::accept_get_error_body(
            conditional(ConditionKind::IfMatch),
            failure,
            b"<Error><Code>ConditionNotMet</Code></Error>"
        ),
        GetHeadOutcome::PreconditionFailed
    );
}

#[test]
fn ranged_and_unranged_plans_must_be_answered_in_kind() {
    let bounded = ranged(RequestedRange::Bounded { start: 2, end: 6 });
    assert_eq!(
        accept(
            bounded,
            ResponseHead::from_headers(200, [("Content-Length", b"10".as_slice())]),
        ),
        Err(Error::Response(ResponseFault::Range))
    );
    assert_eq!(
        accept(
            GetShape::default(),
            ResponseHead::from_headers(206, [("Content-Range", b"bytes 0-9/10".as_slice())]),
        ),
        Err(Error::Response(ResponseFault::Range))
    );
    // A 206 that names no range leaves the head missing a value it needs.
    assert_eq!(
        accept(bounded, ResponseHead::new(206)),
        Err(Error::Response(ResponseFault::Head))
    );
}

#[test]
fn enforces_maximal_satisfaction_of_the_requested_range() {
    let bounded = ranged(RequestedRange::Bounded { start: 2, end: 6 });
    let head = |value: &'static [u8]| ResponseHead::from_headers(206, [("Content-Range", value)]);

    // The request is served whole, and a request past EOF clamps at the size.
    assert!(accept(bounded, head(b"bytes 2-5/10")).is_ok());
    assert!(accept(bounded, head(b"bytes 2-3/4")).is_ok());
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
fn a_head_that_fails_needs_no_body() {
    let head = GetShape {
        kind: borink_object_storage_proto::GetKind::Head,
        ..GetShape::default()
    };
    // The answer to a HEAD has no body to name the error in.
    match accept(head, ResponseHead::new(500)) {
        Ok(GetHeadOutcome::ServiceFailure(failure)) => assert_eq!(failure.status, 500),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        accept(GetShape::default(), ResponseHead::new(500)),
        Ok(GetHeadOutcome::NeedErrorBody(_))
    ));
}

#[test]
fn an_object_that_exists_fails_a_write_for_none() {
    use borink_object_storage_proto::{PutHeadOutcome, PutShape};
    let absent = PutShape {
        condition: ConditionKind::IfNoneMatch,
    };
    let exists =
        || ResponseHead::from_headers(409, [("x-ms-error-code", b"BlobAlreadyExists".as_slice())]);
    assert!(matches!(
        azure::accept_put_head(absent, exists()),
        Ok(PutHeadOutcome::PreconditionFailed)
    ));
    // A 409 that only the body names ends the same way.
    let Ok(PutHeadOutcome::NeedErrorBody(failure)) =
        azure::accept_put_head(absent, ResponseHead::new(409))
    else {
        panic!("the head names no error");
    };
    let body = b"<Error><Code>BlobAlreadyExists</Code></Error>";
    assert!(matches!(
        azure::accept_put_error_body(absent, failure, body),
        PutHeadOutcome::PreconditionFailed
    ));
    // Without the condition, the 409 stays the service's failure.
    assert!(matches!(
        azure::accept_put_head(PutShape::default(), exists()),
        Ok(PutHeadOutcome::ServiceFailure(failure)) if failure.kind == Some(ServiceErrorKind::AlreadyExists)
    ));
}

#[test]
fn a_template_states_the_shape_of_its_responses() {
    use borink_object_storage_proto::{
        AzureNamespace, Container, DeleteKind, DeleteOptions, GetItem, InvalidRequest,
    };
    let container = Container::new("https://account.blob.core.windows.net", "container").unwrap();
    let options = DeleteOptions {
        kind: DeleteKind::ObjectAndSnapshots,
        condition: ConditionKind::IfMatch,
        ..DeleteOptions::new()
    };
    let mut bytes = [0; 1024];
    let removals = azure::prepare_delete(
        &container,
        "token",
        AzureNamespace::Unknown,
        &options,
        &mut bytes,
    )
    .unwrap();
    // The shape outlives storing the template.
    let stored = azure::Template::from_bytes(&removals.to_bytes()).unwrap();
    assert_eq!(stored.delete_shape(), Ok(options.shape()));
    assert_eq!(
        stored.get_shape(&GetItem::new("key")),
        Err(Error::InvalidRequest(InvalidRequest::Template))
    );
}
