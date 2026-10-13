// S3 RestoreObject: a temporary copy of an object in an archive storage
// class, readable for a number of days. S3 answers that it started, and
// restores in hours. A HEAD reports the progress in `x-amz-restore`.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::common::finish_with_body;
use crate::request::{ByteSink, U64Decimal};
use crate::s3::template::{Operation, Template, Unsigned};
use crate::s3::{Bucket, ObjectText, encode_object, prepare_object};
use crate::s3::{Service, body_kind, failed};
use crate::sigv4::Credentials;
use crate::{
    ConditionKind, Failure, InvalidRequest, ResponseFault, ResponseHead, RestoreHeadOutcome,
    RestorePriority, Result, Timestamps,
};
use crate::{ObjectItem, RestoreOptions};

const RESTORE_OPEN: &[u8] = b"<RestoreRequest xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">";
const RESTORE_CLOSE: &[u8] = b"</RestoreRequest>";

// The body of a RestoreObject. S3 takes the days only for a storage class
// that it restores a copy of, so the element is left out without them.
// The body of a RestoreObject of `days`, at `priority`.
fn write_restore_request(out: &mut dyn ByteSink, days: Option<u32>, priority: RestorePriority) {
    out.push(RESTORE_OPEN);
    if let Some(days) = days {
        out.push(b"<Days>");
        out.push(U64Decimal::new(days.into()).as_bytes());
        out.push(b"</Days>");
    }
    out.push(b"<GlacierJobParameters><Tier>");
    let tier = match priority {
        RestorePriority::Bulk => "Bulk",
        RestorePriority::Standard => "Standard",
        RestorePriority::High => "Expedited",
    };
    out.push(tier.as_bytes());
    out.push(b"</Tier></GlacierJobParameters>");
    out.push(RESTORE_CLOSE);
}

/// Writes the template of RestoreObjects with `options` into `buf`, signed
/// with `credentials`. Each restores a copy of an archived object, readable
/// for [`RestoreOptions::days`].
///
/// The template holds the body. Sign each request with its SHA-256: see
/// [`set_body_hash`](crate::s3::set_body_hash).
///
/// # Errors
///
/// Returns [`InvalidRequest::Option`] for a directory bucket, which archives
/// nothing, for a tier, which S3 does not take, and for zero days;
/// [`InvalidRequest::Revision`] for a snapshot.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_restore(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &RestoreOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    // S3 restores a copy in the object's own storage class, for at least a
    // day.
    if bucket.service == Service::AwsDirectory || options.tier.is_some() || options.days == Some(0)
    {
        return Err(InvalidRequest::Option.into());
    }
    let (days, priority) = (options.days, options.priority);
    let write = move |out: &mut dyn ByteSink| write_restore_request(out, days, priority);
    prepare_object(
        bucket,
        credentials,
        Operation::Restore,
        ConditionKind::None,
        options.revision,
        options.checksum,
        &ObjectText {
            body: Some(&write),
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the RestoreObject of `item` at `now` from `template` into `buf`,
/// with its body after the canonical request, ready to sign.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a key that S3 refuses, see
/// [Object keys](crate#object-keys), for a version that is not the
/// template's kind, for a condition value, and for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_restore(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    encode_object(template, bytes, Operation::Restore, item, now, buf)
}

/// Reads the response head of a RestoreObject and reports what S3 did:
/// [`RestoreHeadOutcome::Started`] for 202, and
/// [`RestoreHeadOutcome::Readable`] for a copy that is restored already.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 and 202 is [`ResponseFault::Status`].
pub fn accept_restore_head<'h>(head: ResponseHead<'h>) -> Result<RestoreHeadOutcome<'h>> {
    match head.status {
        202 => Ok(RestoreHeadOutcome::Started),
        200 => Ok(RestoreHeadOutcome::Readable),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`RestoreHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_restore_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> RestoreHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}
