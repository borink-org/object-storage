//! S3 requests signed with each SHA-256 provider, against known answers.
//!
//! The first answer is the GET Object example of the AWS Signature Version 4
//! documentation for S3. The others were computed by a separate signer,
//! written from the same specification, that reproduces that example.

#![cfg(any(feature = "sha256-rustcrypto", feature = "sha256-minimal"))]

use borink_object_storage_proto::s3::{Addressing, Bucket, Objects, PayloadHash, Service};
use borink_object_storage_proto::sigv4::{Credentials, Sha256Provider};
use borink_object_storage_proto::{
    ConditionKind, HeaderSpan, MetadataPair, Payload, PhysicalDelete, PhysicalGet, PhysicalPut,
    RequestedRange, Timestamps, WireRequest, layered,
};

const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[allow(clippy::vec_init_then_push)]
fn providers() -> Vec<Sha256Provider> {
    let mut all = Vec::new();
    #[cfg(feature = "sha256-rustcrypto")]
    all.push(borink_object_storage_crypto::SHA256_RUSTCRYPTO);
    #[cfg(feature = "sha256-minimal")]
    all.push(borink_object_storage_crypto::SHA256_MINIMAL);
    all
}

// Every provider, each as a client with no signing key, with the key for
// `now`, and with the key for the day before, which must not be used.
fn clients<'a>(
    bucket: Bucket<'a>,
    credentials: Credentials<'a>,
    now: &Timestamps,
) -> Vec<Objects<'a>> {
    let yesterday = Timestamps::from_unix(now.unix() - 86_400);
    providers()
        .into_iter()
        .flat_map(|sha256| {
            let objects = Objects::new(bucket, credentials, sha256);
            [
                objects,
                objects.with_signing_key(now),
                objects.with_signing_key(&yesterday),
            ]
        })
        .collect()
}

fn credentials() -> Credentials<'static> {
    Credentials::new(KEY_ID, SECRET, borink_object_storage_crypto::wipe).unwrap()
}

fn headers<'r>(request: &WireRequest<'r>) -> Vec<(&'r str, &'r str)> {
    request.headers().collect()
}

// The order of the headers means nothing to the service, so it is not
// compared.
fn assert_headers(request: &WireRequest<'_>, expected: &[(&str, &str)]) {
    let mut actual = headers(request);
    actual.sort();
    let mut expected = expected.to_vec();
    expected.sort();
    assert_eq!(actual, expected);
}

fn unhex(text: &str) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * index..2 * index + 2], 16).unwrap();
    }
    bytes
}

#[test]
fn signs_the_get_object_example_of_the_aws_documentation() {
    let bucket = Bucket::new(
        "https://s3.amazonaws.com",
        "examplebucket",
        "us-east-1",
        Service::Aws,
    )
    .unwrap()
    .with_addressing(Addressing::VirtualHosted);
    let now = Timestamps::from_unix(1_369_353_600);
    let get = PhysicalGet {
        range: RequestedRange::Bounded { start: 0, end: 10 },
        ..PhysicalGet::new("test.txt")
    };
    for objects in clients(bucket, credentials(), &now) {
        let size = layered::s3::get_requirements(&objects, &get, &now).unwrap();
        let mut buf = vec![0; size.bytes];
        let mut slots = vec![HeaderSpan::default(); size.headers];
        let request = objects
            .encode_get(&mut buf, &mut slots, &get, &now)
            .unwrap();
        assert_eq!(
            request.url(),
            "https://examplebucket.s3.amazonaws.com/test.txt"
        );
        assert_headers(
            &request,
            &[
                (
                    "authorization",
                    "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
                     SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
                     Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
                ),
                ("x-amz-date", "20130524T000000Z"),
                ("x-amz-content-sha256", EMPTY_SHA256),
                ("range", "bytes=0-9"),
            ],
        );
    }
}

#[test]
fn signs_a_path_style_write_with_metadata_a_condition_and_a_session_token() {
    const TOKEN: &str = "FwoGZXIvYXdzEJr//////////wEaDH+token==";
    const CONTENT_SHA256: &str = "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072";
    let bucket = Bucket::new_allowing_http(
        "http://127.0.0.1:9000",
        "objects",
        "auto",
        Service::Compatible,
    )
    .unwrap();
    let credentials = credentials().with_session_token(TOKEN).unwrap();
    let now = Timestamps::from_unix(1_787_400_061);
    // Two spaces inside a value are signed as one and sent as two.
    let metadata = [
        MetadataPair {
            name: "Colour",
            value: "dark  red",
        },
        MetadataPair {
            name: "a-b",
            value: "1",
        },
    ];
    let put = PhysicalPut {
        condition: ConditionKind::IfNoneMatch,
        condition_value: Some(b"*"),
        metadata: &metadata,
        ..PhysicalPut::new("dir/a key+é.txt")
    };
    let content = Payload::Slice(b"Welcome to Amazon S3.");
    for objects in clients(bucket, credentials, &now) {
        // The digest the encoder computes and the one you pass are signed
        // alike.
        for hash in [
            PayloadHash::Compute,
            PayloadHash::Sha256(unhex(CONTENT_SHA256)),
        ] {
            let size = layered::s3::put_requirements(&objects, &put, content, hash, &now).unwrap();
            let mut buf = vec![0; size.bytes];
            let mut slots = vec![HeaderSpan::default(); size.headers];
            let request = objects
                .encode_put(&mut buf, &mut slots, &put, content, hash, &now)
                .unwrap();
            assert_eq!(
                request.url(),
                "http://127.0.0.1:9000/objects/dir/a%20key%2B%C3%A9.txt"
            );
            assert_headers(
                &request,
                &[
                    (
                        "authorization",
                        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260822/auto/s3/aws4_request, \
                         SignedHeaders=host;if-none-match;x-amz-content-sha256;x-amz-date;\
                         x-amz-meta-a-b;x-amz-meta-colour;x-amz-security-token, \
                         Signature=dce8aef7b480731201f0112d84a501b3a4945bddc4307b0aaa252fdcc30cf704",
                    ),
                    ("x-amz-date", "20260822T120101Z"),
                    ("x-amz-content-sha256", CONTENT_SHA256),
                    ("x-amz-security-token", TOKEN),
                    ("if-none-match", "*"),
                    ("x-amz-meta-colour", "dark  red"),
                    ("x-amz-meta-a-b", "1"),
                    ("content-length", "21"),
                ],
            );
            assert_eq!(request.payload(), content);
        }
    }
}

#[test]
fn signs_a_virtual_hosted_conditional_removal() {
    let bucket = Bucket::new(
        "https://s3.eu-west-1.amazonaws.com:443",
        "examplebucket",
        "eu-west-1",
        Service::Aws,
    )
    .unwrap()
    .with_addressing(Addressing::VirtualHosted);
    let now = Timestamps::from_unix(1_787_400_061);
    let delete = PhysicalDelete {
        condition: ConditionKind::IfMatch,
        condition_value: Some(b"\"9b2cf535f27731c974343645a3985328\""),
        ..PhysicalDelete::new("~tilde/(paren)*")
    };
    for objects in clients(bucket, credentials(), &now) {
        let size = layered::s3::delete_requirements(&objects, &delete, &now).unwrap();
        let mut buf = vec![0; size.bytes];
        let mut slots = vec![HeaderSpan::default(); size.headers];
        let request = objects
            .encode_delete(&mut buf, &mut slots, &delete, &now)
            .unwrap();
        // The default port is left out of the URL, and so of the host that
        // is signed.
        assert_eq!(
            request.url(),
            "https://examplebucket.s3.eu-west-1.amazonaws.com/~tilde/%28paren%29%2A"
        );
        assert_headers(
            &request,
            &[
                (
                    "authorization",
                    "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260822/eu-west-1/s3/aws4_request, \
                     SignedHeaders=host;if-match;x-amz-content-sha256;x-amz-date, \
                     Signature=cbddf776cb64df5e2dd6e5b811a9db824e6dbca80ae25f3a5b6a57e77f09a515",
                ),
                ("x-amz-date", "20260822T120101Z"),
                ("x-amz-content-sha256", EMPTY_SHA256),
                ("if-match", "\"9b2cf535f27731c974343645a3985328\""),
            ],
        );
    }
}

#[test]
fn signs_a_conditional_read_of_a_suffix() {
    let bucket = Bucket::new(
        "https://s3.eu-west-1.amazonaws.com",
        "examplebucket",
        "eu-west-1",
        Service::Aws,
    )
    .unwrap();
    let now = Timestamps::from_unix(1_787_400_061);
    let get = PhysicalGet {
        range: RequestedRange::Suffix(5),
        condition: ConditionKind::IfNoneMatch,
        condition_value: Some(b"\"abc\""),
        ..PhysicalGet::new("photos/2026.jpg")
    };
    for objects in clients(bucket, credentials(), &now) {
        let size = layered::s3::get_requirements(&objects, &get, &now).unwrap();
        let mut buf = vec![0; size.bytes];
        let mut slots = vec![HeaderSpan::default(); size.headers];
        let request = objects
            .encode_get(&mut buf, &mut slots, &get, &now)
            .unwrap();
        assert_eq!(
            request.url(),
            "https://s3.eu-west-1.amazonaws.com/examplebucket/photos/2026.jpg"
        );
        assert_headers(
            &request,
            &[
                (
                    "authorization",
                    "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260822/eu-west-1/s3/aws4_request, \
                     SignedHeaders=host;if-none-match;range;x-amz-content-sha256;x-amz-date, \
                     Signature=436dff107580c84468dc31145bb4ae7cd867ed2f7050a145fb493a2bc8d3bcc1",
                ),
                ("x-amz-date", "20260822T120101Z"),
                ("x-amz-content-sha256", EMPTY_SHA256),
                ("range", "bytes=-5"),
                ("if-none-match", "\"abc\""),
            ],
        );
    }
}

#[test]
fn signs_a_streamed_write_without_its_content() {
    let bucket = Bucket::new(
        "https://s3.eu-west-1.amazonaws.com",
        "examplebucket",
        "eu-west-1",
        Service::Aws,
    )
    .unwrap()
    .with_addressing(Addressing::VirtualHosted);
    let now = Timestamps::from_unix(1_787_400_061);
    let put = PhysicalPut::new("large.bin");
    let content = Payload::Streamed { len: 1024 };
    for objects in clients(bucket, credentials(), &now) {
        let size =
            layered::s3::put_requirements(&objects, &put, content, PayloadHash::Unsigned, &now)
                .unwrap();
        let mut buf = vec![0; size.bytes];
        let mut slots = vec![HeaderSpan::default(); size.headers];
        let request = objects
            .encode_put(
                &mut buf,
                &mut slots,
                &put,
                content,
                PayloadHash::Unsigned,
                &now,
            )
            .unwrap();
        assert_headers(
            &request,
            &[
                (
                    "authorization",
                    "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260822/eu-west-1/s3/aws4_request, \
                     SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
                     Signature=735a3d750545e7ba05747dbefef64b9caa34b8a3c07b00dcafae4a447bffc2ba",
                ),
                ("x-amz-date", "20260822T120101Z"),
                ("x-amz-content-sha256", "UNSIGNED-PAYLOAD"),
                ("content-length", "1024"),
            ],
        );
        assert_eq!(request.payload(), content);
    }
}

#[cfg(feature = "md5-rustcrypto")]
#[test]
fn a_computed_md5_is_sent_beside_the_signature() {
    let bucket = Bucket::new("https://s3.example.com", "bucket", "auto", Service::Aws).unwrap();
    let now = Timestamps::from_unix(1_787_400_061);
    let put = PhysicalPut {
        options: borink_object_storage_proto::WriteOptions {
            checksum: Some(borink_object_storage_proto::TransactionalChecksum::Compute(
                borink_object_storage_proto::ChecksumKind::Md5,
            )),
            ..Default::default()
        },
        ..PhysicalPut::new("object.bin")
    };
    let content = Payload::Slice(b"0123456789");
    for objects in clients(bucket, credentials(), &now) {
        let objects = objects.with_checksum(borink_object_storage_crypto::MD5_RUSTCRYPTO);
        let size =
            layered::s3::put_requirements(&objects, &put, content, PayloadHash::Unsigned, &now)
                .unwrap();
        let mut buf = vec![0; size.bytes];
        let mut slots = vec![HeaderSpan::default(); size.headers];
        let request = objects
            .encode_put(
                &mut buf,
                &mut slots,
                &put,
                content,
                PayloadHash::Unsigned,
                &now,
            )
            .unwrap();
        assert!(headers(&request).contains(&("content-md5", "eB5eJF1ptWaXm4bijSPyxw==")));
    }
}
