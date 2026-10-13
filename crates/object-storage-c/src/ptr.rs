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

/// Reads `len` items at `ptr` as a slice that a call writes into.
///
/// # Safety
///
/// `ptr` must be valid for `len` reads and writes of `T`, aligned, and reached
/// through no other reference for the lifetime `'a`. Any `ptr` is accepted
/// when `len` is 0.
pub(crate) unsafe fn items_mut<'a, T>(ptr: *mut T, len: usize) -> &'a mut [T] {
    if len == 0 {
        return &mut [];
    }
    // SAFETY: the caller states that `ptr` addresses `len` items and that
    // nothing else reaches them.
    unsafe { core::slice::from_raw_parts_mut(ptr, len) }
}

/// Reads a writable buffer as a slice.
///
/// # Safety
///
/// `buf.ptr` must be valid for `buf.len` reads and writes, aligned, and
/// reached through no other reference for the lifetime `'a`. Any `ptr` is
/// accepted when `len` is 0.
pub(crate) unsafe fn slice_mut<'a>(buf: BytesMut) -> &'a mut [u8] {
    // SAFETY: the caller states the contract of `items_mut`.
    unsafe { items_mut(buf.ptr, buf.len) }
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

/// The values of a container, as the protocol crate's container takes them.
#[derive(Clone, Copy)]
pub(crate) struct ContainerValues<'a> {
    pub(crate) endpoint: &'a [u8],
    pub(crate) name: &'a [u8],
    pub(crate) allow_http: bool,
    pub(crate) object: &'a [u8],
}

/// Reads the endpoint, the name, whether plain HTTP is allowed, and the one
/// object. A null `container` reads nothing.
///
/// # Safety
///
/// `container` must be null or point at one readable value whose three byte
/// values each satisfy `items` for the lifetime `'a`.
pub(crate) unsafe fn container<'a>(container: *const Container) -> Option<ContainerValues<'a>> {
    // SAFETY: the caller states that a non-null `container` is readable, and
    // that so are the values it holds.
    unsafe {
        container.as_ref().map(|container| ContainerValues {
            endpoint: slice(container.endpoint),
            name: slice(container.name),
            allow_http: container.allow_http,
            object: slice(container.object),
        })
    }
}

/// Reads the five content properties and the access tier of `options`, in
/// the order of their fields.
///
/// # Safety
///
/// Each of the six values must satisfy `items` for the lifetime `'a`.
pub(crate) unsafe fn write_options<'a>(options: &WriteOptions) -> [&'a [u8]; 6] {
    let properties = options.properties;
    // SAFETY: the caller states that each value addresses its stated bytes.
    unsafe {
        [
            slice(properties.content_type),
            slice(properties.content_encoding),
            slice(properties.content_language),
            slice(properties.content_disposition),
            slice(properties.cache_control),
            slice(options.storage_class),
        ]
    }
}

/// Reads the ID and the source of each of `count` blocks.
///
/// # Safety
///
/// `blocks` must satisfy `items` for `count` values, and the `id` of each
/// must satisfy it too, for the lifetime `'a`.
pub(crate) unsafe fn blocks<'a>(
    blocks: *const BlockRef,
    count: usize,
) -> impl Iterator<Item = (&'a [u8], u16)> + Clone {
    // SAFETY: the caller states that `blocks` addresses `count` values.
    let blocks = unsafe { items(blocks, count) };
    // SAFETY: the caller states that the ID of each addresses its bytes.
    blocks
        .iter()
        .map(|block| unsafe { (slice(block.id), block.source) })
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

/// Reads a failure as the protocol crate's record. One whose category this
/// crate does not define reads nothing.
///
/// # Safety
///
/// `failure.request_id` must satisfy `maybe_slice` for the lifetime `'a`.
pub(crate) unsafe fn failure<'a>(
    failure: &Failure,
) -> Option<borink_object_storage_proto::Failure<'a>> {
    // SAFETY: the caller states that the identifier it borrows is readable.
    crate::sentence::failure_of(failure, unsafe { maybe_slice(failure.request_id) })
}
