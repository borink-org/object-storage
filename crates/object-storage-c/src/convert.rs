//! Every value that crosses, in both directions.
//!
//! A twin carries the fields of the core crate's own value, and borrows the
//! same bytes rather than copying them. Going inwards, a number that names no
//! variant is refused rather than read as another one.

#![forbid(unsafe_code)]

use crate::types::*;

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{
    DeleteHeadOutcome, Error, GetHeadOutcome, InvalidPlan, PutHeadOutcome, ServiceErrorKind,
};

// A number that names no value of the core crate's enum. It is refused as an
// invalid plan, and the plan is never read as the value that happens to be
// oldest.
pub(crate) const UNKNOWN: Error = Error::InvalidPlan(InvalidPlan::Unknown);

// ----------------------------------------------------------- inwards: plans

pub(crate) fn get_shape(shape: &GetShape) -> proto::Result<proto::GetShape> {
    Ok(proto::GetShape {
        kind: proto::GetKind::from_discriminant(shape.kind).ok_or(UNKNOWN)?,
        range: proto::RequestedRange::from_parts(
            proto::RangeForm::from_discriminant(shape.range.form).ok_or(UNKNOWN)?,
            shape.range.start,
            shape.range.end,
        ),
        condition: condition_kind(shape.condition)?,
    })
}

pub(crate) fn put_shape(shape: &PutShape) -> proto::Result<proto::PutShape> {
    Ok(proto::PutShape {
        condition: condition_kind(shape.condition)?,
    })
}

pub(crate) fn delete_shape(shape: &DeleteShape) -> proto::Result<proto::DeleteShape> {
    Ok(proto::DeleteShape {
        kind: proto::DeleteKind::from_discriminant(shape.kind).ok_or(UNKNOWN)?,
        condition: condition_kind(shape.condition)?,
    })
}

pub(crate) fn condition_kind(condition: u16) -> proto::Result<proto::ConditionKind> {
    proto::ConditionKind::from_discriminant(condition).ok_or(UNKNOWN)
}

// ------------------------------------------------------- outwards: outcomes

pub(crate) fn get_outcome(outcome: &GetHeadOutcome<'_>) -> Outcome {
    match *outcome {
        GetHeadOutcome::Body { meta, body } => Outcome {
            meta: meta_view(&meta),
            body: body_view(&body),
            ..only(OutcomeKind::Body)
        },
        GetHeadOutcome::Complete { meta } => Outcome {
            meta: meta_view(&meta),
            ..only(OutcomeKind::Complete)
        },
        GetHeadOutcome::NotModified { e_tag } => Outcome {
            meta: ObjectMeta {
                e_tag: maybe_bytes(e_tag),
                ..Default::default()
            },
            ..only(OutcomeKind::NotModified)
        },
        GetHeadOutcome::PreconditionFailed => only(OutcomeKind::PreconditionFailed),
        GetHeadOutcome::NotFound { kind } => not_found(kind),
        GetHeadOutcome::RangeNotSatisfiable { object_size } => Outcome {
            body: BodyWindow {
                object_size: maybe_number(object_size),
                ..Default::default()
            },
            ..only(OutcomeKind::RangeNotSatisfiable)
        },
        GetHeadOutcome::NeedErrorBody(failure) => failed(OutcomeKind::NeedErrorBody, &failure),
        GetHeadOutcome::ServiceFailure(failure) => failed(OutcomeKind::ServiceFailure, &failure),
        // The outcome is sealed, so a later version can add a variant. Report
        // one that this crate does not know rather than guessing at it.
        _ => only(OutcomeKind::Unsupported),
    }
}

pub(crate) fn put_outcome(outcome: &PutHeadOutcome<'_>) -> Outcome {
    match *outcome {
        PutHeadOutcome::Created { meta } => Outcome {
            meta: meta_view(&meta),
            ..only(OutcomeKind::Done)
        },
        PutHeadOutcome::PreconditionFailed => only(OutcomeKind::PreconditionFailed),
        PutHeadOutcome::NotFound { kind } => not_found(kind),
        PutHeadOutcome::NeedErrorBody(failure) => failed(OutcomeKind::NeedErrorBody, &failure),
        PutHeadOutcome::ServiceFailure(failure) => failed(OutcomeKind::ServiceFailure, &failure),
        _ => only(OutcomeKind::Unsupported),
    }
}

pub(crate) fn delete_outcome(outcome: &DeleteHeadOutcome<'_>) -> Outcome {
    match *outcome {
        // A removal returns no object, so Azure sends no metadata for one.
        DeleteHeadOutcome::Accepted => only(OutcomeKind::Accepted),
        DeleteHeadOutcome::PreconditionFailed => only(OutcomeKind::PreconditionFailed),
        DeleteHeadOutcome::NotFound { kind } => not_found(kind),
        DeleteHeadOutcome::NeedErrorBody(failure) => failed(OutcomeKind::NeedErrorBody, &failure),
        DeleteHeadOutcome::ServiceFailure(failure) => failed(OutcomeKind::ServiceFailure, &failure),
        _ => only(OutcomeKind::Unsupported),
    }
}

pub(crate) fn invalid(error: Error) -> Outcome {
    Outcome {
        error: status_of(&error),
        ..only(OutcomeKind::Invalid)
    }
}

// An outcome that says this and nothing else: every other field is absent.
pub(crate) fn only(kind: OutcomeKind) -> Outcome {
    Outcome {
        kind: kind as u16,
        ..Default::default()
    }
}

fn failed(kind: OutcomeKind, failure: &proto::Failure<'_>) -> Outcome {
    Outcome {
        failure: Failure {
            status: failure.status,
            class: failure.class as u16,
            kind: kind_view(failure.kind),
            request_id: maybe_bytes(failure.request_id),
        },
        ..only(kind)
    }
}

// A missing object is not a failure of the head. The core crate's variant
// carries the error it named and nothing else, and so does this.
fn not_found(kind: Option<ServiceErrorKind>) -> Outcome {
    Outcome {
        failure: Failure {
            kind: kind_view(kind),
            ..Default::default()
        },
        ..only(OutcomeKind::NotFound)
    }
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

fn meta_view(meta: &proto::ObjectMeta<'_>) -> ObjectMeta {
    ObjectMeta {
        size: maybe_number(meta.size),
        e_tag: maybe_bytes(meta.e_tag),
        last_modified: maybe_bytes(meta.last_modified),
        version: maybe_bytes(meta.version),
        content_encoding: maybe_bytes(meta.content_encoding),
    }
}

fn body_view(body: &proto::BodyWindow) -> BodyWindow {
    BodyWindow {
        object_offset: body.object_offset,
        expected_len: maybe_number(body.expected_len),
        object_size: maybe_number(body.object_size),
    }
}

// ------------------------------------------------------- numbers, both ways

pub(crate) fn status_of(error: &Error) -> Status {
    Status {
        code: error.code() as u16,
        detail: error.detail(),
    }
}

pub(crate) fn outcome_kind_of(value: u16) -> Option<OutcomeKind> {
    use OutcomeKind as D;
    [
        D::Body,
        D::Complete,
        D::NotModified,
        D::PreconditionFailed,
        D::NotFound,
        D::RangeNotSatisfiable,
        D::Done,
        D::Accepted,
        D::NeedErrorBody,
        D::ServiceFailure,
        D::Invalid,
        D::Unsupported,
    ]
    .into_iter()
    .find(|kind| *kind as u16 == value)
}

pub(crate) fn kind_view(kind: Option<ServiceErrorKind>) -> u16 {
    kind.map_or(0, |kind| kind as u16)
}

fn maybe_bytes(value: Option<&[u8]>) -> MaybeBytes {
    value.map_or_else(Default::default, |bytes| MaybeBytes {
        present: true,
        bytes: Bytes {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        },
    })
}

fn maybe_number(value: Option<u64>) -> MaybeU64 {
    value.map_or_else(Default::default, |value| MaybeU64 {
        present: true,
        value,
    })
}

pub(crate) fn number(value: MaybeU64) -> Option<u64> {
    value.present.then_some(value.value)
}
