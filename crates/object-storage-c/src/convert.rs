//! Every value that crosses, in both directions.
//!
//! A twin carries the fields of the core crate's own value, and borrows the
//! same bytes rather than copying them. Going inwards, a number that names no
//! variant is refused rather than read as another one.

use crate::{ptr::*, step::*, types::*};

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{DeleteHeadOutcome, GetHeadOutcome, PutHeadOutcome};

// ---------------------------------------------------------- plans, both ways

pub(crate) fn get_shape(shape: &GetShape) -> Result<proto::GetShape, Status> {
    Ok(proto::GetShape {
        kind: proto::GetKind::from_discriminant(shape.kind).ok_or_else(unknown)?,
        range: proto::RequestedRange::from_parts(
            proto::RangeForm::from_discriminant(shape.range.form).ok_or_else(unknown)?,
            shape.range.start,
            shape.range.end,
        ),
        condition: condition_kind(shape.condition)?,
    })
}

pub(crate) fn put_shape(shape: &PutShape) -> Result<proto::PutShape, Status> {
    Ok(proto::PutShape {
        condition: condition_kind(shape.condition)?,
    })
}

pub(crate) fn delete_shape(shape: &DeleteShape) -> Result<proto::DeleteShape, Status> {
    Ok(proto::DeleteShape {
        kind: proto::DeleteKind::from_discriminant(shape.kind).ok_or_else(unknown)?,
        condition: condition_kind(shape.condition)?,
    })
}

pub(crate) fn condition_kind(condition: u16) -> Result<proto::ConditionKind, Status> {
    proto::ConditionKind::from_discriminant(condition).ok_or_else(unknown)
}

// ------------------------------------------------------- outcomes, both ways

pub(crate) fn class_of(class: u16) -> Option<proto::FailureClass> {
    proto::FailureClass::from_discriminant(class)
}

pub(crate) fn kind_view(kind: Option<proto::ServiceErrorKind>) -> u16 {
    kind.map_or(0, |kind| kind as u16)
}

pub(crate) fn kind_of(kind: u16) -> Option<proto::ServiceErrorKind> {
    proto::ServiceErrorKind::from_discriminant(kind)
}

pub(crate) fn failure_view(failure: &proto::Failure<'_>) -> Failure {
    Failure {
        status: failure.status,
        class: failure.class as u16,
        kind: kind_view(failure.kind),
        request_id: maybe_bytes(failure.request_id),
    }
}

// The failure that the twin carries, as the core crate's own record, so that
// the sentence for it is the core crate's own too. It is `None` only for a
// category that a later core crate defined and this crate cannot name.
//
// # Safety
//
// `failure.request_id` must still address its stated bytes.
pub(crate) unsafe fn failure_of<'a>(failure: &Failure) -> Option<proto::Failure<'a>> {
    Some(proto::Failure {
        status: failure.status,
        class: class_of(failure.class)?,
        kind: kind_of(failure.kind),
        // SAFETY: the caller states that the identifier is readable.
        request_id: unsafe { maybe_slice(failure.request_id) },
    })
}

// A named error and nothing else. A missing object is not a failure of the
// head: the core crate's variant carries a kind alone, and so does this.
pub(crate) fn named_error(kind: Option<proto::ServiceErrorKind>) -> Failure {
    Failure {
        status: 0,
        class: 0,
        kind: kind_view(kind),
        request_id: absent_bytes(),
    }
}

pub(crate) fn get_outcome(outcome: &GetHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(OutcomeKind::Unsupported);
    match *outcome {
        GetHeadOutcome::Body { meta, body } => {
            view.kind = OutcomeKind::Body as u16;
            view.meta = meta_view(&meta);
            view.body = body_view(&body);
        }
        GetHeadOutcome::Complete { meta } => {
            view.kind = OutcomeKind::Complete as u16;
            view.meta = meta_view(&meta);
        }
        GetHeadOutcome::NotModified { e_tag } => {
            view.kind = OutcomeKind::NotModified as u16;
            view.meta.e_tag = maybe_bytes(e_tag);
        }
        GetHeadOutcome::PreconditionFailed => {
            view.kind = OutcomeKind::PreconditionFailed as u16;
        }
        GetHeadOutcome::NotFound { kind } => {
            view.kind = OutcomeKind::NotFound as u16;
            view.failure = named_error(kind);
        }
        GetHeadOutcome::RangeNotSatisfiable { object_size } => {
            view.kind = OutcomeKind::RangeNotSatisfiable as u16;
            view.body.object_size = maybe_number(object_size);
        }
        GetHeadOutcome::NeedErrorBody(failure) => {
            view.kind = OutcomeKind::NeedErrorBody as u16;
            view.failure = failure_view(&failure);
        }
        GetHeadOutcome::ServiceFailure(failure) => {
            view.kind = OutcomeKind::ServiceFailure as u16;
            view.failure = failure_view(&failure);
        }
        // The outcome is sealed, so a later version can add a variant. Report
        // one that this crate does not know rather than guessing at it.
        _ => {}
    }
    view
}

pub(crate) fn put_outcome(outcome: &PutHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(OutcomeKind::Unsupported);
    match *outcome {
        PutHeadOutcome::Created { meta } => {
            view.kind = OutcomeKind::Done as u16;
            view.meta = meta_view(&meta);
        }
        PutHeadOutcome::PreconditionFailed => {
            view.kind = OutcomeKind::PreconditionFailed as u16;
        }
        PutHeadOutcome::NotFound { kind } => {
            view.kind = OutcomeKind::NotFound as u16;
            view.failure = named_error(kind);
        }
        PutHeadOutcome::NeedErrorBody(failure) => {
            view.kind = OutcomeKind::NeedErrorBody as u16;
            view.failure = failure_view(&failure);
        }
        PutHeadOutcome::ServiceFailure(failure) => {
            view.kind = OutcomeKind::ServiceFailure as u16;
            view.failure = failure_view(&failure);
        }
        _ => {}
    }
    view
}

pub(crate) fn delete_outcome(outcome: &DeleteHeadOutcome<'_>) -> Outcome {
    let mut view = empty_outcome(OutcomeKind::Unsupported);
    match *outcome {
        // A removal returns no object, so Azure sends no metadata for one.
        DeleteHeadOutcome::Accepted => view.kind = OutcomeKind::Accepted as u16,
        DeleteHeadOutcome::PreconditionFailed => {
            view.kind = OutcomeKind::PreconditionFailed as u16;
        }
        DeleteHeadOutcome::NotFound { kind } => {
            view.kind = OutcomeKind::NotFound as u16;
            view.failure = named_error(kind);
        }
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            view.kind = OutcomeKind::NeedErrorBody as u16;
            view.failure = failure_view(&failure);
        }
        DeleteHeadOutcome::ServiceFailure(failure) => {
            view.kind = OutcomeKind::ServiceFailure as u16;
            view.failure = failure_view(&failure);
        }
        _ => {}
    }
    view
}

pub(crate) fn invalid(status: Status) -> Outcome {
    let mut view = empty_outcome(OutcomeKind::Invalid);
    view.error = status;
    view
}

pub(crate) fn empty_outcome(kind: OutcomeKind) -> Outcome {
    Outcome {
        kind: kind as u16,
        meta: ObjectMeta {
            size: absent_number(),
            e_tag: absent_bytes(),
            last_modified: absent_bytes(),
            version: absent_bytes(),
            content_encoding: absent_bytes(),
        },
        body: BodyWindow {
            object_offset: 0,
            expected_len: absent_number(),
            object_size: absent_number(),
        },
        failure: named_error(None),
        error: Status { code: 0, detail: 0 },
    }
}

pub(crate) fn meta_view(meta: &proto::ObjectMeta<'_>) -> ObjectMeta {
    ObjectMeta {
        size: maybe_number(meta.size),
        e_tag: maybe_bytes(meta.e_tag),
        last_modified: maybe_bytes(meta.last_modified),
        version: maybe_bytes(meta.version),
        content_encoding: maybe_bytes(meta.content_encoding),
    }
}

pub(crate) fn body_view(body: &proto::BodyWindow) -> BodyWindow {
    BodyWindow {
        object_offset: body.object_offset,
        expected_len: maybe_number(body.expected_len),
        object_size: maybe_number(body.object_size),
    }
}

pub(crate) fn maybe_bytes(value: Option<&[u8]>) -> MaybeBytes {
    match value {
        Some(bytes) => MaybeBytes {
            present: true,
            bytes: Bytes {
                ptr: bytes.as_ptr(),
                len: bytes.len(),
            },
        },
        None => absent_bytes(),
    }
}

pub(crate) fn absent_bytes() -> MaybeBytes {
    MaybeBytes {
        present: false,
        bytes: Bytes {
            ptr: core::ptr::null(),
            len: 0,
        },
    }
}

pub(crate) fn maybe_number(value: Option<u64>) -> MaybeU64 {
    match value {
        Some(value) => MaybeU64 {
            present: true,
            value,
        },
        None => absent_number(),
    }
}

pub(crate) fn number(value: MaybeU64) -> Option<u64> {
    value.present.then_some(value.value)
}

pub(crate) fn absent_number() -> MaybeU64 {
    MaybeU64 {
        present: false,
        value: 0,
    }
}
