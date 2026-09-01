//! Writing what an outcome says into the caller's buffer.
//!
//! Writing keeps counting after the buffer is full, so a caller that wants the
//! whole sentence learns its length and calls again.

#![forbid(unsafe_code)]

use core::fmt::{self, Write as _};

use crate::types::*;

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{Error, GetHeadOutcome, ServiceErrorKind};

// `request_id` is the bytes that `outcome.failure.request_id` addresses.
pub(crate) fn describe(outcome: &Outcome, request_id: Option<&[u8]>, into: &mut [u8]) -> usize {
    match outcome_kind_of(outcome.kind) {
        Some(OutcomeKind::Invalid) => describe_status(outcome.error, into),
        // The core crate wrote the sentence for a failure and for an
        // unsatisfiable range, and both carry numbers that no table holds.
        // The twin carries every field of them, so the sentence is borrowed.
        Some(OutcomeKind::NeedErrorBody | OutcomeKind::ServiceFailure) => {
            match failure_of(&outcome.failure, request_id) {
                Some(failure) => say(into, &failure),
                None => say(
                    into,
                    &"the service failed in a way that this crate cannot name",
                ),
            }
        }
        Some(OutcomeKind::RangeNotSatisfiable) => say(
            into,
            &GetHeadOutcome::RangeNotSatisfiable {
                object_size: number(outcome.body.object_size),
            },
        ),
        // A missing object names an error and carries nothing else, so the
        // error is the whole sentence.
        Some(OutcomeKind::NotFound) => {
            match ServiceErrorKind::from_discriminant(outcome.failure.kind) {
                Some(kind) => say(into, &kind),
                None => say(into, &"the object or its container does not exist"),
            }
        }
        // One literal per remaining kind. They say less than the core
        // crate's own sentences, which name the operation: one outcome type
        // crosses for all three operations, so the sentence names none.
        settled => say(into, &settled_sentence(settled)),
    }
}

pub(crate) fn settled_sentence(kind: Option<OutcomeKind>) -> &'static str {
    match kind {
        Some(OutcomeKind::Body) => "the object follows in the response body",
        Some(OutcomeKind::Page) => "the page follows in the response body",
        Some(OutcomeKind::Complete) => "the response carries no body and is complete",
        Some(OutcomeKind::NotModified) => "the object is not modified",
        Some(OutcomeKind::PreconditionFailed) => "the condition did not hold",
        Some(OutcomeKind::Done) => "the service stored the object",
        Some(OutcomeKind::Accepted) => "the service accepted the removal",
        _ => "the core crate returned an outcome that this crate does not know",
    }
}

pub(crate) fn describe_status(status: Status, into: &mut [u8]) -> usize {
    let Some(code) = proto::ErrorCode::from_discriminant(status.code) else {
        return say(into, &"nothing failed");
    };
    match Error::from_parts(code, status.detail) {
        Some(error) => say(into, &error),
        // A capacity error carries sizes rather than a discriminant, and a
        // detail from a later version names nothing here.
        None => say(into, &code.as_str()),
    }
}

// Writes what `reason` says into `into`, and returns the length of the whole
// sentence.
fn say(into: &mut [u8], reason: &dyn fmt::Display) -> usize {
    let mut sentence = Sentence { into, used: 0 };
    // Display for these types never fails, and a full buffer is not a failure
    // here: the count is the answer.
    let _ = write!(sentence, "{reason}");
    sentence.used
}

// Keeps counting after the buffer is full, so a caller that wants all of the
// sentence learns the size and calls again.
struct Sentence<'a> {
    into: &'a mut [u8],
    used: usize,
}

impl fmt::Write for Sentence<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.used + text.len();
        if end <= self.into.len() {
            self.into[self.used..end].copy_from_slice(text.as_bytes());
        }
        self.used = end;
        Ok(())
    }
}

pub(crate) fn outcome_kind_of(value: u16) -> Option<OutcomeKind> {
    use OutcomeKind as K;
    [
        K::Body,
        K::Complete,
        K::NotModified,
        K::PreconditionFailed,
        K::NotFound,
        K::RangeNotSatisfiable,
        K::Done,
        K::Accepted,
        K::NeedErrorBody,
        K::ServiceFailure,
        K::Invalid,
        K::Unsupported,
        K::Page,
    ]
    .into_iter()
    .find(|kind| *kind as u16 == value)
}

// The failure that the twin carries, as the core crate's own record, so that
// the sentence for it is the core crate's own too. `request_id` is the bytes
// that the twin's `request_id` addresses. It is `None` only for a category
// that a later core crate defined and this crate cannot name.
pub(crate) fn failure_of<'a>(
    failure: &Failure,
    request_id: Option<&'a [u8]>,
) -> Option<proto::Failure<'a>> {
    Some(proto::Failure {
        status: failure.status,
        class: proto::FailureClass::from_discriminant(failure.class)?,
        kind: ServiceErrorKind::from_discriminant(failure.kind),
        request_id,
    })
}

pub(crate) fn number(value: MaybeU64) -> Option<u64> {
    value.present.then_some(value.value)
}
