// The rehydration of an Azure blob out of the `Archive` tier: a Set Blob
// Tier that names an online tier and `x-ms-rehydrate-priority`. Azure
// answers that it started, and rehydrates the blob for good, which takes
// hours. A HEAD reports the progress in `x-ms-archive-status`.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::azure::{Blobs, body_kind, named, revision_parameter, validate_key};
use crate::common::{encoded, failure, finish_with_body, missing, valid_header, validate_revision};
use crate::request::HeadWriter;
use crate::url::QueryValue;
use crate::{
    Failure, HeaderSpan, InvalidPlan, Method, Payload, PhysicalRestore, RequestedRange,
    ResponseFault, ResponseHead, RestoreHeadOutcome, RestorePriority, Result, Timestamps,
    WireRequest,
};

impl<'a> Blobs<'a> {
    /// Writes the request head of a Set Blob Tier into `buf` that rehydrates
    /// an archived object into `plan.tier`, at `plan.priority`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key or a revision that
    /// [`Self::encode_get`] refuses, [`InvalidPlan::ContentProperty`] for a
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
    ) -> Result<WireRequest<'r>> {
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
            Some(("comp", QueryValue::Literal("tier"))),
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
        match head.status {
            202 => Ok(RestoreHeadOutcome::Started),
            200 => Ok(RestoreHeadOutcome::Readable),
            201..=299 => Err(ResponseFault::Status.into()),
            404 if head.error_code.is_some() => Ok(missing(&head, named(&head))),
            status if head.error_code.is_none() => Ok(RestoreHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(RestoreHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`RestoreHeadOutcome::NeedErrorBody`] with the response
    /// body.
    pub fn accept_restore_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> RestoreHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }
}
