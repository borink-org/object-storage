//! The outcome, as a C program reads it.
//!
//! A twin carries the fields of the core crate's own value, and borrows the
//! same bytes rather than copying them.

#![forbid(unsafe_code)]

use crate::types::*;

pub(crate) fn stage_outcome(outcome: &proto::StageHeadOutcome<'_>) -> Outcome {
    match *outcome {
        proto::StageHeadOutcome::Staged { e_tag } => Outcome {
            meta: ObjectMeta {
                e_tag: maybe_bytes(e_tag),
                ..Default::default()
            },
            ..only(OutcomeKind::Staged)
        },
        proto::StageHeadOutcome::NotFound { kind } => not_found(kind),
        proto::StageHeadOutcome::NeedErrorBody(failure) => {
            failed(OutcomeKind::NeedErrorBody, &failure)
        }
        proto::StageHeadOutcome::ServiceFailure(failure) => {
            failed(OutcomeKind::ServiceFailure, &failure)
        }
        _ => only(OutcomeKind::Unsupported),
    }
}

pub(crate) fn commit_outcome(outcome: &proto::CommitHeadOutcome<'_>) -> Outcome {
    match *outcome {
        proto::CommitHeadOutcome::Committed { meta } => Outcome {
            meta: meta_view(&meta),
            ..only(OutcomeKind::Committed)
        },
        proto::CommitHeadOutcome::PreconditionFailed => only(OutcomeKind::PreconditionFailed),
        proto::CommitHeadOutcome::NotFound { kind } => not_found(kind),
        proto::CommitHeadOutcome::NeedErrorBody(failure) => {
            failed(OutcomeKind::NeedErrorBody, &failure)
        }
        proto::CommitHeadOutcome::ServiceFailure(failure) => {
            failed(OutcomeKind::ServiceFailure, &failure)
        }
        _ => only(OutcomeKind::Unsupported),
    }
}

pub(crate) fn list_parts_outcome(outcome: &proto::ListPartsHeadOutcome<'_>) -> Outcome {
    match *outcome {
        proto::ListPartsHeadOutcome::Parts {
            meta, expected_len, ..
        } => Outcome {
            meta: meta_view(&meta),
            body: BodyWindow {
                expected_len: maybe_number(expected_len),
                ..Default::default()
            },
            ..only(OutcomeKind::Parts)
        },
        proto::ListPartsHeadOutcome::NotFound { kind } => not_found(kind),
        proto::ListPartsHeadOutcome::NeedErrorBody(failure) => {
            failed(OutcomeKind::NeedErrorBody, &failure)
        }
        proto::ListPartsHeadOutcome::ServiceFailure(failure) => {
            failed(OutcomeKind::ServiceFailure, &failure)
        }
        _ => only(OutcomeKind::Unsupported),
    }
}

impl From<proto::azure::Block<'_>> for Block {
    fn from(part: proto::azure::Block<'_>) -> Self {
        Self {
            id: bytes(part.id.as_bytes()),
            size: part.size,
            state: part.state as u16,
        }
    }
}

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{
    DeleteHeadOutcome, Error, GetHeadOutcome, ListHeadOutcome, PutHeadOutcome, ServiceErrorKind,
};

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

pub(crate) fn list_outcome(outcome: &ListHeadOutcome<'_>) -> Outcome {
    match *outcome {
        // A page is a document rather than part of an object, so the length is
        // the only field of the window that a listing fills.
        ListHeadOutcome::Page { expected_len } => Outcome {
            body: BodyWindow {
                expected_len: maybe_number(expected_len),
                ..Default::default()
            },
            ..only(OutcomeKind::Page)
        },
        ListHeadOutcome::NotFound { kind } => not_found(kind),
        ListHeadOutcome::NeedErrorBody(failure) => failed(OutcomeKind::NeedErrorBody, &failure),
        ListHeadOutcome::ServiceFailure(failure) => failed(OutcomeKind::ServiceFailure, &failure),
        _ => only(OutcomeKind::Unsupported),
    }
}

// One entry, pointing at the bytes of the body that the fill decoded.
impl From<proto::ListEntry<'_>> for ListEntry {
    fn from(entry: proto::ListEntry<'_>) -> Self {
        entry_view(&entry)
    }
}

pub(crate) fn entry_view(entry: &proto::ListEntry<'_>) -> ListEntry {
    ListEntry {
        kind: entry.kind as u16,
        key: bytes(entry.key.as_bytes()),
        size: maybe_number(entry.size),
        e_tag: maybe_bytes(entry.e_tag.map(str::as_bytes)),
        last_modified: maybe_bytes(entry.last_modified.map(str::as_bytes)),
        content_type: maybe_bytes(entry.content_type.map(str::as_bytes)),
        raw: bytes(entry.raw),
    }
}

// A walk, as the two values that a C program keeps between calls.
pub(crate) fn properties_view(walk: proto::Properties<'_>) -> Properties {
    Properties {
        remaining: bytes(walk.remaining()),
        within: walk.within(),
    }
}

// One value that a walk read, or the end of the walk.
pub(crate) fn property_view(found: Option<(&[u8], &[u8])>) -> Property {
    found.map_or_else(Default::default, |(name, value)| Property {
        present: true,
        name: bytes(name),
        value: bytes(value),
    })
}

// A fill that read the page to its end.
pub(crate) fn page_fill(filled: usize, next_marker: Option<&[u8]>) -> Fill {
    Fill {
        filled,
        next_marker: maybe_bytes(next_marker),
        ..Default::default()
    }
}

// A fill that read nothing, because the body is not a page, the array is too
// small for it, or the call was refused. No entry of the array is reported,
// whatever the call wrote there.
pub(crate) fn refused_fill(error: &Error) -> Fill {
    Fill {
        status: status_of(error),
        required: error.capacity().map_or(0, |capacity| capacity.required),
        ..Default::default()
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

fn meta_view(meta: &proto::ObjectMeta<'_>) -> ObjectMeta {
    ObjectMeta {
        size: maybe_number(meta.size),
        e_tag: maybe_bytes(meta.e_tag),
        last_modified: maybe_bytes(meta.last_modified),
        version: maybe_bytes(meta.version),
        content_encoding: maybe_bytes(meta.content_encoding),
        content_type: maybe_bytes(meta.content_type),
    }
}

fn body_view(body: &proto::BodyWindow) -> BodyWindow {
    BodyWindow {
        object_offset: body.object_offset,
        expected_len: maybe_number(body.expected_len),
        object_size: maybe_number(body.object_size),
    }
}

pub(crate) fn status_of(error: &Error) -> Status {
    Status {
        code: error.code() as u16,
        detail: error.detail(),
    }
}

pub(crate) fn kind_view(kind: Option<ServiceErrorKind>) -> u16 {
    kind.map_or(0, |kind| kind as u16)
}

pub(crate) fn maybe_bytes(value: Option<&[u8]>) -> MaybeBytes {
    value.map_or_else(Default::default, |value| MaybeBytes {
        present: true,
        bytes: bytes(value),
    })
}

fn bytes(value: &[u8]) -> Bytes {
    Bytes {
        ptr: value.as_ptr(),
        len: value.len(),
    }
}

pub(crate) fn maybe_number(value: Option<u64>) -> MaybeU64 {
    value.map_or_else(Default::default, |value| MaybeU64 {
        present: true,
        value,
    })
}

pub(crate) fn block_outcome(
    outcome: Outcome,
    head: proto::azure::BlockResponseHead<'_>,
) -> AzureBlockOutcome {
    AzureBlockOutcome {
        outcome,
        content_md5: maybe_bytes(head.content_md5),
        content_crc64: maybe_bytes(head.content_crc64),
        server_encrypted: maybe_bytes(head.server_encrypted),
        encryption_key_sha256: maybe_bytes(head.encryption_key_sha256),
        encryption_scope: maybe_bytes(head.encryption_scope),
        client_request_id: maybe_bytes(head.client_request_id),
        date: maybe_bytes(head.date),
        blob_content_length: maybe_bytes(head.blob_content_length),
        error_code: maybe_bytes(head.common.error_code),
        request_id: maybe_bytes(head.common.request_id),
        version: maybe_bytes(head.common.version),
        e_tag: maybe_bytes(head.common.e_tag),
        last_modified: maybe_bytes(head.common.last_modified),
    }
}
