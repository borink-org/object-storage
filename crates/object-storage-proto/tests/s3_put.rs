//! S3 PUT: the plans the client refuses, and the response heads it reads.

use borink_object_storage_proto::checksum::{ChecksumProvider, ChecksumState, Digest};
use borink_object_storage_proto::s3::{Bucket, MAX_PUT_LEN, Objects, PayloadHash, Service};
use borink_object_storage_proto::sigv4::{
    Credentials, Sha256Provider, Sha256State, wipe_best_effort,
};
use borink_object_storage_proto::{
    ChecksumKind, Condition, Error, FailureClass, InvalidPlan, MetadataPair, Payload, PhysicalPut,
    PutHeadOutcome, ResponseFault, ResponseHead, Timestamps, TransactionalChecksum, WriteOptions,
    layered,
};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "session-token";

const ZEROS: Sha256Provider = Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32]);

const MD5: ChecksumProvider = ChecksumProvider::new(
    ChecksumKind::Md5,
    ChecksumState::uninit,
    |_, _| {},
    |_| Digest::md5([0; 16]),
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

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_061)
}

fn refusal(result: Result<(), Error>) -> InvalidPlan {
    match result {
        Err(Error::InvalidPlan(reason)) => reason,
        other => panic!("expected an invalid plan, got {other:?}"),
    }
}

fn head<'h>(status: u16, headers: &[(&'h str, &'h [u8])]) -> ResponseHead<'h> {
    ResponseHead::from_headers(status, headers.iter().copied())
}

#[test]
fn a_write_that_s3_cannot_take_is_refused() {
    let objects = objects();
    let slice = Payload::Slice(b"0123456789");
    let streamed = Payload::Streamed { len: 10 };
    let put = |put: PhysicalPut<'_>, content, hash, objects: &Objects<'_>| {
        refusal(layered::s3::put_requirements(objects, &put, content, hash, &now()).map(drop))
    };
    let checksum = |checksum| PhysicalPut {
        options: WriteOptions {
            checksum: Some(checksum),
            ..WriteOptions::new()
        },
        ..PhysicalPut::new("k")
    };
    let cases = [
        // S3 writes on `If-None-Match` only when no object holds the key.
        (
            PhysicalPut::new("k").with_condition(Condition::IfNoneMatch(b"\"etag\"")),
            slice,
            PayloadHash::Compute,
            InvalidPlan::Condition,
        ),
        // The encoder cannot hash content it does not hold.
        (
            PhysicalPut::new("k"),
            streamed,
            PayloadHash::Compute,
            InvalidPlan::Option,
        ),
        (
            PhysicalPut::new("k"),
            Payload::Streamed {
                len: MAX_PUT_LEN + 1,
            },
            PayloadHash::Unsigned,
            InvalidPlan::PayloadTooLarge,
        ),
        (
            checksum(TransactionalChecksum::Md5("not base64")),
            slice,
            PayloadHash::Compute,
            InvalidPlan::Checksum,
        ),
        // No MD5 provider is registered.
        (
            checksum(TransactionalChecksum::Compute(ChecksumKind::Md5)),
            slice,
            PayloadHash::Compute,
            InvalidPlan::Option,
        ),
        (
            PhysicalPut {
                options: WriteOptions {
                    declared_md5: Some("1B2M2Y8AsgTpgAmY7PhCfg=="),
                    ..WriteOptions::new()
                },
                ..PhysicalPut::new("k")
            },
            slice,
            PayloadHash::Compute,
            InvalidPlan::Option,
        ),
    ];
    for (plan, content, hash, reason) in cases {
        assert_eq!(put(plan, content, hash, &objects), reason, "{plan:?}");
    }

    // With a provider, an MD5 is computed only over content the encoder holds.
    let with_md5 = objects.with_checksum(MD5);
    let md5 = checksum(TransactionalChecksum::Compute(ChecksumKind::Md5));
    assert!(
        layered::s3::put_requirements(&with_md5, &md5, slice, PayloadHash::Compute, &now()).is_ok()
    );
    assert_eq!(
        put(md5, streamed, PayloadHash::Unsigned, &with_md5),
        InvalidPlan::Option
    );

    let metadata = |pairs: &[MetadataPair<'_>]| {
        refusal(
            layered::s3::put_requirements(
                &objects,
                &PhysicalPut {
                    metadata: pairs,
                    ..PhysicalPut::new("k")
                },
                slice,
                PayloadHash::Compute,
                &now(),
            )
            .map(drop),
        )
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
fn a_write_reports_what_s3_stored() {
    let objects = objects();
    let unconditional = PhysicalPut::new("k").shape();
    let created = objects
        .accept_put_head(
            unconditional,
            head(200, &[("etag", b"\"tag\""), ("x-amz-version-id", b"v1")]),
        )
        .unwrap();
    let PutHeadOutcome::Created { meta } = created else {
        panic!("{created:?}");
    };
    assert_eq!(
        (meta.e_tag, meta.version, meta.size),
        (Some(b"\"tag\"".as_slice()), Some(b"v1".as_slice()), None)
    );
    // S3 answers a write with 200, never 201.
    assert_eq!(
        objects.accept_put_head(unconditional, head(201, &[])),
        Err(ResponseFault::Status.into())
    );
    assert_eq!(
        objects.accept_put_head(unconditional, head(412, &[])),
        Err(ResponseFault::Status.into())
    );

    let create = PhysicalPut::new("k")
        .with_condition(Condition::IfNoneMatch(b"*"))
        .shape();
    assert_eq!(
        objects.accept_put_head(create, head(412, &[])),
        Ok(PutHeadOutcome::PreconditionFailed)
    );

    let Ok(PutHeadOutcome::NeedErrorBody(failure)) =
        objects.accept_put_head(create, head(409, &[]))
    else {
        panic!("a conflict names its error in the body");
    };
    let outcome = objects.accept_put_error_body(
        create,
        failure,
        b"<Error><Code>ConditionalRequestConflict</Code></Error>",
    );
    let PutHeadOutcome::ServiceFailure(failure) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        (failure.status, failure.class, failure.kind),
        (409, FailureClass::Other, None)
    );
}
