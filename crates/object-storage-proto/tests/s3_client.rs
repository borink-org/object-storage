//! S3 client construction, service rules and request sizing.
//!
//! The signatures themselves are checked against known answers in
//! `crates/object-storage-crypto/tests/sigv4.rs`, which has SHA-256
//! implementations. The provider here computes nothing.

use borink_object_storage_proto::s3::{
    self, Bucket, MAX_METADATA_LEN, Objects, PayloadSigning, Service,
};
use borink_object_storage_proto::sigv4::{
    Credentials, Sha256Provider, Sha256State, wipe_best_effort,
};
use borink_object_storage_proto::{
    BodyWindow, ConditionKind, ConditionValue, DeleteHeadOutcome, DeleteKind, DeleteOptions, Error,
    GetHeadOutcome, InvalidPlan, MetadataPair, Payload, PutItem, PutOptions, ResponseFault,
    ResponseHead, Timestamps,
};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "session-token";

const ZEROS: Sha256Provider = Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32]);

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
    let delete = DeleteOptions {
        condition: ConditionKind::IfNoneMatch,
        ..DeleteOptions::new()
    };
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
    let content = Payload::Slice(b"0");
    let etag = PutItem {
        condition_value: Some(ConditionValue::ETag(b"\"etag\"")),
        ..PutItem::new("k", content)
    };
    let create = PutOptions {
        condition: ConditionKind::IfNoneMatch,
        ..PutOptions::default()
    };
    let put = |service, options: &PutOptions<'_>, item: &PutItem<'_>| {
        let bucket = Bucket::new("https://s3.example.com", "bucket", "auto", service).unwrap();
        let mut bytes = vec![0; 16384];
        let template = s3::prepare_put(
            &bucket,
            &credentials(),
            options,
            PayloadSigning::Unsigned,
            &mut bytes,
        )?;
        s3::encode_put(&template, &bytes, item, None, &now, &mut vec![0; 16384]).map(drop)
    };
    let remove = |service, options: &DeleteOptions| {
        let bucket = Bucket::new("https://s3.example.com", "bucket", "auto", service).unwrap();
        s3::prepare_delete(&bucket, &credentials(), options, &mut [0; 1024]).map(drop)
    };
    let with = |metadata| PutOptions {
        metadata,
        ..PutOptions::default()
    };
    let plain = PutItem::new("k", content);

    assert_eq!(
        put(Service::Aws, &create, &etag),
        Err(InvalidPlan::Condition.into())
    );
    assert_eq!(
        remove(Service::Aws, &delete),
        Err(InvalidPlan::Condition.into())
    );
    assert_eq!(put(Service::Aws, &with(&fits), &plain), Ok(()));
    assert_eq!(
        put(Service::Aws, &with(&over), &plain),
        Err(InvalidPlan::MetadataTooLarge.into())
    );

    assert_eq!(put(Service::Compatible, &create, &etag), Ok(()));
    assert_eq!(remove(Service::Compatible, &delete), Ok(()));
    // S3 keeps no snapshots, whatever the service.
    let snapshots = DeleteOptions {
        kind: DeleteKind::ObjectAndSnapshots,
        ..DeleteOptions::new()
    };
    assert_eq!(
        remove(Service::Compatible, &snapshots),
        Err(InvalidPlan::Option.into())
    );
    assert_eq!(put(Service::Compatible, &with(&over), &plain), Ok(()));
}

#[test]
fn an_aws_client_accepts_only_what_aws_answers() {
    let get = borink_object_storage_proto::GetItem::new("k")
        .shape(&borink_object_storage_proto::GetOptions::new());
    let delete = DeleteOptions::new().shape();
    // AWS states the length of every response it serves.
    assert_eq!(
        s3::accept_get_head(Service::Aws, get, head(200, &[])),
        Err(ResponseFault::Head.into())
    );
    assert!(matches!(
        s3::accept_get_head(Service::Compatible, get, head(200, &[])),
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
        s3::accept_delete_head(Service::Aws, delete, head(200, &[])),
        Err(ResponseFault::Status.into())
    );
    assert_eq!(
        s3::accept_delete_head(Service::Compatible, delete, head(200, &[])),
        Ok(DeleteHeadOutcome::Accepted)
    );
}
