// The rehydration of an Azure blob out of the `Archive` tier: a Set Blob
// Tier that names an online tier and `x-ms-rehydrate-priority`. Azure
// answers that it started, and rehydrates the blob for good, which takes
// hours. A HEAD reports the progress in `x-ms-archive-status`.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::azure::template::{Field, Operation, Template, Value};
use crate::azure::{AzureNamespace, Container, ObjectRequest, encode_object, prepare_object};
use crate::azure::{Blobs, body_kind, failed, revision_parameter, validate_key};
use crate::common::{encoded, finish_with_body, valid_header, validate_revision};
use crate::request::HeadWriter;
use crate::template::WireRequest;
use crate::url;
use crate::{
    Failure, HeaderSpan, InvalidPlan, LegacyRequest, Method, Payload, PhysicalRestore,
    RequestedRange, ResponseFault, ResponseHead, RestoreHeadOutcome, RestorePriority, Result,
    Timestamps,
};
use crate::{ObjectItem, RestoreOptions};

impl<'a> Blobs<'a> {
    /// Writes the request head of a Set Blob Tier into `buf` that rehydrates
    /// an archived object into `plan.tier`, at `plan.priority`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key or a revision that
    /// [`encode_get`](crate::azure::encode_get) refuses, [`InvalidPlan::ContentProperty`] for a
    /// tier that is not one header value, and [`InvalidPlan::Option`] for a
    /// plan without a tier, or with a number of days, a checksum or
    /// [`RestorePriority::Bulk`].
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::restore_requirements`](crate::layered::restore_requirements)
    /// first.
    pub fn encode_restore<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalRestore<'_>,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
        validate_key(plan.key, self.namespace)?;
        validate_revision(plan.revision, true)?;
        let priority = match plan.priority {
            RestorePriority::Standard => "Standard",
            RestorePriority::High => "High",
            RestorePriority::Bulk => return Err(InvalidPlan::Option.into()),
        };
        // A rehydration is for good, and the request has no body.
        let Some(tier) = plan.tier else {
            return Err(InvalidPlan::Option.into());
        };
        if plan.days.is_some() || plan.checksum.is_some() {
            return Err(InvalidPlan::Option.into());
        }
        if !valid_header(tier.as_bytes()) || tier.trim_ascii() != tier {
            return Err(InvalidPlan::ContentProperty.into());
        }
        let query = [
            url::literal("comp", "tier"),
            revision_parameter(plan.revision),
        ];
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &query,
            RequestedRange::Whole,
            now,
        )?;
        head.header("x-ms-access-tier", tier.as_bytes());
        head.header("x-ms-rehydrate-priority", priority.as_bytes());
        head.header("content-length", b"0");
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of a rehydration and reports what Azure did.
    ///
    /// Azure answers 202 when it starts to rehydrate an archived object,
    /// which is [`RestoreHeadOutcome::Started`], and 200 when it moved an
    /// object that was not archived at once, which is
    /// [`RestoreHeadOutcome::Readable`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. Any other
    /// success status is [`ResponseFault::Status`].
    pub fn accept_restore_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<RestoreHeadOutcome<'h>> {
        accept_restore_head(head)
    }

    /// Finishes a [`RestoreHeadOutcome::NeedErrorBody`] with the response
    /// body.
    pub fn accept_restore_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> RestoreHeadOutcome<'h> {
        accept_restore_error_body(failure, body)
    }
}

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
    let mut request = ObjectRequest {
        comp: Some("tier"),
        revision: options.revision,
        ..ObjectRequest::new(Operation::Restore, Method::Put)
    };
    request.fields.push(Field::new(
        "x-ms-access-tier",
        Value::Bytes(tier.as_bytes()),
    ));
    request.fields.push(Field::new(
        "x-ms-rehydrate-priority",
        Value::Bytes(priority.as_bytes()),
    ));
    request
        .fields
        .push(Field::new("content-length", Value::Bytes(b"0")));
    prepare_object(container, token, namespace, request, buf)
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
