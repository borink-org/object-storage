//! S3 GET and HEAD: the URL, the plans the client refuses, and the response
//! heads it reads.

use borink_object_storage_proto::s3::{Addressing, Bucket, MAX_KEY_LEN, Objects, Service};
use borink_object_storage_proto::sigv4::{
    Credentials, Sha256Provider, Sha256State, wipe_best_effort,
};
use borink_object_storage_proto::{
    BodyWindow, ConditionKind, Error, FailureClass, GetHeadOutcome, GetKind, HeaderSpan,
    InvalidPlan, PhysicalGet, RequestedRange, ResponseFault, ResponseHead, ServiceErrorKind,
    Timestamps, layered,
};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "session-token";

const ZEROS: Sha256Provider =
    Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32], |_, _| [0; 32]);

fn bucket() -> Bucket<'static> {
    Bucket::new("https://s3.example.com", "bucket", "auto", Service::Aws).unwrap()
}

fn credentials() -> Credentials<'static> {
    Credentials::new("AKIAIOSFODNN7EXAMPLE", SECRET, wipe_best_effort)
        .unwrap()
        .with_session_token(TOKEN)
        .unwrap()
}

fn objects() -> Objects<'static> {
    Objects::new(bucket(), credentials(), ZEROS)
}

fn compatible() -> Objects<'static> {
    let bucket = Bucket::new(
        "https://s3.example.com",
        "bucket",
        "auto",
        Service::Compatible,
    )
    .unwrap();
    Objects::new(bucket, credentials(), ZEROS)
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_061)
}

fn url(bucket: Bucket<'_>, key: &str) -> String {
    let objects = Objects::new(bucket, credentials(), ZEROS);
    let get = PhysicalGet::new(key);
    let size = layered::s3::get_requirements(&objects, &get, &now()).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut slots = vec![HeaderSpan::default(); size.headers];
    let request = objects
        .encode_get(&mut buf, &mut slots, &get, &now())
        .unwrap();
    request.url().to_owned()
}

fn refusal(result: Result<(), Error>) -> InvalidPlan {
    match result {
        Err(Error::InvalidPlan(reason)) => reason,
        other => panic!("expected an invalid plan, got {other:?}"),
    }
}

fn head<'h>(status: u16, headers: &[(&'h str, &'h [u8])]) -> ResponseHead<'h> {
    ResponseHead::from_headers(status, headers.iter().copied())
}

#[test]
fn the_url_names_the_bucket_where_the_addressing_says() {
    let path = |endpoint| Bucket::new(endpoint, "bucket", "auto", Service::Aws).unwrap();
    assert_eq!(
        url(path("https://s3.example.com"), "a/b"),
        "https://s3.example.com/bucket/a/b"
    );
    assert_eq!(
        url(
            path("https://s3.example.com").with_addressing(Addressing::VirtualHosted),
            "a/b"
        ),
        "https://bucket.s3.example.com/a/b"
    );
    // The default port of the scheme is left out, as an HTTP client leaves it
    // out of the host it sends. Any other port stays.
    assert_eq!(
        url(path("https://s3.example.com:443"), "k"),
        "https://s3.example.com/bucket/k"
    );
    assert_eq!(
        url(path("http://localhost:80"), "k"),
        "http://localhost/bucket/k"
    );
    assert_eq!(
        url(path("http://localhost:443"), "k"),
        "http://localhost:443/bucket/k"
    );
    assert_eq!(
        url(path("https://[::1]:443"), "k"),
        "https://[::1]/bucket/k"
    );
    assert_eq!(
        url(path("https://[::1]:9000"), "k"),
        "https://[::1]:9000/bucket/k"
    );
    // A trailing dot is part of an S3 key, unlike an Azure name.
    assert_eq!(
        url(path("https://s3.example.com"), "dir./a."),
        "https://s3.example.com/bucket/dir./a."
    );
}

#[test]
fn a_read_that_s3_cannot_take_is_refused() {
    let objects = objects();
    let get = |get: PhysicalGet<'_>| {
        refusal(
            objects
                .encode_get(
                    &mut [0; 1024],
                    &mut [HeaderSpan::default(); 8],
                    &get,
                    &now(),
                )
                .map(drop),
        )
    };
    let long = "k".repeat(MAX_KEY_LEN + 1);
    assert_eq!(get(PhysicalGet::new("")), InvalidPlan::EmptyKey);
    assert_eq!(get(PhysicalGet::new(&long)), InvalidPlan::KeyTooLong);
    assert_eq!(
        get(PhysicalGet::new("a/../b")),
        InvalidPlan::KeyWouldBeNormalized
    );
    assert_eq!(
        get(PhysicalGet::new("./b")),
        InvalidPlan::KeyWouldBeNormalized
    );
    assert_eq!(
        get(PhysicalGet {
            range: RequestedRange::Suffix(0),
            ..PhysicalGet::new("k")
        }),
        InvalidPlan::Range
    );
    assert_eq!(
        get(PhysicalGet {
            kind: GetKind::Head,
            range: RequestedRange::Suffix(4),
            ..PhysicalGet::new("k")
        }),
        InvalidPlan::RangedHead
    );
    assert_eq!(
        get(PhysicalGet {
            condition: ConditionKind::IfMatch,
            ..PhysicalGet::new("k")
        }),
        InvalidPlan::Condition
    );
    // A key of exactly the limit is sent.
    let exact = "k".repeat(MAX_KEY_LEN);
    assert!(layered::s3::get_requirements(&objects, &PhysicalGet::new(&exact), &now()).is_ok());
}

#[test]
fn a_read_names_its_error_from_the_body_unless_it_is_a_head() {
    let objects = objects();
    let get = PhysicalGet::new("k").shape();
    let id: &[u8] = b"4442587FB7D0A2F9";

    let GetHeadOutcome::NeedErrorBody(failure) = objects
        .accept_get_head(get, head(404, &[("x-amz-request-id", id)]))
        .unwrap()
    else {
        panic!("a GET names its error in the body");
    };
    assert_eq!(
        (failure.status, failure.kind, failure.request_id),
        (404, None, Some(id))
    );
    let body = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>NoSuchBucket</Code>\
        <Message>The specified bucket does not exist</Message></Error>";
    assert_eq!(
        objects.accept_error_body(404, Some(id), body),
        GetHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        }
    );
    let GetHeadOutcome::ServiceFailure(failure) = objects.accept_error_body(
        403,
        Some(id),
        b"<Error><Code>SignatureDoesNotMatch</Code></Error>",
    ) else {
        panic!("a refusal is a service failure");
    };
    assert_eq!(
        (failure.class, failure.kind),
        (FailureClass::Auth, Some(ServiceErrorKind::Unauthorized))
    );

    // A redirect names the region in its body, and is reported as one.
    let GetHeadOutcome::NeedErrorBody(failure) =
        objects.accept_get_head(get, head(301, &[])).unwrap()
    else {
        panic!("a redirect has a body");
    };
    assert_eq!(failure.class, FailureClass::Redirect);

    // A HEAD response has no body, so its outcome is final.
    let head_shape = PhysicalGet::head("k").shape();
    assert_eq!(
        objects.accept_get_head(head_shape, head(404, &[])),
        Ok(GetHeadOutcome::NotFound { kind: None })
    );
    let Ok(GetHeadOutcome::ServiceFailure(failure)) =
        objects.accept_get_head(head_shape, head(403, &[]))
    else {
        panic!("a HEAD refusal is final");
    };
    assert_eq!((failure.class, failure.kind), (FailureClass::Auth, None));
    let Ok(GetHeadOutcome::Complete { meta }) =
        objects.accept_get_head(head_shape, head(200, &[("content-length", b"12")]))
    else {
        panic!("a HEAD success is complete");
    };
    assert_eq!(meta.size, Some(12));
}

#[test]
fn a_read_checks_the_range_and_the_condition_it_asked_for() {
    // A compatible client reads a range as an AWS one does, without needing
    // `Content-Length` beside it.
    let objects = compatible();
    let suffix = |n| {
        PhysicalGet {
            range: RequestedRange::Suffix(n),
            ..PhysicalGet::new("k")
        }
        .shape()
    };
    let ranged = |status, range: &'static [u8]| head(status, &[("content-range", range)]);

    assert_eq!(
        objects
            .accept_get_head(suffix(5), ranged(206, b"bytes 95-99/100"))
            .map(|outcome| match outcome {
                GetHeadOutcome::Body { body, meta } => (body, meta.size),
                other => panic!("{other:?}"),
            }),
        Ok((
            BodyWindow {
                object_offset: 95,
                expected_len: Some(5),
                object_size: Some(100)
            },
            Some(100)
        ))
    );
    // A suffix longer than the object is the whole object.
    assert!(matches!(
        objects.accept_get_head(suffix(500), ranged(206, b"bytes 0-99/100")),
        Ok(GetHeadOutcome::Body {
            body: BodyWindow {
                object_offset: 0,
                ..
            },
            ..
        })
    ));
    for (shape, head) in [
        (suffix(5), ranged(206, b"bytes 90-99/100")),
        (suffix(5), ranged(206, b"bytes 95-98/100")),
        (suffix(5), ranged(206, b"bytes 95-99/*")),
        (suffix(5), ResponseHead::new(200)),
        (
            PhysicalGet::new("k").shape(),
            ranged(206, b"bytes 0-99/100"),
        ),
    ] {
        assert_eq!(
            objects.accept_get_head(shape, head),
            Err(ResponseFault::Range.into()),
            "{head:?}"
        );
    }
    assert_eq!(
        objects.accept_get_head(suffix(5), ResponseHead::new(206)),
        Err(ResponseFault::Head.into())
    );
    assert_eq!(
        objects.accept_get_head(suffix(5), head(416, &[])),
        Ok(GetHeadOutcome::RangeNotSatisfiable { object_size: None })
    );

    let conditional = |condition| {
        PhysicalGet {
            condition,
            condition_value: Some(b"\"etag\""),
            ..PhysicalGet::new("k")
        }
        .shape()
    };
    let unconditional = PhysicalGet::new("k").shape();
    assert_eq!(
        objects.accept_get_head(
            conditional(ConditionKind::IfNoneMatch),
            head(304, &[("etag", b"\"etag\"")])
        ),
        Ok(GetHeadOutcome::NotModified {
            e_tag: Some(b"\"etag\"")
        })
    );
    assert_eq!(
        objects.accept_get_head(conditional(ConditionKind::IfMatch), head(412, &[])),
        Ok(GetHeadOutcome::PreconditionFailed)
    );
    assert_eq!(
        objects.accept_get_head(unconditional, head(304, &[])),
        Err(ResponseFault::Status.into())
    );
    assert_eq!(
        objects.accept_get_head(unconditional, head(412, &[])),
        Err(ResponseFault::Status.into())
    );
}
