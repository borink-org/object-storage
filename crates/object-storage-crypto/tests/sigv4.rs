//! S3 requests signed with each SHA-256 provider, against known answers.
//!
//! The first answer is the GET Object example of the AWS Signature Version 4
//! documentation for S3. The others were computed by a separate signer,
//! written from the same specification, that reproduces that example.

#![cfg(any(feature = "sha256-rustcrypto", feature = "sha256-minimal"))]

use borink_object_storage_crypto::{Sha256, sigv4};
use borink_object_storage_proto::s3::{
    self, Addressing, Bucket, PayloadSigning, Service, Template, Unsigned,
};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{
    ChecksumKind, ChecksumValue, ConditionKind, ConditionValue, DeleteItem, DeleteOptions, Error,
    GetItem, GetOptions, MetadataPair, Payload, PutItem, PutOptions, RequestedRange, Timestamps,
};

const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn credentials() -> Credentials<'static> {
    Credentials::new(KEY_ID, SECRET, borink_object_storage_crypto::wipe).unwrap()
}

// A request from the template that `prepare` writes, signed with each
// provider, as its URL, its sorted headers and its HTTP/1.1 head. The
// template and the request are written into buffers of exactly the size
// that an empty buffer reports.
type Signed = (String, Vec<(String, String)>, String);
type Prepare<'p> = dyn Fn(&mut [u8]) -> borink_object_storage_proto::Result<Template> + 'p;
type Encode<'e, 'c> =
    dyn Fn(&Template, &[u8], &mut [u8]) -> borink_object_storage_proto::Result<Unsigned<'c>> + 'e;

fn signed<'c>(
    credentials: &Credentials<'_>,
    now: &Timestamps,
    prepare: &Prepare<'_>,
    encode: &Encode<'_, 'c>,
) -> Vec<Signed> {
    fn exact<T>(encode: impl Fn(&mut [u8]) -> borink_object_storage_proto::Result<T>) -> Vec<u8> {
        let Err(Error::Capacity(capacity)) = encode(&mut []) else {
            panic!("an empty buffer is refused with its size");
        };
        let mut buf = vec![0; capacity.required];
        encode(&mut buf).unwrap();
        assert!(encode(&mut buf[..capacity.required - 1]).is_err());
        buf
    }
    fn one<'c, S: Sha256>(
        credentials: &Credentials<'_>,
        now: &Timestamps,
        prepare: &Prepare<'_>,
        encode: &Encode<'_, 'c>,
    ) -> Signed {
        let bytes = exact(prepare);
        let template = prepare(&mut bytes.clone()).unwrap();
        let key = sigv4::signing_key::<S>(&template, &bytes, credentials, now).unwrap();
        let stamp = sigv4::stamp(&key, &template, &bytes, now).unwrap();
        let mut buf = exact(|buf| encode(&template, &bytes, buf));
        let unsigned = encode(&template, &bytes, &mut buf).unwrap();
        let request = sigv4::sign(&stamp, &unsigned, &mut buf);
        let mut headers: Vec<_> = request
            .headers()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        headers.sort();
        (
            request.url().to_owned(),
            headers,
            String::from_utf8(request.head().to_vec()).unwrap(),
        )
    }
    Vec::from([
        #[cfg(feature = "sha256-rustcrypto")]
        one::<borink_object_storage_crypto::Sha256RustCrypto>(credentials, now, prepare, encode),
        #[cfg(feature = "sha256-minimal")]
        one::<borink_object_storage_crypto::Sha256Minimal>(credentials, now, prepare, encode),
    ])
}

fn signed_get(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &GetOptions,
    item: &GetItem<'_>,
    now: &Timestamps,
) -> Vec<Signed> {
    signed(
        credentials,
        now,
        &|buf| s3::prepare_get(bucket, credentials, options, buf),
        &|template, bytes, buf| s3::encode_get(template, bytes, item, now, buf),
    )
}

fn sorted(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut expected: Vec<_> = expected
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    expected.sort();
    expected
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
    let item = GetItem::new("test.txt").with_range(RequestedRange::Bounded { start: 0, end: 10 });
    let expected = sorted(&[
        (
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
        ),
        ("x-amz-date", "20130524T000000Z"),
        ("x-amz-content-sha256", EMPTY_SHA256),
        ("range", "bytes=0-9"),
    ]);
    let options = GetOptions::new().ranged();
    for (url, headers, head) in signed_get(&bucket, &credentials(), &options, &item, &now) {
        assert_eq!(url, "https://examplebucket.s3.amazonaws.com/test.txt");
        assert_eq!(headers, expected);
        let mut lines = head.split("\r\n");
        assert_eq!(lines.next(), Some("GET /test.txt HTTP/1.1"));
        assert_eq!(lines.next(), Some("host: examplebucket.s3.amazonaws.com"));
        assert!(head.ends_with("\r\n\r\n"), "{head}");
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
    let options = GetOptions {
        condition: ConditionKind::IfNoneMatch,
        ..GetOptions::new().ranged()
    };
    let item = GetItem {
        condition_value: Some(ConditionValue::ETag(b"\"abc\"")),
        ..GetItem::new("photos/2026.jpg").with_range(RequestedRange::Suffix(5))
    };
    let expected = sorted(&[
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
    ]);
    for (url, headers, _) in signed_get(&bucket, &credentials(), &options, &item, &now) {
        assert_eq!(
            url,
            "https://s3.eu-west-1.amazonaws.com/examplebucket/photos/2026.jpg"
        );
        assert_eq!(headers, expected);
    }
}

#[test]
fn a_batch_is_signed_as_each_request_is() {
    use borink_object_storage_crypto::Sha256RustCrypto as S;
    let bucket = Bucket::new(
        "https://fly.storage.tigris.dev",
        "bucket",
        "auto",
        Service::Compatible,
    )
    .unwrap();
    let credentials = credentials();
    let now = Timestamps::from_unix(1_787_400_061);
    let options = GetOptions::new().ranged();
    let mut bytes = vec![0; 1024];
    let template = s3::prepare_get(&bucket, &credentials, &options, &mut bytes).unwrap();
    let key = sigv4::signing_key::<S>(&template, &bytes, &credentials, &now).unwrap();
    let stamp = sigv4::stamp(&key, &template, &bytes, &now).unwrap();
    let keys = ["a", "dir/b c", "é/+"];
    let mut bufs = vec![vec![0u8; 1024]; keys.len()];
    let unsigned: Vec<_> = keys
        .iter()
        .zip(bufs.iter_mut())
        .map(|(key, buf)| {
            let item = GetItem::new(key).with_range(RequestedRange::Bounded { start: 1, end: 9 });
            s3::encode_get(&template, &bytes, &item, &now, buf).unwrap()
        })
        .collect();
    let canonicals: Vec<&[u8]> = unsigned
        .iter()
        .zip(&bufs)
        .map(|(u, buf)| u.canonical(buf))
        .collect();
    let mut macs = vec![[0u8; 32]; keys.len()];
    sigv4::sign_batch(&stamp, &canonicals, &mut macs);
    for ((unsigned, mut buf), mac) in unsigned.iter().zip(bufs.clone()).zip(&macs) {
        let mut copy = buf.clone();
        let batched: Vec<_> = s3::finish(unsigned, &mut buf, mac)
            .headers()
            .map(|(n, v)| (n.to_owned(), v.to_owned()))
            .collect();
        let single: Vec<_> = sigv4::sign(&stamp, unsigned, &mut copy)
            .headers()
            .map(|(n, v)| (n.to_owned(), v.to_owned()))
            .collect();
        assert_eq!(batched, single);
    }
}

fn unhex(text: &str) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * index..2 * index + 2], 16).unwrap();
    }
    bytes
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
    let options = DeleteOptions {
        condition: ConditionKind::IfMatch,
        ..DeleteOptions::new()
    };
    let item = DeleteItem {
        condition_value: Some(ConditionValue::ETag(
            b"\"9b2cf535f27731c974343645a3985328\"",
        )),
        ..DeleteItem::new("~tilde/(paren)*")
    };
    let expected = sorted(&[
        (
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260822/eu-west-1/s3/aws4_request, \
             SignedHeaders=host;if-match;x-amz-content-sha256;x-amz-date, \
             Signature=cbddf776cb64df5e2dd6e5b811a9db824e6dbca80ae25f3a5b6a57e77f09a515",
        ),
        ("x-amz-date", "20260822T120101Z"),
        ("x-amz-content-sha256", EMPTY_SHA256),
        ("if-match", "\"9b2cf535f27731c974343645a3985328\""),
    ]);
    let credentials = credentials();
    for (url, headers, _) in signed(
        &credentials,
        &now,
        &|buf| s3::prepare_delete(&bucket, &credentials, &options, buf),
        &|template, bytes, buf| s3::encode_delete(template, bytes, &item, &now, buf),
    ) {
        // The default port is left out of the URL, and so of the host that
        // is signed.
        assert_eq!(
            url,
            "https://examplebucket.s3.eu-west-1.amazonaws.com/~tilde/%28paren%29%2A"
        );
        assert_eq!(headers, expected);
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
    let options = PutOptions {
        condition: ConditionKind::IfNoneMatch,
        metadata: &metadata,
        ..PutOptions::default()
    };
    let content = b"Welcome to Amazon S3.";
    let item = PutItem {
        condition_value: Some(ConditionValue::ETag(b"*")),
        ..PutItem::new("dir/a key+é.txt", Payload::Slice(content))
    };
    let digest = unhex(CONTENT_SHA256);
    #[cfg(feature = "sha256-rustcrypto")]
    assert_eq!(
        sigv4::sha256::<borink_object_storage_crypto::Sha256RustCrypto>(content),
        digest
    );
    let expected = sorted(&[
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
    ]);
    for (url, headers, _) in signed(
        &credentials,
        &now,
        &|buf| s3::prepare_put(&bucket, &credentials, &options, PayloadSigning::Signed, buf),
        &|template, bytes, buf| s3::encode_put(template, bytes, &item, Some(&digest), &now, buf),
    ) {
        assert_eq!(
            url,
            "http://127.0.0.1:9000/objects/dir/a%20key%2B%C3%A9.txt"
        );
        assert_eq!(headers, expected);
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
    let options = PutOptions::default();
    let item = PutItem::new("large.bin", Payload::Streamed { len: 1024 });
    let expected = sorted(&[
        (
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260822/eu-west-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=735a3d750545e7ba05747dbefef64b9caa34b8a3c07b00dcafae4a447bffc2ba",
        ),
        ("x-amz-date", "20260822T120101Z"),
        ("x-amz-content-sha256", "UNSIGNED-PAYLOAD"),
        ("content-length", "1024"),
    ]);
    let credentials = credentials();
    for (_, headers, _) in signed(
        &credentials,
        &now,
        &|buf| {
            s3::prepare_put(
                &bucket,
                &credentials,
                &options,
                PayloadSigning::Unsigned,
                buf,
            )
        },
        &|template, bytes, buf| s3::encode_put(template, bytes, &item, None, &now, buf),
    ) {
        assert_eq!(headers, expected);
    }
}

#[test]
fn an_md5_is_sent_beside_the_signature() {
    use borink_object_storage_crypto::{Checksum, Md5RustCrypto};
    let bucket = Bucket::new("https://s3.example.com", "bucket", "auto", Service::Aws).unwrap();
    let now = Timestamps::from_unix(1_787_400_061);
    let options = PutOptions {
        checksum: Some(ChecksumKind::Md5),
        ..PutOptions::default()
    };
    let content = b"0123456789";
    let mut md5 = Md5RustCrypto::default();
    md5.update(content);
    let item = PutItem {
        checksum: Some(ChecksumValue::Digest(md5.finish())),
        ..PutItem::new("object.bin", Payload::Slice(content))
    };
    let credentials = credentials();
    for (_, headers, _) in signed(
        &credentials,
        &now,
        &|buf| {
            s3::prepare_put(
                &bucket,
                &credentials,
                &options,
                PayloadSigning::Unsigned,
                buf,
            )
        },
        &|template, bytes, buf| s3::encode_put(template, bytes, &item, None, &now, buf),
    ) {
        assert!(headers.contains(&(
            "content-md5".to_owned(),
            "eB5eJF1ptWaXm4bijSPyxw==".to_owned()
        )));
    }
}
