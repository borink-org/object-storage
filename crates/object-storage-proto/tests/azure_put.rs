//! Azure write encoding and response interpretation.

use borink_object_storage_proto::{
    AzureNamespace, Blobs, ConditionKind, Container, Error, Failure, FailureClass, HeaderSpan,
    InvalidPlan, MetadataPair, Method, ObjectMeta, Payload, PhysicalPut, PutHeadOutcome, PutShape,
    ResponseFault, ResponseHead, ServiceErrorKind, Timestamps, TransactionalChecksum, WriteOptions,
    layered,
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
    let mut request_headers = [HeaderSpan::default(); 8];
    let blobs = blobs();
    let put = PhysicalPut::new("directory/object.txt");
    let content = [7u8; 4096];
    let mut buf = vec![
        0;
        layered::put_requirements(&blobs, &put, Payload::Slice(&content), &now())
            .map(|size| size.bytes)
            .unwrap()
    ];
    let request = blobs
        .encode_put(
            &mut buf,
            &mut request_headers,
            &put,
            Payload::Slice(&content),
            &now(),
        )
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
    let mut request_headers = [HeaderSpan::default(); 8];
    let blobs = blobs();
    let put = PhysicalPut::new("empty.bin");
    let mut buf = vec![
        0;
        layered::put_requirements(&blobs, &put, Payload::Slice(b""), &now())
            .map(|size| size.bytes)
            .unwrap()
    ];
    let request = blobs
        .encode_put(
            &mut buf,
            &mut request_headers,
            &put,
            Payload::Slice(b""),
            &now(),
        )
        .unwrap();

    assert!(request.headers().any(|h| h == ("content-length", "0")));
    assert!(request.payload().is_empty());
}

#[test]
fn a_conditional_write_sends_the_condition_header() {
    let mut request_headers = [HeaderSpan::default(); 8];
    let blobs = blobs();
    let create = PhysicalPut {
        condition: ConditionKind::IfNoneMatch,
        condition_value: Some(b"*"),
        ..PhysicalPut::new("object.bin")
    };
    let mut buf = vec![
        0;
        layered::put_requirements(&blobs, &create, Payload::Slice(b"one"), &now())
            .map(|size| size.bytes)
            .unwrap()
    ];
    let request = blobs
        .encode_put(
            &mut buf,
            &mut request_headers,
            &create,
            Payload::Slice(b"one"),
            &now(),
        )
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
            .map(|size| size.bytes)
            .unwrap()
    ];
    let request = blobs
        .encode_put(
            &mut buf,
            &mut request_headers,
            &replace,
            Payload::Slice(b"one"),
            &now(),
        )
        .unwrap();
    assert!(request.headers().any(|h| h == ("if-match", "\"etag\"")));
}

#[test]
fn the_requirement_grows_with_the_stated_length_but_not_with_the_content() {
    let mut request_headers = [HeaderSpan::default(); 8];
    let blobs = blobs();
    let put = PhysicalPut::new("object.bin");
    let short = layered::put_requirements(&blobs, &put, Payload::Slice(&[0; 9]), &now())
        .map(|size| size.bytes)
        .unwrap();
    let long = layered::put_requirements(&blobs, &put, Payload::Slice(&[0; 1_000_000]), &now())
        .map(|size| size.bytes)
        .unwrap();

    // "9" is one byte, "1000000" is seven, and nothing else differs.
    assert_eq!(long - short, 6);

    // The exact requirement is exact: one byte less does not encode.
    let mut buf = vec![0; short - 1];
    assert_eq!(
        blobs
            .encode_put(
                &mut buf,
                &mut request_headers,
                &put,
                Payload::Slice(&[0; 9]),
                &now()
            )
            .unwrap_err()
            .capacity()
            .unwrap()
            .required,
        short
    );
}

#[test]
fn a_write_plan_is_validated_before_any_byte_is_written() {
    let mut request_headers = [HeaderSpan::default(); 8];
    let blobs = blobs();
    for (put, expected) in [
        (PhysicalPut::new(""), InvalidPlan::EmptyKey),
        (
            PhysicalPut {
                condition: ConditionKind::IfMatch,
                condition_value: None,
                ..PhysicalPut::new("object.bin")
            },
            InvalidPlan::Condition,
        ),
        (
            PhysicalPut {
                condition: ConditionKind::None,
                condition_value: Some(b"\"etag\""),
                ..PhysicalPut::new("object.bin")
            },
            InvalidPlan::Condition,
        ),
    ] {
        assert_eq!(
            blobs
                .encode_put(
                    &mut [0; 512],
                    &mut request_headers,
                    &put,
                    Payload::Slice(b"one"),
                    &now()
                )
                .err(),
            Some(Error::InvalidPlan(expected))
        );
    }

    // A key longer than a flat account accepts is refused by character count,
    // not by byte count: these are two-byte characters. Only a client told it
    // is on a flat account refuses it.
    let long = "\u{e9}".repeat(1025);
    assert_eq!(
        blobs
            .with_namespace(AzureNamespace::Flat)
            .encode_put(
                &mut [0; 4096],
                &mut request_headers,
                &PhysicalPut::new(&long),
                Payload::Slice(b"one"),
                &now()
            )
            .err(),
        Some(Error::InvalidPlan(InvalidPlan::KeyTooLong))
    );

    // A client that does not know its account, and one told it is on a
    // hierarchical account, both send the key as written: only a flat account
    // refuses it, and the service says so.
    let mut buf = [0; 16384];
    for client in [blobs, blobs.with_namespace(AzureNamespace::Hierarchical)] {
        let request = client
            .encode_put(
                &mut buf,
                &mut request_headers,
                &PhysicalPut::new(&long),
                Payload::Slice(b"one"),
                &now(),
            )
            .unwrap();
        assert!(request.url().ends_with(&"%C3%A9".repeat(1025)));
    }
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
            last_modified: Some("Fri, 24 May 2013 00:00:00 GMT"),
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
    assert_eq!(
        blobs.accept_put_head(PutShape::default(), ResponseHead::new(412)),
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
    let mut head = ResponseHead::new(409);
    head.error_code = Some(b"BlobAlreadyExists");
    head.request_id = Some(b"request-123");
    assert!(matches!(
        blobs().accept_put_head(conditional(ConditionKind::IfNoneMatch), head),
        Ok(PutHeadOutcome::ServiceFailure(Failure {
            status: 409,
            class: FailureClass::Other,
            kind: Some(ServiceErrorKind::AlreadyExists),
            request_id: Some(b"request-123"),
        }))
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
    let mut streamed_headers = [HeaderSpan::default(); 8];
    let mut borrowed_headers = [HeaderSpan::default(); 8];
    let blobs = blobs();
    let put = PhysicalPut::new("object.bin");
    let content = [7u8; 4096];
    let streamed = Payload::Streamed { len: 4096 };

    let mut held = vec![
        0;
        layered::put_requirements(&blobs, &put, Payload::Slice(&content), &now())
            .map(|size| size.bytes)
            .unwrap()
    ];
    let mut sent = vec![
        0;
        layered::put_requirements(&blobs, &put, streamed, &now())
            .map(|size| size.bytes)
            .unwrap()
    ];
    let borrowed = blobs
        .encode_put(
            &mut held,
            &mut borrowed_headers,
            &put,
            Payload::Slice(&content),
            &now(),
        )
        .unwrap();
    let streaming = blobs
        .encode_put(&mut sent, &mut streamed_headers, &put, streamed, &now())
        .unwrap();

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
    let mut request_headers = [HeaderSpan::default(); 8];
    let blobs = blobs();
    let put = PhysicalPut::new("object.bin");
    // 5000 MiB is the most Azure writes in one request, so this cannot be a
    // slice on any machine. Only the streamed form can state it at all.
    let too_long = Payload::Streamed {
        len: 5000 * 1024 * 1024 + 1,
    };
    assert_eq!(
        blobs
            .encode_put(&mut [0; 512], &mut request_headers, &put, too_long, &now())
            .err(),
        Some(Error::InvalidPlan(InvalidPlan::PayloadTooLarge))
    );

    let longest = Payload::Streamed {
        len: 5000 * 1024 * 1024,
    };
    assert!(
        layered::put_requirements(&blobs, &put, longest, &now())
            .map(|size| size.bytes)
            .is_ok()
    );
}

#[test]
fn a_write_sends_one_header_per_metadata_pair_and_the_content_md5() {
    let blobs = blobs();
    let metadata = [
        MetadataPair {
            name: "source_mtime",
            value: "1787400000",
        },
        MetadataPair {
            name: "Source_Size",
            value: "4096",
        },
    ];
    let put = PhysicalPut {
        metadata: &metadata,
        options: WriteOptions {
            checksum: Some(TransactionalChecksum::Md5("rL0Y20zC+Fzt72VPzMSk2A==")),
            ..Default::default()
        },
        ..PhysicalPut::new("directory/object.txt")
    };
    let content = Payload::Slice(b"contents");
    let size = layered::put_requirements(&blobs, &put, content, &now()).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut request_headers = vec![HeaderSpan::default(); size.headers];
    let request = blobs
        .encode_put(&mut buf, &mut request_headers, &put, content, &now())
        .unwrap();

    let headers: Vec<_> = request.headers().collect();
    assert!(headers.contains(&("x-ms-meta-source_mtime", "1787400000")));
    // The name is written as the plan spelled it.
    assert!(headers.contains(&("x-ms-meta-Source_Size", "4096")));
    assert!(headers.contains(&("content-md5", "rL0Y20zC+Fzt72VPzMSk2A==")));
    // The five a plain write carries, one per pair, and the checksum.
    assert_eq!(headers.len(), 8);
}

#[test]
fn a_write_without_metadata_sends_the_same_head_as_before() {
    let blobs = blobs();
    let put = PhysicalPut::new("directory/object.txt");
    let content = Payload::Slice(b"contents");
    let size = layered::put_requirements(&blobs, &put, content, &now()).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut request_headers = vec![HeaderSpan::default(); size.headers];
    let request = blobs
        .encode_put(&mut buf, &mut request_headers, &put, content, &now())
        .unwrap();

    let names: Vec<_> = request.headers().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        [
            "authorization",
            "x-ms-date",
            "x-ms-version",
            "x-ms-blob-type",
            "content-length"
        ]
    );
}

#[test]
fn metadata_a_request_cannot_carry_is_refused_before_it_is_written() {
    let blobs = blobs();
    let duplicate = [
        MetadataPair {
            name: "mtime",
            value: "1",
        },
        MetadataPair {
            name: "MTime",
            value: "2",
        },
    ];
    for (metadata, expected) in [
        (
            [MetadataPair {
                name: "",
                value: "1",
            }]
            .as_slice(),
            InvalidPlan::MetadataName,
        ),
        (
            &[MetadataPair {
                name: "1st",
                value: "1",
            }],
            InvalidPlan::MetadataName,
        ),
        (
            &[MetadataPair {
                name: "source-mtime",
                value: "1",
            }],
            InvalidPlan::MetadataName,
        ),
        (
            &[MetadataPair {
                name: "mtime",
                value: "one\ntwo",
            }],
            InvalidPlan::MetadataValue,
        ),
        (
            &[MetadataPair {
                name: "mtime",
                value: "één",
            }],
            InvalidPlan::MetadataValue,
        ),
        // HTTP drops the spaces at either end of a header value, so the pair
        // would not come back as it was written.
        (
            &[MetadataPair {
                name: "mtime",
                value: " 1",
            }],
            InvalidPlan::MetadataValue,
        ),
        (
            &[MetadataPair {
                name: "mtime",
                value: "1 ",
            }],
            InvalidPlan::MetadataValue,
        ),
        (&duplicate, InvalidPlan::MetadataDuplicate),
    ] {
        let put = PhysicalPut {
            metadata,
            ..PhysicalPut::new("object.bin")
        };
        assert_eq!(
            blobs
                .encode_put(
                    &mut [0; 1024],
                    &mut [HeaderSpan::default(); 8],
                    &put,
                    Payload::Slice(b""),
                    &now()
                )
                .unwrap_err(),
            Error::InvalidPlan(expected),
            "{:?}",
            metadata[0].name
        );
    }
}

#[test]
fn a_crc64_is_sent_as_its_own_header() {
    let blobs = blobs();
    let put = PhysicalPut {
        options: WriteOptions {
            checksum: Some(TransactionalChecksum::Crc64("HZz9TO6x+RU=")),
            ..Default::default()
        },
        ..PhysicalPut::new("object.bin")
    };
    let content = Payload::Slice(b"0123456789");
    let size = layered::put_requirements(&blobs, &put, content, &now()).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut request_headers = vec![HeaderSpan::default(); size.headers];
    let request = blobs
        .encode_put(&mut buf, &mut request_headers, &put, content, &now())
        .unwrap();
    let headers: Vec<_> = request.headers().collect();
    assert!(headers.contains(&("x-ms-content-crc64", "HZz9TO6x+RU=")));
    assert!(!headers.iter().any(|(name, _)| *name == "content-md5"));
}

#[test]
fn a_computed_checksum_needs_a_provider_of_that_kind() {
    // This client has none, so both kinds are refused. `borink-crypto`'s
    // `encoder` tests cover a client that has one.
    for kind in [
        borink_object_storage_proto::ChecksumKind::Md5,
        borink_object_storage_proto::ChecksumKind::Crc64,
    ] {
        let options = WriteOptions {
            checksum: Some(TransactionalChecksum::Compute(kind)),
            ..Default::default()
        };
        assert_eq!(
            blobs()
                .encode_put(
                    &mut [0; 1024],
                    &mut [HeaderSpan::default(); 8],
                    &PhysicalPut {
                        options,
                        ..PhysicalPut::new("object.bin")
                    },
                    Payload::Slice(b"0123456789"),
                    &now()
                )
                .map(drop),
            Err(Error::InvalidPlan(InvalidPlan::Option)),
            "{kind:?}"
        );
    }
}

#[test]
fn a_checksum_that_is_not_the_base64_of_its_bytes_is_refused() {
    let blobs = blobs();
    let refused = |options: WriteOptions<'_>| {
        blobs
            .encode_put(
                &mut [0; 1024],
                &mut [HeaderSpan::default(); 8],
                &PhysicalPut {
                    options,
                    ..PhysicalPut::new("object.bin")
                },
                Payload::Slice(b""),
                &now(),
            )
            .map(drop)
            .unwrap_err()
    };
    for value in [
        "rL0Y20zC+Fzt72VPzMSk2A=",
        "rL0Y20zC+Fzt72VPzMSk2A===",
        "rL0Y20zC+Fzt72VPzMSk2A",
        "rL0Y20zC+Fzt72VPzMSk2!==",
        "HZz9TO6x+RU=",
        "",
    ] {
        assert_eq!(
            refused(WriteOptions {
                checksum: Some(TransactionalChecksum::Md5(value)),
                ..Default::default()
            }),
            Error::InvalidPlan(InvalidPlan::Checksum),
            "{value:?}"
        );
    }
    for value in [
        "HZz9TO6x+RU",
        "HZz9TO6x+RU==",
        "HZz9TO6x+R!=",
        "rL0Y20zC+Fzt72VPzMSk2A==",
        "",
    ] {
        assert_eq!(
            refused(WriteOptions {
                checksum: Some(TransactionalChecksum::Crc64(value)),
                ..Default::default()
            }),
            Error::InvalidPlan(InvalidPlan::Checksum),
            "{value:?}"
        );
    }
    // A whole-object write stores the MD5 it checks, so it takes no declared
    // one.
    assert_eq!(
        refused(WriteOptions {
            declared_md5: Some("rL0Y20zC+Fzt72VPzMSk2A=="),
            ..Default::default()
        }),
        Error::InvalidPlan(InvalidPlan::Option)
    );
}
