//! S3 PUT: the writes the client refuses, and the response heads it reads.

use borink_object_storage_proto::s3::{self, Bucket, MAX_PUT_LEN, PayloadSigning, Service};
use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
use borink_object_storage_proto::{
    ChecksumKind, ChecksumValue, ConditionKind, ConditionValue, Digest, Error, InvalidPlan,
    MetadataPair, Payload, PutItem, PutOptions, Timestamps,
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

// Writes `item` from a template of `options` into a buffer, and returns
// its fields, or the reason the crate refused it.
fn encode(
    options: &PutOptions<'_>,
    signing: PayloadSigning,
    item: &PutItem<'_>,
    content_sha256: Option<&[u8; 32]>,
) -> Result<Vec<(String, String)>, InvalidPlan> {
    let refusal = |error| match error {
        Error::InvalidPlan(reason) => reason,
        other => panic!("expected an invalid plan, got {other:?}"),
    };
    let mut bytes = vec![0; 2048];
    let template = s3::prepare_put(&bucket(), &credentials(), options, signing, &mut bytes)
        .map_err(refusal)?;
    let mut buf = vec![0; 4096];
    let unsigned = s3::encode_put(&template, &bytes, item, content_sha256, &now(), &mut buf)
        .map_err(refusal)?;
    let request = s3::set_signature(&unsigned, &mut buf, &[0; 32]);
    Ok(request
        .headers()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect())
}

fn refused(
    options: &PutOptions<'_>,
    signing: PayloadSigning,
    item: &PutItem<'_>,
    content_sha256: Option<&[u8; 32]>,
) -> InvalidPlan {
    encode(options, signing, item, content_sha256).unwrap_err()
}

#[test]
fn a_write_that_s3_cannot_take_is_refused() {
    let slice = Payload::Slice(b"0123456789");
    let plain = PutOptions::default();
    let unsigned = PayloadSigning::Unsigned;

    // S3 writes on `If-None-Match` only when no object holds the key.
    let create = PutOptions {
        condition: ConditionKind::IfNoneMatch,
        ..plain
    };
    let etag = PutItem {
        condition_value: Some(ConditionValue::ETag(b"\"etag\"")),
        ..PutItem::new("k", slice)
    };
    assert_eq!(
        refused(&create, unsigned, &etag, None),
        InvalidPlan::Condition
    );
    // AWS takes no date condition on a write.
    let dated = PutOptions {
        condition: ConditionKind::IfModifiedSince,
        ..plain
    };
    assert_eq!(
        refused(&dated, unsigned, &PutItem::new("k", slice), None),
        InvalidPlan::Condition
    );
    // A signed template needs the SHA-256 of each write, and an unsigned one
    // takes none.
    assert_eq!(
        refused(
            &plain,
            PayloadSigning::Signed,
            &PutItem::new("k", slice),
            None
        ),
        InvalidPlan::Option
    );
    assert_eq!(
        refused(&plain, unsigned, &PutItem::new("k", slice), Some(&[0; 32])),
        InvalidPlan::Option
    );
    let too_long = PutItem::new(
        "k",
        Payload::Streamed {
            len: MAX_PUT_LEN + 1,
        },
    );
    assert_eq!(
        refused(&plain, unsigned, &too_long, None),
        InvalidPlan::PayloadTooLarge
    );
    let md5 = PutOptions {
        checksum: Some(ChecksumKind::Md5),
        ..plain
    };
    let not_base64 = PutItem {
        checksum: Some(ChecksumValue::Base64("not base64")),
        ..PutItem::new("k", slice)
    };
    assert_eq!(
        refused(&md5, unsigned, &not_base64, None),
        InvalidPlan::Checksum
    );

    let metadata = |pairs: &[MetadataPair<'_>]| {
        let options = PutOptions {
            metadata: pairs,
            ..plain
        };
        refused(&options, unsigned, &PutItem::new("k", slice), None)
    };
    let pair = |name, value| MetadataPair { name, value };
    assert_eq!(metadata(&[pair("", "v")]), InvalidPlan::MetadataName);
    assert_eq!(metadata(&[pair("a b", "v")]), InvalidPlan::MetadataName);
    assert_eq!(metadata(&[pair("a", "v\n")]), InvalidPlan::MetadataValue);
    assert_eq!(
        metadata(&[pair("Name", "v"), pair("name", "w")]),
        InvalidPlan::MetadataDuplicate
    );
}

#[test]
fn a_checksum_goes_where_s3_reads_it() {
    let slice = Payload::Slice(b"0123456789");
    let with = |kind, value| {
        let options = PutOptions {
            checksum: Some(kind),
            ..PutOptions::default()
        };
        let item = PutItem {
            checksum: Some(value),
            ..PutItem::new("k", slice)
        };
        encode(&options, PayloadSigning::Unsigned, &item, None).unwrap()
    };
    // An MD5 is `Content-MD5`, which S3 does not sign.
    let headers = with(
        ChecksumKind::Md5,
        ChecksumValue::Base64("eB5eJF1ptWaXm4bijSPyxw=="),
    );
    assert!(headers.contains(&(
        "content-md5".to_owned(),
        "eB5eJF1ptWaXm4bijSPyxw==".to_owned()
    )));
    assert!(
        !headers
            .iter()
            .any(|(name, value)| name == "authorization" && value.contains("content-md5"))
    );
    // S3 reads a CRC64 in big-endian order: the same digest that Azure takes
    // as `HZz9TO6x+RU=`.
    let crc64 = Digest::crc64(u64::from_le_bytes([
        0x1d, 0x9c, 0xfd, 0x4c, 0xee, 0xb1, 0xf9, 0x15,
    ]));
    let headers = with(ChecksumKind::Crc64, ChecksumValue::Digest(crc64));
    assert!(
        headers.contains(&(
            "x-amz-checksum-crc64nvme".to_owned(),
            "Ffmx7kz9nB0=".to_owned()
        )),
        "{headers:?}"
    );
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "authorization"
                && value.contains("x-amz-checksum-crc64nvme"))
    );
}
