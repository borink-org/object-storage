//! Every read of a pointer that a caller passed.
//!
//! Each function turns one C value into the Rust value it addresses, and
//! states what it needs of the pointer. [`crate::entry`] calls these under the
//! `# Safety` contract of each entry point, and [`crate::layout`] calls
//! `items` once. No other module reads a pointer.

use crate::types::*;

/// Reads `len` items at `ptr` as a slice.
///
/// # Safety
///
/// `ptr` must be valid for `len` reads of `T`, aligned, and unwritten for the
/// lifetime `'a`. Any `ptr` is accepted when `len` is 0.
pub(crate) unsafe fn items<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        return &[];
    }
    // SAFETY: the caller states that `ptr` addresses `len` items.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

/// Reads borrowed bytes as a slice.
///
/// # Safety
///
/// As `items`.
pub(crate) unsafe fn slice<'a>(bytes: Bytes) -> &'a [u8] {
    // SAFETY: the caller states the contract of `items`.
    unsafe { items(bytes.ptr, bytes.len) }
}

/// Reads a writable buffer as a slice.
///
/// # Safety
///
/// `buf.ptr` must be valid for `buf.len` reads and writes, aligned, and
/// reached through no other reference for the lifetime `'a`. Any `ptr` is
/// accepted when `len` is 0.
pub(crate) unsafe fn slice_mut<'a>(buf: BytesMut) -> &'a mut [u8] {
    if buf.len == 0 {
        return &mut [];
    }
    // SAFETY: the caller states that `buf.ptr` addresses `buf.len` bytes and
    // that nothing else reaches them.
    unsafe { core::slice::from_raw_parts_mut(buf.ptr, buf.len) }
}

/// Reads a value the head may not have carried.
///
/// # Safety
///
/// As `items`, when `value.present`.
pub(crate) unsafe fn maybe_slice<'a>(value: MaybeBytes) -> Option<&'a [u8]> {
    // SAFETY: the caller states the contract of `items`.
    value.present.then(|| unsafe { slice(value.bytes) })
}

/// Reads the endpoint, the container and the token of a session, in that
/// order. A null `session` reads nothing.
///
/// # Safety
///
/// `session` must be null or point at one readable value whose three values
/// each satisfy `items` for the lifetime `'a`.
pub(crate) unsafe fn session<'a>(session: *const Session) -> Option<[&'a [u8]; 3]> {
    // SAFETY: the caller states that a non-null `session` is readable, and
    // that so are the three values it holds.
    unsafe {
        session.as_ref().map(|session| {
            [
                slice(session.endpoint),
                slice(session.container),
                slice(session.token),
            ]
        })
    }
}

/// Reads the name and the value of each of `count` headers.
///
/// # Safety
///
/// `headers` must satisfy `items` for `count` values, and the `name` and
/// `value` of each must satisfy it too, for the lifetime `'a`.
pub(crate) unsafe fn headers<'a>(
    headers: *const HeaderRef,
    count: usize,
) -> impl Iterator<Item = (&'a [u8], &'a [u8])> {
    // SAFETY: the caller states that `headers` addresses `count` values.
    let headers = unsafe { items(headers, count) };
    // SAFETY: the caller states that each of them addresses its stated bytes.
    headers
        .iter()
        .map(|header| unsafe { (slice(header.name), slice(header.value)) })
}

/// Reads the status and the request identifier of a failure. A null `failure`
/// reads nothing.
///
/// # Safety
///
/// `failure` must be null or point at one readable value whose `request_id`
/// satisfies `maybe_slice` for the lifetime `'a`.
pub(crate) unsafe fn failure<'a>(failure: *const Failure) -> Option<(u16, Option<&'a [u8]>)> {
    // SAFETY: the caller states that a non-null `failure` is readable, and
    // that so is the identifier it borrows.
    unsafe {
        failure
            .as_ref()
            .map(|failure| (failure.status, maybe_slice(failure.request_id)))
    }
}
