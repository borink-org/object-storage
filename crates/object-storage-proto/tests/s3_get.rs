//! S3 GET and HEAD: the URL, the reads the client refuses, and the response
//! heads it reads.

use borink_object_storage_proto::s3::{self, Addressing, Bucket, MAX_KEY_LEN, Service};
use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
use borink_object_storage_proto::{
    BodyWindow, ConditionKind, Error, GetHeadOutcome, GetItem, GetKind, GetOptions, GetShape,
    InvalidRequest, RequestedRange, ResponseFault, ResponseHead, Timestamps,
};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "session-token";

fn bucket() -> Bucket<'static> {
    Bucket::new("https://s3.example.com", "bucket", "auto", Service::Aws).unwrap()
}

fn credentials() -> Credentials<'static> {
    Credentials::new("AKIAIOSFODNN7EXAMPLE", SECRET, wipe_best_effort)
        .unwrap()
        .with_session_token(TOKEN)
        .unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_061)
}

// Writes `item` from a template of `options` for `bucket`, and signs it
// with a signature of zeros.
fn encode(bucket: &Bucket<'_>, options: GetOptions, item: &GetItem<'_>) -> Result<String, Error> {
    let mut bytes = vec![0; 1024];
    let template = s3::prepare_get(bucket, &credentials(), &options, &mut bytes)?;
    let mut buf = vec![0; 3 * MAX_KEY_LEN + 2048];
    let unsigned = s3::encode_get(&template, &bytes, item, &now(), &mut buf)?;
    Ok(s3::set_signature(&unsigned, &mut buf, &[0; 32])
        .url(&buf)
        .to_owned())
}

fn url(bucket: Bucket<'_>, key: &str) -> String {
    encode(&bucket, GetOptions::new(), &GetItem::new(key)).unwrap()
}

fn refusal(result: Result<String, Error>) -> InvalidRequest {
    match result {
        Err(Error::InvalidRequest(reason)) => reason,
        other => panic!("expected an invalid request, got {other:?}"),
    }
}

fn head<'h>(status: u16, headers: &[(&'h str, &'h [u8])]) -> ResponseHead<'h> {
    ResponseHead::from_headers(status, headers.iter().copied())
}

fn shape(kind: GetKind, range: RequestedRange, condition: ConditionKind) -> GetShape {
    GetShape {
        kind,
        range,
        condition,
    }
}

#[test]
fn the_url_names_the_bucket_where_the_addressing_says() {
    let path =
        |endpoint| Bucket::new_allowing_http(endpoint, "bucket", "auto", Service::Aws).unwrap();
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
    let bucket = bucket();
    let get = |options, item: GetItem<'_>| refusal(encode(&bucket, options, &item));
    let plain = GetOptions::new();
    let long = "k".repeat(MAX_KEY_LEN + 1);
    assert_eq!(get(plain, GetItem::new("")), InvalidRequest::EmptyKey);
    assert_eq!(get(plain, GetItem::new(&long)), InvalidRequest::KeyTooLong);
    assert_eq!(
        get(plain, GetItem::new("a/../b")),
        InvalidRequest::KeyWouldBeNormalized
    );
    assert_eq!(
        get(plain, GetItem::new("./b")),
        InvalidRequest::KeyWouldBeNormalized
    );
    assert_eq!(
        get(
            plain.ranged(),
            GetItem::new("k").with_range(RequestedRange::Suffix(0))
        ),
        InvalidRequest::Range
    );
    assert_eq!(
        get(
            GetOptions {
                condition: ConditionKind::IfMatch,
                ..plain
            },
            GetItem::new("k")
        ),
        InvalidRequest::Condition
    );
    // A template of ranged HEADs, or of snapshots, is never written.
    let mut bytes = [0; 1024];
    for (options, reason) in [
        (GetOptions::head().ranged(), InvalidRequest::RangedHead),
        (
            GetOptions {
                revision: borink_object_storage_proto::RevisionKind::Snapshot,
                ..plain
            },
            InvalidRequest::Revision,
        ),
    ] {
        assert_eq!(
            s3::prepare_get(&bucket, &credentials(), &options, &mut bytes).err(),
            Some(Error::InvalidRequest(reason))
        );
    }
    // A key of exactly the limit is sent.
    assert!(encode(&bucket, plain, &GetItem::new(&"k".repeat(MAX_KEY_LEN))).is_ok());
}

#[test]
fn a_read_checks_the_range_and_the_condition_it_asked_for() {
    // A compatible client reads a range as an AWS one does, without needing
    // `Content-Length` beside it.
    const SERVICE: Service = Service::Compatible;
    let suffix = |n| {
        shape(
            GetKind::Bytes,
            RequestedRange::Suffix(n),
            ConditionKind::None,
        )
    };
    let ranged = |status, range: &'static [u8]| head(status, &[("content-range", range)]);

    assert_eq!(
        s3::accept_get_head(SERVICE, suffix(5), ranged(206, b"bytes 95-99/100")).map(|outcome| {
            match outcome {
                GetHeadOutcome::Body { body, meta } => (body, meta.size),
                other => panic!("{other:?}"),
            }
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
        s3::accept_get_head(SERVICE, suffix(500), ranged(206, b"bytes 0-99/100")),
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
            shape(GetKind::Bytes, RequestedRange::Whole, ConditionKind::None),
            ranged(206, b"bytes 0-99/100"),
        ),
    ] {
        assert_eq!(
            s3::accept_get_head(SERVICE, shape, head),
            Err(ResponseFault::Range.into()),
            "{head:?}"
        );
    }
    assert_eq!(
        s3::accept_get_head(SERVICE, suffix(5), ResponseHead::new(206)),
        Err(ResponseFault::Head.into())
    );
    assert_eq!(
        s3::accept_get_head(SERVICE, suffix(5), head(416, &[])),
        Ok(GetHeadOutcome::RangeNotSatisfiable { object_size: None })
    );

    let conditional = |condition| shape(GetKind::Bytes, RequestedRange::Whole, condition);
    let unconditional = shape(GetKind::Bytes, RequestedRange::Whole, ConditionKind::None);
    assert_eq!(
        s3::accept_get_head(
            SERVICE,
            conditional(ConditionKind::IfNoneMatch),
            head(304, &[("etag", b"\"etag\"")])
        ),
        Ok(GetHeadOutcome::NotModified {
            e_tag: Some(b"\"etag\"")
        })
    );
    assert_eq!(
        s3::accept_get_head(SERVICE, conditional(ConditionKind::IfMatch), head(412, &[])),
        Ok(GetHeadOutcome::PreconditionFailed)
    );
    assert_eq!(
        s3::accept_get_head(SERVICE, unconditional, head(304, &[])),
        Err(ResponseFault::Status.into())
    );
    assert_eq!(
        s3::accept_get_head(SERVICE, unconditional, head(412, &[])),
        Err(ResponseFault::Status.into())
    );
}
