//! Turning the raw pointers a caller passes into Rust values.
//!
//! Four functions, each the one place a pointer shape is read. [`crate::step`]
//! and [`crate::entry`] call these rather than writing the reads again.

use crate::types::*;

// ------------------------------------------------------------------ pointers

/// Reads `len` items at `ptr` as a slice.
///
/// # Safety
///
/// `ptr` must be valid for `len` reads of `T`, aligned, and unwritten for the
/// lifetime `'a`. Any `ptr` is accepted when `len` is 0.
pub(crate) unsafe fn parts<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        return &[];
    }
    // SAFETY: the caller states that `ptr` addresses `len` items.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

/// Reads a writable buffer as a slice.
///
/// # Safety
///
/// `buf.ptr` must be valid for `buf.len` reads and writes, aligned, and
/// reached through no other reference for the lifetime `'a`. Any `ptr` is
/// accepted when `len` is 0.
pub(crate) unsafe fn parts_mut<'a>(buf: BytesMut) -> &'a mut [u8] {
    if buf.len == 0 {
        return &mut [];
    }
    // SAFETY: the caller states that `buf.ptr` addresses `buf.len` bytes and
    // that nothing else reaches them.
    unsafe { core::slice::from_raw_parts_mut(buf.ptr, buf.len) }
}

/// Reads borrowed bytes as a slice.
///
/// # Safety
///
/// As `parts`.
pub(crate) unsafe fn slice<'a>(bytes: Bytes) -> &'a [u8] {
    // SAFETY: the caller states the contract of `parts`.
    unsafe { parts(bytes.ptr, bytes.len) }
}

/// Reads a value the head may not have carried.
///
/// # Safety
///
/// As `parts`, when `value.present`.
pub(crate) unsafe fn maybe_slice<'a>(value: MaybeBytes) -> Option<&'a [u8]> {
    // SAFETY: the caller states the contract of `parts`.
    value.present.then(|| unsafe { slice(value.bytes) })
}
