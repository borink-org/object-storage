//! The plan, as the core crate reads it.
//!
//! A number that names no variant is refused rather than read as another one.

#![forbid(unsafe_code)]

use crate::types::*;

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{Error, InvalidPlan};

// A number that names no value of the core crate's enum. It is refused as an
// invalid plan, and the plan is never read as the value that happens to be
// oldest.
pub(crate) const UNKNOWN: Error = Error::InvalidPlan(InvalidPlan::Unknown);

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

pub(crate) fn list_shape(shape: &ListShape) -> proto::Result<proto::ListShape> {
    Ok(proto::ListShape {
        delimited: shape.delimited,
        max_results: number(shape.max_results),
    })
}

// The position that a fill reported, as the core crate reads it. A position
// built from other numbers names no entry, which the core crate refuses when
// it reads the body.
pub(crate) fn resume(resume: &Resume) -> proto::Resume {
    proto::Resume::from_parts(
        resume.at,
        resume.within,
        resume.marker.present.then_some(proto::Span {
            start: resume.marker.span.start,
            len: resume.marker.span.len,
        }),
    )
}

fn number(value: MaybeU32) -> Option<u32> {
    value.present.then_some(value.value)
}

pub(crate) fn condition_kind(condition: u16) -> proto::Result<proto::ConditionKind> {
    proto::ConditionKind::from_discriminant(condition).ok_or(UNKNOWN)
}
