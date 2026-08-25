//! Azure bearer GET integration tests.

use borink_object_storage::{
    Blobs, BodyWindow, ConditionKind, Container, Error, GetHead, GetHeadOutcome, GetKind,
    InvalidPlan, ObjectMeta, PhysicalGet, RequestedRange, Timestamps, VERSION, layered,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "objects").unwrap(),
        "access-token",
    )
    .unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

#[test]
fn encodes_a_bearer_get_in_caller_memory() {
    let blobs = blobs();
    let mut buf = [0; 256];
    let request = blobs
        .encode_get(&mut buf, &PhysicalGet::new("directory/a key+é"), &now())
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
fn the_head_borrows_nothing_the_caller_passed_in() {
    let blobs = blobs();
    let mut buf = [0; 256];
    // The key, the condition value and the timestamp are all temporaries.
    let request = blobs
        .encode_get(
            &mut buf,
            &PhysicalGet {
                key: &String::from("object"),
                condition: ConditionKind::IfMatch,
                condition_value: Some(String::from("\"etag\"").as_bytes()),
                ..PhysicalGet::new("")
            },
            &Timestamps::from_unix(1_787_400_000),
        )
        .unwrap();

    assert!(
        request
            .headers()
            .any(|header| header == ("x-ms-date", "Sat, 22 Aug 2026 12:00:00 GMT"))
    );
    assert!(
        request
            .headers()
            .any(|header| header == ("if-match", "\"etag\""))
    );
}

#[test]
fn reports_the_exact_required_capacity() {
    let blobs = blobs();
    let get = PhysicalGet::new("object");
    let error = blobs.encode_get(&mut [], &get, &now()).unwrap_err();
    let Error::Capacity(capacity) = error else {
        panic!("unexpected error: {error}");
    };
    assert_eq!(capacity.available, 0);
    assert_eq!(
        layered::requirements(&blobs, &get, &now()),
        Ok(capacity.required)
    );

    let mut short = vec![0; capacity.required - 1];
    assert_eq!(
        blobs
            .encode_get(&mut short, &get, &now())
            .unwrap_err()
            .capacity()
            .map(|capacity| capacity.required),
        Some(capacity.required)
    );

    let mut exact = vec![0; capacity.required];
    blobs.encode_get(&mut exact, &get, &now()).unwrap();
}

#[test]
fn encodes_ranges_conditions_and_metadata_plans() {
    let blobs = blobs();
    let mut buf = [0; 256];
    let get = PhysicalGet {
        key: "object",
        kind: GetKind::Bytes,
        range: RequestedRange::Bounded { start: 2, end: 6 },
        condition: ConditionKind::IfNoneMatch,
        condition_value: Some(b"\"etag\""),
    };
    let request = blobs.encode_get(&mut buf, &get, &now()).unwrap();
    assert_eq!(request.method(), "GET");
    assert!(
        request
            .headers()
            .any(|header| header == ("range", "bytes=2-5"))
    );
    assert!(
        request
            .headers()
            .any(|header| header == ("if-none-match", "\"etag\""))
    );

    let metadata = blobs
        .encode_get(
            &mut buf,
            &PhysicalGet {
                kind: GetKind::Metadata,
                ..PhysicalGet::new("object")
            },
            &now(),
        )
        .unwrap();
    assert_eq!(metadata.method(), "HEAD");

    let head = GetHead::from_headers(
        206,
        [
            ("Content-Range", b"bytes 2-5/10".as_slice()),
            ("ETag", b"\"etag\""),
            ("x-ms-version-id", b"version-1"),
        ],
    );
    let Ok(GetHeadOutcome::Body { meta, body, .. }) = blobs.accept_get_head(get.shape(), head)
    else {
        panic!("a ranged read returns a body");
    };
    assert_eq!(
        meta,
        ObjectMeta {
            size: Some(10),
            e_tag: Some(b"\"etag\""),
            version: Some(b"version-1"),
            ..ObjectMeta::default()
        }
    );
    assert_eq!(
        body,
        BodyWindow {
            object_offset: 2,
            expected_len: Some(4),
            object_size: Some(10),
        }
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
        Container::new("https://tést.example", "objects"),
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

#[test]
fn refuses_invalid_plans_before_writing_anything() {
    let condition = |condition, condition_value| PhysicalGet {
        condition,
        condition_value,
        ..PhysicalGet::new("object")
    };
    let ranged = |range| PhysicalGet {
        range,
        ..PhysicalGet::new("object")
    };
    let cases = [
        (PhysicalGet::new(""), InvalidPlan::Key),
        (
            ranged(RequestedRange::Bounded { start: 6, end: 2 }),
            InvalidPlan::Range,
        ),
        (
            ranged(RequestedRange::Suffix(4)),
            InvalidPlan::UnsupportedRange,
        ),
        (
            PhysicalGet {
                kind: GetKind::Metadata,
                range: RequestedRange::Offset(2),
                ..PhysicalGet::new("object")
            },
            InvalidPlan::RangedMetadata,
        ),
        // A kind without a value and a value without a kind are both invalid.
        (
            condition(ConditionKind::IfMatch, None),
            InvalidPlan::Condition,
        ),
        (
            condition(ConditionKind::None, Some(b"\"etag\"")),
            InvalidPlan::Condition,
        ),
        (
            condition(ConditionKind::IfMatch, Some(b"etag\r\nheader")),
            InvalidPlan::Condition,
        ),
    ];

    let blobs = blobs();
    let mut buf = [0; 256];
    for (get, expected) in cases {
        assert_eq!(
            blobs.encode_get(&mut buf, &get, &now()).err(),
            Some(Error::InvalidPlan(expected)),
            "{get:?}"
        );
        // The layered requirement path reports the same refusal unchanged.
        assert_eq!(
            layered::requirements(&blobs, &get, &now()),
            Err(Error::InvalidPlan(expected))
        );
    }
}
