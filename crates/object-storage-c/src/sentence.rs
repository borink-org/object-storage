//! Writing what an outcome says into the caller's buffer.
//!
//! Writing keeps counting after the buffer is full, so a caller that wants the
//! whole sentence learns its length and calls again.

use core::fmt::{self, Write as _};

use crate::{convert::*, types::*};

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{Error, GetHeadOutcome};

// ---------------------------------------------------------------- sentences

pub(crate) fn outcome_kind_of(value: u16) -> Option<OutcomeKind> {
    Some(match value {
        1 => OutcomeKind::Body,
        2 => OutcomeKind::Complete,
        3 => OutcomeKind::NotModified,
        4 => OutcomeKind::PreconditionFailed,
        5 => OutcomeKind::NotFound,
        6 => OutcomeKind::RangeNotSatisfiable,
        7 => OutcomeKind::Done,
        8 => OutcomeKind::Accepted,
        9 => OutcomeKind::NeedErrorBody,
        10 => OutcomeKind::ServiceFailure,
        11 => OutcomeKind::Invalid,
        12 => OutcomeKind::Unsupported,
        _ => return None,
    })
}

// # Safety
//
// Every borrowed field of `outcome` must still address its stated bytes.
pub(crate) unsafe fn describe(outcome: &Outcome, into: &mut [u8]) -> usize {
    match outcome_kind_of(outcome.kind) {
        Some(OutcomeKind::Invalid) => describe_status(outcome.error, into),
        // The core crate wrote the sentence for a failure and for an
        // unsatisfiable range, and both carry numbers that no table holds.
        // The twin carries every field of them, so the sentence is borrowed.
        Some(OutcomeKind::NeedErrorBody | OutcomeKind::ServiceFailure) => {
            // SAFETY: the caller states that the identifier is readable.
            match unsafe { failure_of(&outcome.failure) } {
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
        Some(OutcomeKind::NotFound) => match kind_of(outcome.failure.kind) {
            Some(kind) => say(into, &kind),
            None => say(into, &"the object or its container does not exist"),
        },
        // One literal per remaining kind. They say less than the core
        // crate's own sentences, which name the operation: one outcome type
        // crosses for all three operations, so the sentence names none.
        settled => say(into, &settled_sentence(settled)),
    }
}

pub(crate) fn settled_sentence(kind: Option<OutcomeKind>) -> &'static str {
    match kind {
        Some(OutcomeKind::Body) => "the object follows in the response body",
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
// sentence. Writing keeps counting after the buffer is full, so a caller that
// wants all of it learns the size and calls again.
pub(crate) fn say(into: &mut [u8], reason: &dyn fmt::Display) -> usize {
    let mut sentence = Sentence { into, used: 0 };
    // Display for these types never fails, and a full buffer is not a failure
    // here: the count is the answer.
    let _ = write!(sentence, "{reason}");
    sentence.used
}

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
