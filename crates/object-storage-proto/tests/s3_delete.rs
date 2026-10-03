//! S3 DELETE: the plans the client refuses, and the response heads it reads.

use borink_object_storage_proto::s3::{Bucket, Objects, Service};
use borink_object_storage_proto::sigv4::{
    Credentials, Sha256Provider, Sha256State, wipe_best_effort,
};
use borink_object_storage_proto::{
    Condition, DeleteHeadOutcome, DeleteKind, Error, InvalidPlan, PhysicalDelete, ResponseFault,
    ResponseHead, Timestamps, layered,
};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "session-token";

const ZEROS: Sha256Provider =
    Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32], |_, _| [0; 32]);

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
fn a_removal_that_s3_cannot_take_is_refused() {
    let objects = objects();
    let delete = |delete: PhysicalDelete<'_>| {
        refusal(layered::s3::delete_requirements(&objects, &delete, &now()).map(drop))
    };
    assert_eq!(
        delete(PhysicalDelete {
            kind: DeleteKind::ObjectAndSnapshots,
            ..PhysicalDelete::new("k")
        }),
        InvalidPlan::Option
    );
    assert_eq!(
        delete(PhysicalDelete::new("k").with_condition(Condition::IfNoneMatch(b"*"))),
        InvalidPlan::Condition
    );
}

#[test]
fn a_removal_is_accepted_whether_or_not_the_key_held_an_object() {
    let objects = objects();
    let unconditional = PhysicalDelete::new("k").shape();
    for (objects, status) in [(objects, 204), (compatible(), 200)] {
        assert_eq!(
            objects.accept_delete_head(unconditional, head(status, &[])),
            Ok(DeleteHeadOutcome::Accepted)
        );
    }
    assert_eq!(
        objects.accept_delete_head(unconditional, head(202, &[])),
        Err(ResponseFault::Status.into())
    );
    assert_eq!(
        objects.accept_delete_head(unconditional, head(412, &[])),
        Err(ResponseFault::Status.into())
    );
    let conditional = PhysicalDelete::new("k")
        .with_condition(Condition::IfMatch(b"\"etag\""))
        .shape();
    assert_eq!(
        objects.accept_delete_head(conditional, head(412, &[])),
        Ok(DeleteHeadOutcome::PreconditionFailed)
    );
    assert!(matches!(
        objects.accept_delete_head(conditional, head(403, &[])),
        Ok(DeleteHeadOutcome::NeedErrorBody(_))
    ));
}
