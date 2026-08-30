//! The functions a C program calls.
//!
//! Each one has the same shape: refuse what cannot be read, convert inwards,
//! call the core crate, convert outwards. The steps they share are in
//! [`crate::step`], and the conversions are in [`crate::convert`].

use crate::{convert::*, ptr::*, sentence::*, step::*, types::*};

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{PhysicalDelete, PhysicalGet, PhysicalPut, Timestamps};

// ------------------------------------------------------------- entry points

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
    match unsafe { usable(session) } {
        Ok(_) => Status { code: 0, detail: 0 },
        Err(status) => status,
    }
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
    let planned = unsafe { planning(session, shape, get_shape, key) };
    let (blobs, shape, key) = match planned {
        Ok(planned) => planned,
        Err(status) => return refused(status, 0),
    };
    let now = Timestamps::from_unix(unix_seconds);
    // SAFETY: as above.
    let get = PhysicalGet::from_shape(shape, key, unsafe { condition(condition_value) });
    // SAFETY: as above.
    written(blobs.encode_get(unsafe { parts_mut(buf) }, &get, &now))
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
    let planned = unsafe { planning(session, shape, put_shape, key) };
    let (blobs, shape, key) = match planned {
        Ok(planned) => planned,
        Err(status) => return refused(status, 0),
    };
    let now = Timestamps::from_unix(unix_seconds);
    // SAFETY: as above.
    let put = PhysicalPut::from_shape(shape, key, unsafe { condition(condition_value) });
    // The content stays in your program. Only its length reaches the head, so
    // the request borrows no content and you send the bytes yourself.
    let content = proto::Payload::Streamed { len: content_len };
    // SAFETY: as above.
    written(blobs.encode_put(unsafe { parts_mut(buf) }, &put, content, &now))
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
    let planned = unsafe { planning(session, shape, delete_shape, key) };
    let (blobs, shape, key) = match planned {
        Ok(planned) => planned,
        Err(status) => return refused(status, 0),
    };
    let now = Timestamps::from_unix(unix_seconds);
    // SAFETY: as above.
    let delete = PhysicalDelete::from_shape(shape, key, unsafe { condition(condition_value) });
    // SAFETY: as above.
    written(blobs.encode_delete(unsafe { parts_mut(buf) }, &delete, &now))
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
    match unsafe { reading(session, shape, get_shape, status, headers, header_count) } {
        Ok((blobs, shape, head)) => match blobs.accept_get_head(shape, head) {
            Ok(outcome) => get_outcome(&outcome),
            Err(error) => invalid(status_of(&error)),
        },
        Err(status) => invalid(status),
    }
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
    match unsafe { reading(session, shape, put_shape, status, headers, header_count) } {
        Ok((blobs, shape, head)) => match blobs.accept_put_head(shape, head) {
            Ok(outcome) => put_outcome(&outcome),
            Err(error) => invalid(status_of(&error)),
        },
        Err(status) => invalid(status),
    }
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
    match unsafe { reading(session, shape, delete_shape, status, headers, header_count) } {
        Ok((blobs, shape, head)) => match blobs.accept_delete_head(shape, head) {
            Ok(outcome) => delete_outcome(&outcome),
            Err(error) => invalid(status_of(&error)),
        },
        Err(status) => invalid(status),
    }
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
    match unsafe { finishing(session, failure) } {
        // SAFETY: as above.
        Ok((blobs, status, id)) => {
            get_outcome(&blobs.accept_error_body(status, id, unsafe { slice(body) }))
        }
        Err(status) => invalid(status),
    }
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
    match unsafe { finishing(session, failure) } {
        // SAFETY: as above.
        Ok((blobs, status, id)) => {
            put_outcome(&blobs.accept_put_error_body(status, id, unsafe { slice(body) }))
        }
        Err(status) => invalid(status),
    }
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
    match unsafe { finishing(session, failure) } {
        // SAFETY: as above.
        Ok((blobs, status, id)) => {
            delete_outcome(&blobs.accept_delete_error_body(status, id, unsafe { slice(body) }))
        }
        Err(status) => invalid(status),
    }
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
    if outcome.is_null() {
        return 0;
    }
    // SAFETY: the caller states the contract of this function.
    unsafe { describe(&*outcome, parts_mut(into)) }
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
    describe_status(status, unsafe { parts_mut(into) })
}
