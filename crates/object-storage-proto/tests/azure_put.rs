//! Azure write encoding and response interpretation.

use borink_object_storage_proto::azure;
use borink_object_storage_proto::azure::{AzureNamespace, Container};
use borink_object_storage_proto::{
    ChecksumKind, ChecksumValue, ConditionKind, ConditionValue, Digest, Error, InvalidRequest,
    MetadataPair, Payload, PutItem, PutOptions, Timestamps, WireRequest,
};

fn container() -> Container<'static> {
    Container::new("https://account.blob.core.windows.net", "container").unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

// Writes `item` from a template of `options` for an account of `namespace`
// into `buf`.
fn encode_on<'r>(
    namespace: AzureNamespace,
    options: &PutOptions<'_>,
    item: &PutItem<'_>,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>, Error> {
    let mut bytes = vec![0; 2048];
    let template = azure::prepare_put(&container(), "token", namespace, options, &mut bytes)?;
    let placed = azure::encode_put(&template, &bytes, item, &now(), buf)?;
    Ok(placed.request(buf))
}

fn encode<'r>(
    options: &PutOptions<'_>,
    item: &PutItem<'_>,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>, Error> {
    encode_on(AzureNamespace::Unknown, options, item, buf)
}

#[test]
fn a_write_is_validated_before_any_byte_is_written() {
    let if_match = PutOptions {
        condition: ConditionKind::IfMatch,
        ..PutOptions::default()
    };
    let content = Payload::Slice(b"one");
    for (options, item, expected) in [
        (
            PutOptions::default(),
            PutItem::new("", content),
            InvalidRequest::EmptyKey,
        ),
        (
            if_match,
            PutItem::new("object.bin", content),
            InvalidRequest::Condition,
        ),
        (
            PutOptions::default(),
            PutItem {
                condition_value: Some(ConditionValue::ETag(b"\"etag\"")),
                ..PutItem::new("object.bin", content)
            },
            InvalidRequest::Condition,
        ),
    ] {
        let mut untouched = [0xAA; 1024];
        assert_eq!(
            encode(&options, &item, &mut untouched).err(),
            Some(Error::InvalidRequest(expected))
        );
        assert!(untouched.iter().all(|byte| *byte == 0xAA));
    }

    // A key longer than a flat account accepts is refused by character count,
    // not by byte count: these are two-byte characters. Only a template told
    // it is for a flat account refuses it.
    let long = "\u{e9}".repeat(1025);
    let item = PutItem::new(&long, content);
    assert_eq!(
        encode_on(
            AzureNamespace::Flat,
            &PutOptions::default(),
            &item,
            &mut [0; 16384]
        )
        .err(),
        Some(Error::InvalidRequest(InvalidRequest::KeyTooLong))
    );

    // A template that does not know its account, and one told it is for a
    // hierarchical account, both send the key as written: only a flat
    // account refuses it, and the service says so.
    for namespace in [AzureNamespace::Unknown, AzureNamespace::Hierarchical] {
        let mut buf = [0; 16384];
        let request = encode_on(namespace, &PutOptions::default(), &item, &mut buf).unwrap();
        assert!(request.url().ends_with(&"%C3%A9".repeat(1025)));
    }
}

#[test]
fn streamed_content_writes_the_same_head_without_the_bytes() {
    let content = [7u8; 4096];
    let options = PutOptions::default();
    let held = PutItem::new("object.bin", Payload::Slice(&content));
    let streamed = PutItem::new("object.bin", Payload::Streamed { len: 4096 });
    let (mut held_buf, mut streamed_buf) = ([0; 1024], [0; 1024]);
    let held = encode(&options, &held, &mut held_buf).unwrap();
    let streaming = encode(&options, &streamed, &mut streamed_buf).unwrap();

    // The head is what the service sees, and it does not know the difference.
    assert_eq!(held.url(), streaming.url());
    assert_eq!(held.head(), streaming.head());

    // Neither lends the content: both state its length, and the host sends
    // the bytes it holds.
    assert_eq!(held.payload(), Payload::Streamed { len: 4096 });
    assert_eq!(streaming.payload(), Payload::Streamed { len: 4096 });
}

#[test]
fn a_streamed_payload_is_refused_at_the_same_length_as_a_held_one() {
    // 5000 MiB is the most Azure writes in one request, so this cannot be a
    // slice on any machine. Only the streamed form can state it at all.
    let options = PutOptions::default();
    let too_long = PutItem::new(
        "object.bin",
        Payload::Streamed {
            len: azure::MAX_PUT_LEN + 1,
        },
    );
    assert_eq!(
        encode(&options, &too_long, &mut [0; 1024]).err(),
        Some(Error::InvalidRequest(InvalidRequest::PayloadTooLarge))
    );
    let longest = PutItem::new(
        "object.bin",
        Payload::Streamed {
            len: azure::MAX_PUT_LEN,
        },
    );
    assert!(encode(&options, &longest, &mut [0; 1024]).is_ok());
}

#[test]
fn metadata_a_request_cannot_carry_is_refused_before_it_is_written() {
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
    let pair = |name, value| [MetadataPair { name, value }];
    for (metadata, expected) in [
        (pair("", "1").as_slice(), InvalidRequest::MetadataName),
        (&pair("1st", "1"), InvalidRequest::MetadataName),
        (&pair("source-mtime", "1"), InvalidRequest::MetadataName),
        (&pair("mtime", "one\ntwo"), InvalidRequest::MetadataValue),
        (&pair("mtime", "één"), InvalidRequest::MetadataValue),
        // HTTP drops the spaces at either end of a header value, so the pair
        // would not come back as it was written.
        (&pair("mtime", " 1"), InvalidRequest::MetadataValue),
        (&pair("mtime", "1 "), InvalidRequest::MetadataValue),
        (&duplicate, InvalidRequest::MetadataDuplicate),
    ] {
        let options = PutOptions {
            metadata,
            ..PutOptions::default()
        };
        assert_eq!(
            azure::prepare_put(
                &container(),
                "token",
                AzureNamespace::Unknown,
                &options,
                &mut [0; 1024]
            )
            .err(),
            Some(Error::InvalidRequest(expected)),
            "{:?}",
            metadata[0].name
        );
    }
}

#[test]
fn a_crc64_is_sent_as_its_own_header_in_azure_byte_order() {
    let options = PutOptions {
        checksum: Some(ChecksumKind::Crc64),
        ..PutOptions::default()
    };
    // The text, and the digest it is the little-endian base64 of.
    for checksum in [
        ChecksumValue::Base64("HZz9TO6x+RU="),
        ChecksumValue::Digest(Digest::crc64(u64::from_le_bytes([
            0x1d, 0x9c, 0xfd, 0x4c, 0xee, 0xb1, 0xf9, 0x15,
        ]))),
    ] {
        let item = PutItem {
            checksum: Some(checksum),
            ..PutItem::new("object.bin", Payload::Slice(b"0123456789"))
        };
        let mut buf = [0; 1024];
        let request = encode(&options, &item, &mut buf).unwrap();
        let headers: Vec<_> = request.headers().collect();
        assert!(
            headers.contains(&("x-ms-content-crc64", "HZz9TO6x+RU=")),
            "{headers:?}"
        );
        assert!(!headers.iter().any(|(name, _)| *name == "content-md5"));
    }
}

#[test]
fn a_checksum_that_is_not_the_base64_of_its_kind_is_refused() {
    let refused = |kind, value| {
        let options = PutOptions {
            checksum: Some(kind),
            ..PutOptions::default()
        };
        let item = PutItem {
            checksum: Some(ChecksumValue::Base64(value)),
            ..PutItem::new("object.bin", Payload::Slice(b""))
        };
        encode(&options, &item, &mut [0; 1024])
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
            refused(ChecksumKind::Md5, value),
            Error::InvalidRequest(InvalidRequest::Checksum),
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
            refused(ChecksumKind::Crc64, value),
            Error::InvalidRequest(InvalidRequest::Checksum),
            "{value:?}"
        );
    }
    // A digest of another kind than the template's, and a checksum the
    // template does not send, are refused too.
    let options = PutOptions {
        checksum: Some(ChecksumKind::Md5),
        ..PutOptions::default()
    };
    let other = PutItem {
        checksum: Some(ChecksumValue::Digest(Digest::crc64(1))),
        ..PutItem::new("object.bin", Payload::Slice(b""))
    };
    assert_eq!(
        encode(&options, &other, &mut [0; 1024]).map(drop),
        Err(Error::InvalidRequest(InvalidRequest::Checksum))
    );
    assert_eq!(
        encode(&PutOptions::default(), &other, &mut [0; 1024]).map(drop),
        Err(Error::InvalidRequest(InvalidRequest::Checksum))
    );
    // Azure takes no other kind.
    let options = PutOptions {
        checksum: Some(ChecksumKind::Crc32c),
        ..PutOptions::default()
    };
    assert_eq!(
        azure::prepare_put(
            &container(),
            "token",
            AzureNamespace::Unknown,
            &options,
            &mut [0; 1024]
        )
        .err(),
        Some(Error::InvalidRequest(InvalidRequest::Option))
    );
}
