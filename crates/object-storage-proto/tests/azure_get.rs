//! Azure bearer GET integration tests.

use borink_object_storage_proto::{
    Blobs, BodyWindow, ConditionKind, Container, Error, GetHeadOutcome, GetKind, InvalidPlan,
    Method, ObjectMeta, PhysicalGet, RequestedRange, ResponseHead, Timestamps, VERSION, layered,
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

    assert_eq!(request.method(), Method::Get);
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
        layered::get_requirements(&blobs, &get, &now()),
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
    assert_eq!(request.method(), Method::Get);
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
    assert_eq!(metadata.method(), Method::Head);

    let head = ResponseHead::from_headers(
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
            layered::get_requirements(&blobs, &get, &now()),
            Err(Error::InvalidPlan(expected))
        );
    }
}

/// The keys a plan may name, and the ones it may not because they would name
/// something else by the time the request arrives.
///
/// Each refusal here is a measurement from the live suite, not a rule read off
/// a specification: a name is refused where storing it would store it under a
/// name the caller did not write.
#[test]
fn a_key_that_would_not_survive_the_journey_is_refused() {
    let refused = |key: &str| {
        borink_object_storage_proto::layered::get_requirements(
            &blobs(),
            &PhysicalGet::new(key),
            &now(),
        )
        .map(drop)
    };

    // Azure counts a name in UTF-16 code units, so a character outside the
    // basic plane counts twice. 512 of them is the limit, and 1024 of a
    // character inside it is too.
    assert!(refused(&"a".repeat(1024)).is_ok());
    assert!(refused(&"é".repeat(1024)).is_ok());
    assert!(refused(&"🦀".repeat(512)).is_ok());
    for over in ["a".repeat(1025), "é".repeat(1025), "🦀".repeat(513)] {
        assert_eq!(
            refused(&over),
            Err(Error::InvalidPlan(InvalidPlan::Key)),
            "{} UTF-16 code units",
            over.encode_utf16().count()
        );
    }

    // Azure drops a dot from the end of every segment, so a key with one
    // names an object that will not be there.
    for key in ["dot.", "a/dot.", "dotseg./x", "a./b", "..", ".", "a/../"] {
        assert_eq!(
            refused(key),
            Err(Error::InvalidPlan(InvalidPlan::Key)),
            "{key:?}"
        );
    }

    // Azure refuses a control character in a name, so a key holding one is a
    // key that cannot become a request.
    for key in ["a\u{1}b", "a\tb", "a\nb", "a\rb", "\u{1f}", "a\u{7f}b"] {
        assert_eq!(
            refused(key),
            Err(Error::InvalidPlan(InvalidPlan::Key)),
            "{key:?}"
        );
    }
    // A character outside ASCII is not a control character, whatever its
    // bytes look like one at a time.
    for key in ["café", "🦀", "\u{85}", "\u{a0}"] {
        assert!(refused(key).is_ok(), "{key:?}");
    }

    // A host resolves these out of the URL before sending it, so the request
    // would name another object entirely.
    for key in ["a/../b", "a/./b", "../b", "./b", "a/.."] {
        assert_eq!(
            refused(key),
            Err(Error::InvalidPlan(InvalidPlan::Key)),
            "{key:?}"
        );
    }

    // Azure takes 255 `/`-delimited segments and refuses 256.
    assert!(refused(&vec!["s"; 255].join("/")).is_ok());
    assert_eq!(
        refused(&vec!["s"; 256].join("/")),
        Err(Error::InvalidPlan(InvalidPlan::Key))
    );

    // A dot that is not the whole segment and not at the end is ordinary text,
    // and so is a slash wherever it falls but the cases above.
    for key in [
        "a.b",
        "..a",
        "a..b",
        "a/b",
        "a//b",
        "trailing/",
        "/leading",
        "..leading",
        "a.b/c",
    ] {
        assert!(refused(key).is_ok(), "{key:?}");
    }
}
