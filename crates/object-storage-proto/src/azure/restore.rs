// The rehydration of an Azure blob out of the `Archive` tier: a Set Blob
// Tier that names an online tier and `x-ms-rehydrate-priority`. Azure
// answers that it started, and rehydrates the blob for good, which takes
// hours. A HEAD reports the progress in `x-ms-archive-status`.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::azure::template::{Field, Operation, Template, Value};
use crate::azure::{AzureNamespace, Container, ObjectText, encode_object, prepare_object};
use crate::azure::{body_kind, failed};
use crate::common::{finish_with_body, valid_header};
use crate::template::WireRequest;
use crate::{ConditionKind, ObjectItem, RestoreOptions};
use crate::{
    Failure, InvalidPlan, ResponseFault, ResponseHead, RestoreHeadOutcome, RestorePriority, Result,
    Timestamps,
};

/// Writes the template of rehydrations with `options` into `buf`: Set Blob
/// Tiers that move blobs out of the `Archive` tier to the tier
/// [`RestoreOptions::tier`], at [`RestoreOptions::priority`].
///
/// # Errors
///
/// Returns [`InvalidPlan::Option`] for [`RestorePriority::Bulk`], for no
/// tier, for days and for a checksum: a rehydration is for good, and has no
/// body. A tier that is not one header value is
/// [`InvalidPlan::ContentProperty`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_restore(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &RestoreOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    let priority = match options.priority {
        RestorePriority::Standard => "Standard",
        RestorePriority::High => "High",
        RestorePriority::Bulk => return Err(InvalidPlan::Option.into()),
    };
    let Some(tier) = options.tier else {
        return Err(InvalidPlan::Option.into());
    };
    if options.days.is_some() || options.checksum.is_some() {
        return Err(InvalidPlan::Option.into());
    }
    if !valid_header(tier.as_bytes()) || tier.trim_ascii() != tier {
        return Err(InvalidPlan::ContentProperty.into());
    }
    let fields = [
        Field::new("x-ms-access-tier", Value::Bytes(tier.as_bytes())),
        Field::new("x-ms-rehydrate-priority", Value::Bytes(priority.as_bytes())),
        Field::new("content-length", Value::Bytes(b"0")),
    ];
    prepare_object(
        container,
        token,
        namespace,
        Operation::Restore,
        ConditionKind::None,
        options.revision,
        &ObjectText {
            fields: &fields,
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the rehydration of `item` at `now` from `template` into `buf`,
/// and returns it.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), for a revision that is not the
/// template's kind, for a condition value, and for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_restore<'r>(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    encode_object(template, bytes, Operation::Restore, item, now, buf)
}

/// Reads the response head of a rehydration and reports what Azure did:
/// [`RestoreHeadOutcome::Started`] for 202, and
/// [`RestoreHeadOutcome::Readable`] for a blob that is online already.
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
