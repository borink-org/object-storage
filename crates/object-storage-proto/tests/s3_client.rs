//! S3 client construction, service rules and request sizing.
//!
//! The signatures themselves are checked against known answers in
//! `crates/object-storage-crypto/tests/sigv4.rs`, which has SHA-256
//! implementations. The providers here compute nothing, and `REFUSES` panics
//! if it is asked to.

use borink_object_storage_proto::s3::{Bucket, MAX_METADATA_LEN, Objects, PayloadHash, Service};
use borink_object_storage_proto::sigv4::{
    Credentials, Sha256Provider, Sha256State, wipe_best_effort,
};
use borink_object_storage_proto::{
    BodyWindow, CapacityError, Condition, DeleteHeadOutcome, Error, GetHeadOutcome, HeaderSpan,
    InvalidPlan, MetadataPair, Payload, PhysicalDelete, PhysicalGet, PhysicalPut, RequestedRange,
    ResponseFault, ResponseHead, Timestamps, layered,
};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "session-token";

const ZEROS: Sha256Provider =
    Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32], |_, _| [0; 32]);

// A provider that fails the test if the encoder hashes anything.
const REFUSES: Sha256Provider = Sha256Provider::new(
    Sha256State::uninit,
    |_, _| panic!("the encoder hashed content"),
    |_| panic!("the encoder finished a hash"),
    |_, _| panic!("the encoder computed an HMAC"),
);

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

fn head<'h>(status: u16, headers: &[(&'h str, &'h [u8])]) -> ResponseHead<'h> {
    ResponseHead::from_headers(status, headers.iter().copied())
}

#[test]
fn a_bucket_refuses_what_a_signed_request_cannot_carry() {
    for endpoint in [
        "s3.example.com",
        "https://",
        "https://:443",
        "https://s3.example.com/path",
    ] {
        assert_eq!(
            Bucket::new(endpoint, "bucket", "auto", Service::Aws).map(drop),
            Err(Error::InvalidEndpoint),
            "{endpoint}"
        );
    }
    for name in ["", "a/b", "a b", "a?b", "bücket"] {
        assert_eq!(
            Bucket::new("https://s3.example.com", name, "auto", Service::Aws).map(drop),
            Err(Error::InvalidContainer),
            "{name}"
        );
    }
    let long = "r".repeat(65);
    for region in ["", "eu west", "eu/west", &long] {
        assert_eq!(
            Bucket::new("https://s3.example.com", "bucket", region, Service::Aws).map(drop),
            Err(Error::InvalidRegion),
            "{region}"
        );
    }
    assert!(Bucket::new("https://s3.example.com", "bucket", &long[1..], Service::Aws).is_ok());
}

#[test]
fn credentials_refuse_what_the_authorization_header_cannot_carry() {
    let long = "s".repeat(125);
    for (key_id, secret) in [
        ("", SECRET),
        ("AKIA/EXAMPLE", SECRET),
        ("AKIA,EXAMPLE", SECRET),
        ("AKIA EXAMPLE", SECRET),
        ("AKIAÉ", SECRET),
        ("AKIAIOSFODNN7EXAMPLE", ""),
        ("AKIAIOSFODNN7EXAMPLE", &long),
    ] {
        assert_eq!(
            Credentials::new(key_id, secret, wipe_best_effort).map(drop),
            Err(Error::InvalidCredentials),
            "{key_id} {secret}"
        );
    }
    assert!(Credentials::new("AKIAIOSFODNN7EXAMPLE", &long[1..], wipe_best_effort).is_ok());
    let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", SECRET, wipe_best_effort).unwrap();
    for token in ["", "a\nb", "é"] {
        assert_eq!(
            credentials.with_session_token(token).map(drop),
            Err(Error::InvalidCredentials),
            "{token:?}"
        );
    }
}

#[test]
fn debug_output_hides_the_secret_and_the_token() {
    let text = format!("{:?}", objects().with_signing_key(&now()));
    assert!(text.contains("AKIAIOSFODNN7EXAMPLE"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
}

#[test]
fn requirements_are_exact_and_compute_no_signature() {
    let objects = Objects::new(bucket(), credentials(), REFUSES);
    let now = now();
    let get = PhysicalGet {
        range: RequestedRange::Suffix(9),
        ..PhysicalGet::new("a key")
    };
    let metadata = [MetadataPair {
        name: "Colour",
        value: "red",
    }];
    let put = PhysicalPut {
        metadata: &metadata,
        ..PhysicalPut::new("a key")
    };
    let content = Payload::Slice(b"0123456789");
    let delete = PhysicalDelete::new("a key");
    let sizes = [
        layered::s3::get_requirements(&objects, &get, &now).unwrap(),
        layered::s3::put_requirements(&objects, &put, content, PayloadHash::Compute, &now).unwrap(),
        layered::s3::delete_requirements(&objects, &delete, &now).unwrap(),
    ];
    // Every S3 request carries the authorization, the date and the content
    // hash, and this one a session token too.
    assert_eq!(
        [sizes[0].headers, sizes[1].headers, sizes[2].headers],
        [5, 6, 4]
    );

    let objects = Objects::new(bucket(), credentials(), ZEROS);
    for (index, size) in sizes.into_iter().enumerate() {
        let encode = |bytes: usize, headers: usize| {
            let mut buf = vec![0; bytes];
            let mut slots = vec![HeaderSpan::default(); headers];
            match index {
                0 => objects
                    .encode_get(&mut buf, &mut slots, &get, &now)
                    .map(drop),
                1 => objects
                    .encode_put(
                        &mut buf,
                        &mut slots,
                        &put,
                        content,
                        PayloadHash::Compute,
                        &now,
                    )
                    .map(drop),
                _ => objects
                    .encode_delete(&mut buf, &mut slots, &delete, &now)
                    .map(drop),
            }
        };
        let short = Err(Error::Capacity(CapacityError {
            required: size.bytes,
            required_headers: size.headers,
        }));
        assert_eq!(encode(size.bytes, size.headers), Ok(()), "{index}");
        assert_eq!(encode(size.bytes - 1, size.headers), short, "{index}");
        assert_eq!(encode(size.bytes, size.headers - 1), short, "{index}");
    }
}

#[test]
fn an_aws_bucket_name_follows_the_rules_that_aws_documents() {
    let long = "a".repeat(64);
    for name in [
        "ab",
        long.as_str(),
        "Bucket",
        "my_bucket",
        "-bucket",
        "bucket-",
        "my..bucket",
        "192.168.5.4",
        "xn--bucket",
        "sthree-bucket",
        "amzn-s3-demo-bucket",
        "bucket-s3alias",
        "bucket--ol-s3",
        "bucket.mrap",
        "bucket--x-s3",
        "bucket--table-s3",
    ] {
        let bucket = |service| Bucket::new("https://s3.example.com", name, "us-east-1", service);
        assert_eq!(
            bucket(Service::Aws).map(drop),
            Err(Error::InvalidContainer),
            "{name}"
        );
        assert!(bucket(Service::Compatible).is_ok(), "{name}");
    }
    for name in ["abc", "my.bucket-1", &long[1..], "1.2.3", "1.2.3.4a"] {
        assert!(
            Bucket::new("https://s3.example.com", name, "us-east-1", Service::Aws).is_ok(),
            "{name}"
        );
    }
    for region in ["EU-west-1", "eu_west_1", "eu.west"] {
        let bucket = |service| Bucket::new("https://s3.example.com", "bucket", region, service);
        assert_eq!(
            bucket(Service::Aws).map(drop),
            Err(Error::InvalidRegion),
            "{region}"
        );
        assert!(bucket(Service::Compatible).is_ok(), "{region}");
    }
}

#[test]
fn a_compatible_client_sends_what_aws_would_refuse() {
    let now = now();
    let content = Payload::Slice(b"0");
    let etag_create = PhysicalPut::new("k").with_condition(Condition::IfNoneMatch(b"\"etag\""));
    let delete = PhysicalDelete::new("k").with_condition(Condition::IfNoneMatch(b"\"etag\""));
    // AWS counts the name and the value: one byte and the rest.
    let fits = "v".repeat(MAX_METADATA_LEN - 1);
    let fits = [MetadataPair {
        name: "a",
        value: &fits,
    }];
    let over = "v".repeat(MAX_METADATA_LEN);
    let over = [MetadataPair {
        name: "a",
        value: &over,
    }];
    let fits = PhysicalPut {
        metadata: &fits,
        ..PhysicalPut::new("k")
    };
    let over = PhysicalPut {
        metadata: &over,
        ..PhysicalPut::new("k")
    };
    let put = |objects: &Objects<'_>, put: &PhysicalPut<'_>| {
        layered::s3::put_requirements(objects, put, content, PayloadHash::Compute, &now).map(drop)
    };
    let remove =
        |objects: &Objects<'_>| layered::s3::delete_requirements(objects, &delete, &now).map(drop);

    let aws = objects();
    assert_eq!(put(&aws, &etag_create), Err(InvalidPlan::Condition.into()));
    assert_eq!(remove(&aws), Err(InvalidPlan::Condition.into()));
    assert_eq!(put(&aws, &fits), Ok(()));
    assert_eq!(put(&aws, &over), Err(InvalidPlan::MetadataTooLarge.into()));

    let compatible = compatible();
    assert_eq!(put(&compatible, &etag_create), Ok(()));
    assert_eq!(remove(&compatible), Ok(()));
    assert_eq!(put(&compatible, &over), Ok(()));
}

#[test]
fn an_aws_client_accepts_only_what_aws_answers() {
    let get = PhysicalGet::new("k").shape();
    let delete = PhysicalDelete::new("k").shape();
    let (aws, compatible) = (objects(), compatible());
    // AWS states the length of every response it serves.
    assert_eq!(
        aws.accept_get_head(get, head(200, &[])),
        Err(ResponseFault::Head.into())
    );
    assert!(matches!(
        compatible.accept_get_head(get, head(200, &[])),
        Ok(GetHeadOutcome::Body {
            body: BodyWindow {
                expected_len: None,
                ..
            },
            ..
        })
    ));
    // AWS answers a removal with 204 and nothing else.
    assert_eq!(
        aws.accept_delete_head(delete, head(200, &[])),
        Err(ResponseFault::Status.into())
    );
    assert_eq!(
        compatible.accept_delete_head(delete, head(200, &[])),
        Ok(DeleteHeadOutcome::Accepted)
    );
}
