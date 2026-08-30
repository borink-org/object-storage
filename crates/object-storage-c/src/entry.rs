//! The functions a C program calls.
//!
//! Each one reads every pointer it was passed in one `unsafe` block, under
//! the `# Safety` contract written above it, and hands Rust values to
//! [`crate::step`] and [`crate::convert`]. Those modules forbid `unsafe`
//! code, so what a pointer read needs is checked here and nowhere else.

use crate::convert::{
    delete_outcome, delete_shape, get_outcome, get_shape, invalid, put_outcome, put_shape,
    status_of,
};
use crate::ptr;
use crate::sentence::{describe, describe_status};
use crate::step::{condition, finishing, head_of, open, ready, text, written};
use crate::types::*;

use borink_object_storage_proto::{
    InvalidPlan, Payload, PhysicalDelete, PhysicalGet, PhysicalPut, Timestamps,
};

/// Reports what is wrong with `session`, if anything.
///
/// A `code` of 0 means that the session can build requests. Every other call
/// makes the same check, so this exists to fail early.
///
/// # Safety
///
/// `session` must be null or point at one readable `borink_session` whose
/// three values are each readable for their length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_validate(session: *const Session) -> Status {
    // SAFETY: the caller states the contract of this function.
    let session = unsafe { ptr::session(session) };
    open(session).map_or_else(|error| status_of(&error), |_| Status::default())
}

/// Writes the request head of a read into `buf`.
///
/// Pass an empty `condition_value` if `shape` carries no condition. Pass an
/// empty `buf` to learn the size that this request needs.
///
/// # Safety
///
/// `session` and `shape` must each be null or point at one readable value.
/// `key`, `condition_value` and `buf` must each address their stated length,
/// and `buf` must be reached through nothing else during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_get(
    session: *const Session,
    shape: *const GetShape,
    key: Bytes,
    condition_value: Bytes,
    buf: BytesMut,
    unix_seconds: u64,
) -> RequestHead {
    // SAFETY: the caller states the contract of this function.
    let (session, shape, key, condition_value, buf) = unsafe {
        (
            ptr::session(session),
            shape.as_ref(),
            ptr::slice(key),
            ptr::slice(condition_value),
            ptr::slice_mut(buf),
        )
    };
    written(ready(session, shape, get_shape).and_then(|(blobs, shape)| {
        let get = PhysicalGet::from_shape(
            shape,
            text(key, InvalidPlan::Key)?,
            condition(condition_value),
        );
        blobs.encode_get(buf, &get, &Timestamps::from_unix(unix_seconds))
    }))
}

/// Writes the request head of a write into `buf`.
///
/// The head states `content_len`. You send those bytes yourself.
///
/// # Safety
///
/// As `borink_encode_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_put(
    session: *const Session,
    shape: *const PutShape,
    key: Bytes,
    condition_value: Bytes,
    buf: BytesMut,
    content_len: u64,
    unix_seconds: u64,
) -> RequestHead {
    // SAFETY: the caller states the contract of this function.
    let (session, shape, key, condition_value, buf) = unsafe {
        (
            ptr::session(session),
            shape.as_ref(),
            ptr::slice(key),
            ptr::slice(condition_value),
            ptr::slice_mut(buf),
        )
    };
    written(ready(session, shape, put_shape).and_then(|(blobs, shape)| {
        let put = PhysicalPut::from_shape(
            shape,
            text(key, InvalidPlan::Key)?,
            condition(condition_value),
        );
        // The content stays in your program. Only its length reaches the
        // head, so the request borrows no content and you send the bytes
        // yourself.
        let content = Payload::Streamed { len: content_len };
        blobs.encode_put(buf, &put, content, &Timestamps::from_unix(unix_seconds))
    }))
}

/// Writes the request head of a removal into `buf`.
///
/// # Safety
///
/// As `borink_encode_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_delete(
    session: *const Session,
    shape: *const DeleteShape,
    key: Bytes,
    condition_value: Bytes,
    buf: BytesMut,
    unix_seconds: u64,
) -> RequestHead {
    // SAFETY: the caller states the contract of this function.
    let (session, shape, key, condition_value, buf) = unsafe {
        (
            ptr::session(session),
            shape.as_ref(),
            ptr::slice(key),
            ptr::slice(condition_value),
            ptr::slice_mut(buf),
        )
    };
    written(
        ready(session, shape, delete_shape).and_then(|(blobs, shape)| {
            let delete = PhysicalDelete::from_shape(
                shape,
                text(key, InvalidPlan::Key)?,
                condition(condition_value),
            );
            blobs.encode_delete(buf, &delete, &Timestamps::from_unix(unix_seconds))
        }),
    )
}

/// Reads the response head of a read.
///
/// Pass the same `shape` that you passed to `borink_encode_get`, and one
/// `borink_header_ref` per response header. The outcome points into the same
/// bytes as those headers.
///
/// # Safety
///
/// `session` and `shape` must each be null or point at one readable value.
/// `headers` must address `header_count` readable values.
///
/// # Lifetime
///
/// The bytes that `headers` points at must stay valid, and must not move, for
/// as long as you use the returned outcome.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_get_head(
    session: *const Session,
    shape: *const GetShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (session, shape, headers) = unsafe {
        (
            ptr::session(session),
            shape.as_ref(),
            ptr::headers(headers, header_count),
        )
    };
    ready(session, shape, get_shape)
        .and_then(|(blobs, shape)| blobs.accept_get_head(shape, head_of(status, headers)))
        .map_or_else(invalid, |outcome| get_outcome(&outcome))
}

/// Reads the response head of a write.
///
/// # Safety
///
/// As `borink_accept_get_head`.
///
/// # Lifetime
///
/// As `borink_accept_get_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_put_head(
    session: *const Session,
    shape: *const PutShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (session, shape, headers) = unsafe {
        (
            ptr::session(session),
            shape.as_ref(),
            ptr::headers(headers, header_count),
        )
    };
    ready(session, shape, put_shape)
        .and_then(|(blobs, shape)| blobs.accept_put_head(shape, head_of(status, headers)))
        .map_or_else(invalid, |outcome| put_outcome(&outcome))
}

/// Reads the response head of a removal.
///
/// # Safety
///
/// As `borink_accept_get_head`.
///
/// # Lifetime
///
/// As `borink_accept_get_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_delete_head(
    session: *const Session,
    shape: *const DeleteShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (session, shape, headers) = unsafe {
        (
            ptr::session(session),
            shape.as_ref(),
            ptr::headers(headers, header_count),
        )
    };
    ready(session, shape, delete_shape)
        .and_then(|(blobs, shape)| blobs.accept_delete_head(shape, head_of(status, headers)))
        .map_or_else(invalid, |outcome| delete_outcome(&outcome))
}

/// Finishes a read whose head asked for the error body.
///
/// Pass the `failure` of that outcome and the body that you read. Pass an
/// empty body if you read none: the outcome is then final with the error
/// unnamed.
///
/// # Safety
///
/// `session` and `failure` must each be null or point at one readable value.
/// `body` must address its stated length.
///
/// # Lifetime
///
/// `failure->request_id` must still point at valid bytes, and they must stay
/// valid for as long as you use the returned outcome. So must `body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_finish_get_error_body(
    session: *const Session,
    failure: *const Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (session, failure, body) = unsafe {
        (
            ptr::session(session),
            ptr::failure(failure),
            ptr::slice(body),
        )
    };
    finishing(session, failure)
        .map(|(blobs, status, id)| blobs.accept_error_body(status, id, body))
        .map_or_else(invalid, |outcome| get_outcome(&outcome))
}

/// Finishes a write whose head asked for the error body.
///
/// # Safety
///
/// As `borink_finish_get_error_body`.
///
/// # Lifetime
///
/// As `borink_finish_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_finish_put_error_body(
    session: *const Session,
    failure: *const Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (session, failure, body) = unsafe {
        (
            ptr::session(session),
            ptr::failure(failure),
            ptr::slice(body),
        )
    };
    finishing(session, failure)
        .map(|(blobs, status, id)| blobs.accept_put_error_body(status, id, body))
        .map_or_else(invalid, |outcome| put_outcome(&outcome))
}

/// Finishes a removal whose head asked for the error body.
///
/// # Safety
///
/// As `borink_finish_get_error_body`.
///
/// # Lifetime
///
/// As `borink_finish_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_finish_delete_error_body(
    session: *const Session,
    failure: *const Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (session, failure, body) = unsafe {
        (
            ptr::session(session),
            ptr::failure(failure),
            ptr::slice(body),
        )
    };
    finishing(session, failure)
        .map(|(blobs, status, id)| blobs.accept_delete_error_body(status, id, body))
        .map_or_else(invalid, |outcome| delete_outcome(&outcome))
}

/// Writes one sentence naming what `outcome` says.
///
/// Returns the length of the whole sentence, which may be longer than `into`.
/// The part that fits is written. A null `outcome` writes nothing and returns
/// 0.
///
/// # Safety
///
/// `outcome` must be null or point at one readable value whose borrowed fields
/// still address valid bytes. `into` must address its stated length and be
/// reached through nothing else during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_describe(outcome: *const Outcome, into: BytesMut) -> usize {
    // SAFETY: the caller states the contract of this function.
    let Some(outcome) = (unsafe { outcome.as_ref() }) else {
        return 0;
    };
    // SAFETY: as above, for the identifier the outcome borrows and for `into`.
    let (request_id, into) = unsafe {
        (
            ptr::maybe_slice(outcome.failure.request_id),
            ptr::slice_mut(into),
        )
    };
    describe(outcome, request_id, into)
}

/// Writes one sentence naming what `status` says.
///
/// Returns the length of the whole sentence, exactly as `borink_describe`.
///
/// # Safety
///
/// `into` must address its stated length and be reached through nothing else
/// during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_describe_status(status: Status, into: BytesMut) -> usize {
    // SAFETY: the caller states the contract of this function.
    describe_status(status, unsafe { ptr::slice_mut(into) })
}
