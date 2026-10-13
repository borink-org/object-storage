//! Azure bearer GET integration tests.

use borink_object_storage_proto::azure::{self, Template, metadata_name};
use borink_object_storage_proto::{
    AzureNamespace, ConditionKind, ConditionValue, Container, Error, GetHeadOutcome, GetItem,
    GetOptions, GetShape, InvalidRequest, Method, RequestedRange, ResponseHead, Timestamps,
    VERSION, WireRequest,
};

fn container() -> Container<'static> {
    Container::new("https://account.blob.core.windows.net", "objects").unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

// The template of reads with `options`, in a buffer of its own.
fn template(options: GetOptions, namespace: AzureNamespace) -> (Template, Vec<u8>) {
    let mut bytes = vec![0; 512];
    let template = azure::prepare_get(
        &container(),
        "access-token",
        namespace,
        &options,
        &mut bytes,
    )
    .unwrap();
    (template, bytes)
}

// Writes `item` from a template of `options` into `buf`.
fn encode<'r>(
    options: GetOptions,
    item: &GetItem<'_>,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>, Error> {
    let (template, bytes) = template(options, AzureNamespace::Unknown);
    azure::encode_get(&template, &bytes, item, &now(), buf)
}

fn shape(range: RequestedRange) -> GetShape {
    GetItem::new("key")
        .with_range(range)
        .shape(&GetOptions::new().ranged())
}

#[test]
fn response_ranges_must_have_representable_exclusive_ends() {
    let shape = shape(RequestedRange::Offset(0));
    for value in [
        "bytes 0-18446744073709551615/*",
        "bytes 1-18446744073709551615/*",
        "bytes 0-18446744073709551615/18446744073709551615",
    ] {
        let head = ResponseHead::from_headers(206, [("content-range", value.as_bytes())]);
        assert_eq!(
            azure::accept_get_head(shape, head),
            Err(Error::Response(
                borink_object_storage_proto::ResponseFault::Head
            ))
        );
    }
    for value in [
        "bytes 0-18446744073709551614/*",
        "bytes 0-18446744073709551614/18446744073709551615",
    ] {
        let head = ResponseHead::from_headers(206, [("content-range", value.as_bytes())]);
        let GetHeadOutcome::Body { body, .. } = azure::accept_get_head(shape, head).unwrap() else {
            panic!("expected a body");
        };
        assert_eq!(body.expected_len, Some(u64::MAX));
    }
}

#[test]
fn encodes_a_bearer_get_in_caller_memory() {
    let mut buf = [0; 512];
    let request = encode(
        GetOptions::new(),
        &GetItem::new("directory/a key+é"),
        &mut buf,
    )
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
            ("x-ms-version", VERSION),
            ("x-ms-date", "Sat, 22 Aug 2026 12:00:00 GMT"),
        ]
    );
    assert_eq!(
        request.head(),
        format!(
            "GET /objects/directory/a%20key%2B%C3%A9 HTTP/1.1\r\n\
             host: account.blob.core.windows.net\r\n\
             authorization: Bearer access-token\r\n\
             x-ms-version: {VERSION}\r\n\
             x-ms-date: Sat, 22 Aug 2026 12:00:00 GMT\r\n\r\n"
        )
        .as_bytes()
    );
}

#[test]
fn the_request_borrows_nothing_but_its_buffer() {
    let mut buf = [0; 512];
    let (template, bytes) = template(
        GetOptions {
            condition: ConditionKind::IfMatch,
            ..GetOptions::new()
        },
        AzureNamespace::Unknown,
    );
    // The template's bytes, the key, the condition value and the timestamp
    // are all temporaries.
    let request = azure::encode_get(
        &template,
        &bytes.clone(),
        &GetItem {
            condition_value: Some(ConditionValue::ETag(String::from("\"etag\"").as_bytes())),
            ..GetItem::new(&String::from("object"))
        },
        &Timestamps::from_unix(1_787_400_000),
        &mut buf,
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
    let get = GetItem::new("object");
    let Err(Error::Capacity(capacity)) = encode(GetOptions::new(), &get, &mut []) else {
        panic!("an empty buffer is refused with its size");
    };
    let mut short = vec![0; capacity.required - 1];
    assert_eq!(
        encode(GetOptions::new(), &get, &mut short)
            .unwrap_err()
            .capacity()
            .map(|capacity| capacity.required),
        Some(capacity.required)
    );
    let mut exact = vec![0; capacity.required];
    encode(GetOptions::new(), &get, &mut exact).unwrap();

    // A template is sized the same way.
    let options = GetOptions::new();
    let Err(Error::Capacity(capacity)) = azure::prepare_get(
        &container(),
        "token",
        AzureNamespace::Unknown,
        &options,
        &mut [],
    ) else {
        panic!("an empty buffer is refused with its size");
    };
    let mut exact = vec![0; capacity.required];
    azure::prepare_get(
        &container(),
        "token",
        AzureNamespace::Unknown,
        &options,
        &mut exact,
    )
    .unwrap();
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
        azure::prepare_get(
            &container,
            "token\r\nheader",
            AzureNamespace::Unknown,
            &GetOptions::new(),
            &mut [0; 512]
        ),
        Err(Error::InvalidToken)
    ));
}

#[test]
fn refuses_invalid_reads_before_writing_anything() {
    let ranged = GetOptions::new().ranged();
    let if_match = GetOptions {
        condition: ConditionKind::IfMatch,
        ..GetOptions::new()
    };
    let with_value = |value| GetItem {
        condition_value: Some(value),
        ..GetItem::new("object")
    };
    let cases = [
        (
            GetOptions::new(),
            GetItem::new(""),
            InvalidRequest::EmptyKey,
        ),
        (
            ranged,
            GetItem::new("object").with_range(RequestedRange::Bounded { start: 6, end: 2 }),
            InvalidRequest::Range,
        ),
        (
            ranged,
            GetItem::new("object").with_range(RequestedRange::Suffix(4)),
            InvalidRequest::UnsupportedRange,
        ),
        // A range where the template sends none, and none where it sends one.
        (
            GetOptions::new(),
            GetItem::new("object").with_range(RequestedRange::Offset(2)),
            InvalidRequest::Range,
        ),
        (ranged, GetItem::new("object"), InvalidRequest::Range),
        // A kind without a value and a value without a kind are both invalid.
        (if_match, GetItem::new("object"), InvalidRequest::Condition),
        (
            GetOptions::new(),
            with_value(ConditionValue::ETag(b"\"etag\"")),
            InvalidRequest::Condition,
        ),
        (
            if_match,
            with_value(ConditionValue::ETag(b"etag\r\nheader")),
            InvalidRequest::Condition,
        ),
    ];
    for (options, get, expected) in cases {
        let mut untouched = [0xAA; 256];
        assert_eq!(
            encode(options, &get, &mut untouched).err(),
            Some(Error::InvalidRequest(expected)),
            "{get:?}"
        );
        assert!(untouched.iter().all(|byte| *byte == 0xAA));
        // An empty buffer reports the same refusal unchanged.
        assert_eq!(
            encode(options, &get, &mut []).err(),
            Some(Error::InvalidRequest(expected))
        );
    }

    // A HEAD reads no range, so no template of ranged HEADs is written.
    assert_eq!(
        azure::prepare_get(
            &container(),
            "token",
            AzureNamespace::Unknown,
            &GetOptions::head().ranged(),
            &mut [0; 512]
        )
        .err(),
        Some(Error::InvalidRequest(InvalidRequest::RangedHead))
    );
}

#[test]
fn a_url_longer_than_azure_reads_is_refused_before_any_byte_is_written() {
    use borink_object_storage_proto::azure::MAX_URL_LEN;

    // The bytes of the URL that are not the key.
    let fixed = "https://account.blob.core.windows.net/objects/".len();
    // A space is one UTF-16 unit and three bytes of URL, so the bytes of the
    // URL and the units of the name are far apart here.
    let spaces = (MAX_URL_LEN - fixed - 1) / 3;
    let key = format!("{}k", " ".repeat(spaces));
    let mut buf = vec![0; 3 * MAX_URL_LEN];
    let request = encode(GetOptions::new(), &GetItem::new(&key), &mut buf).unwrap();
    assert_eq!(request.url().len(), MAX_URL_LEN);
    assert!(key.encode_utf16().count() * 2 < MAX_URL_LEN);

    // One more byte of URL, and the read is refused before the buffer is
    // touched, whatever the account, and with a buffer too small as well.
    let over = format!(" {key}");
    for namespace in [AzureNamespace::Unknown, AzureNamespace::Hierarchical] {
        let (template, bytes) = template(GetOptions::new(), namespace);
        let mut untouched = [0xAA; 256];
        assert_eq!(
            azure::encode_get(
                &template,
                &bytes,
                &GetItem::new(&over),
                &now(),
                &mut untouched
            )
            .map(drop),
            Err(Error::InvalidRequest(InvalidRequest::UrlTooLong))
        );
        assert!(untouched.iter().all(|byte| *byte == 0xAA));
    }
    assert_eq!(
        InvalidRequest::UrlTooLong.azure_rejection(AzureNamespace::Unknown),
        Some(borink_object_storage_proto::AzureRejection {
            status: 414,
            code: ""
        })
    );
}

/// The keys a read may name, and the ones it may not because they would
/// name something else by the time the request arrives.
///
/// Each refusal here was measured against the live service, not read off
/// a specification: a name is refused where storing it would store it under a
/// name the caller did not write.
#[test]
fn a_key_that_would_not_survive_the_journey_is_refused() {
    let (flat, flat_bytes) = template(GetOptions::new(), AzureNamespace::Flat);
    let (unknown, unknown_bytes) = template(GetOptions::new(), AzureNamespace::Unknown);
    let refused_by = |template: &Template, bytes: &[u8], key: &str| {
        azure::encode_get(
            template,
            bytes,
            &GetItem::new(key),
            &now(),
            &mut vec![0; 40_000],
        )
        .map(drop)
    };
    let refused = |key: &str| refused_by(&flat, &flat_bytes, key);

    // A flat account counts a name in UTF-16 code units, so a character
    // outside the basic plane counts twice. 512 of them is the limit, and 1024
    // of a character inside it is too. Only a template told it is for a flat
    // account refuses the key itself; the default one sends it.
    assert!(refused(&"a".repeat(1024)).is_ok());
    assert!(refused(&"é".repeat(1024)).is_ok());
    assert!(refused(&"🦀".repeat(512)).is_ok());
    for over in ["a".repeat(1025), "é".repeat(1025), "🦀".repeat(513)] {
        assert_eq!(
            refused(&over),
            Err(Error::InvalidRequest(InvalidRequest::KeyTooLong)),
            "{} UTF-16 code units",
            over.encode_utf16().count()
        );
        assert!(refused_by(&unknown, &unknown_bytes, &over).is_ok());
    }

    // Azure drops a dot from the end of every segment, so a key with one
    // names an object that will not be there.
    for key in ["dot.", "a/dot.", "dotseg./x", "a./b", "..", ".", "a/../"] {
        assert_eq!(
            refused(key),
            Err(Error::InvalidRequest(InvalidRequest::KeyWouldBeNormalized)),
            "{key:?}"
        );
    }

    // Azure refuses a control character in a name, so a key holding one is a
    // key that cannot become a request.
    for key in ["a\u{1}b", "a\tb", "a\nb", "a\rb", "\u{1f}", "a\u{7f}b"] {
        assert_eq!(
            refused(key),
            Err(Error::InvalidRequest(InvalidRequest::KeyControlCharacter)),
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
            Err(Error::InvalidRequest(InvalidRequest::KeyWouldBeNormalized)),
            "{key:?}"
        );
    }

    // Azure takes 255 `/`-delimited segments and refuses 256.
    assert!(refused(&vec!["s"; 255].join("/")).is_ok());
    assert_eq!(
        refused(&vec!["s"; 256].join("/")),
        Err(Error::InvalidRequest(InvalidRequest::KeyTooManySegments))
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

#[test]
fn a_metadata_read_reports_its_pairs_through_the_response_header_names() {
    let mut buf = [0; 512];
    let get = GetItem::new("object.bin");
    let request = encode(GetOptions::head(), &get, &mut buf).unwrap();
    assert_eq!(request.method(), Method::Head);

    // The pairs are one header each, so the head this crate reads carries
    // none of them. Collect them from the response headers yourself.
    let response = [
        ("Content-Length", b"8".as_slice()),
        ("ETag", b"\"0x8DF0046E8E555AF\""),
        ("x-ms-meta-source_mtime", b"1787400000"),
        ("X-MS-META-Label", b"a b"),
        ("x-ms-meta-", b"nameless"),
    ];
    let pairs: Vec<_> = response
        .iter()
        .filter_map(|(name, value)| Some((metadata_name(name)?, *value)))
        .collect();
    assert_eq!(
        pairs,
        [
            ("source_mtime", b"1787400000".as_slice()),
            ("Label", b"a b"),
        ]
    );

    match azure::accept_get_head(
        get.shape(&GetOptions::head()),
        ResponseHead::from_headers(200, response),
    )
    .unwrap()
    {
        GetHeadOutcome::Complete { meta } => assert_eq!(meta.size, Some(8)),
        other => panic!("unexpected outcome {other:?}"),
    }
}
