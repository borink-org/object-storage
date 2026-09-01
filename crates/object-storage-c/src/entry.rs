//! The functions a C program calls.
//!
//! Each one reads every pointer it was passed in one `unsafe` block, under
//! the `# Safety` contract written above it, and hands Rust values to
//! [`crate::step`], [`crate::plan`] and [`crate::outcome`]. Those modules
//! forbid `unsafe` code, so what a pointer read needs is checked here and
//! nowhere else.

use crate::outcome::{
    delete_outcome, get_outcome, invalid, list_outcome, maybe_bytes, maybe_number, properties_view,
    property_view, put_outcome, refused_fill, status_of,
};
use crate::plan::{delete_shape, get_shape, list_shape, put_shape, resume};
use crate::ptr;
use crate::sentence::{describe, describe_status};
use crate::step::{filling, finishing, head_of, open, optional, ready, text, written};
use crate::types::*;

use borink_object_storage_proto::{
    self as proto, InvalidPlan, Payload, PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut,
    Timestamps, layered,
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
            optional(condition_value),
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
            optional(condition_value),
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
                optional(condition_value),
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

/// Writes the request head of one page of a listing into `buf`.
///
/// An empty `prefix` lists the whole container. Pass an empty `marker` for the
/// first page, and the `next_marker` of the last fill for every page after it.
///
/// # Safety
///
/// `session` and `shape` must each be null or point at one readable value.
/// `prefix`, `marker` and `buf` must each address their stated length, and
/// `buf` must be reached through nothing else during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_list(
    session: *const Session,
    shape: *const ListShape,
    prefix: Bytes,
    marker: Bytes,
    buf: BytesMut,
    unix_seconds: u64,
) -> RequestHead {
    // SAFETY: the caller states the contract of this function.
    let (session, shape, prefix, marker, buf) = unsafe {
        (
            ptr::session(session),
            shape.as_ref(),
            ptr::slice(prefix),
            ptr::slice(marker),
            ptr::slice_mut(buf),
        )
    };
    written(
        ready(session, shape, list_shape).and_then(|(blobs, shape)| {
            let list = PhysicalList::from_shape(
                shape,
                text(prefix, InvalidPlan::Prefix)?,
                optional(marker),
            );
            blobs.encode_list(buf, &list, &Timestamps::from_unix(unix_seconds))
        }),
    )
}

/// Reads the response head of a listing.
///
/// This call takes no shape: it checks nothing in the response against the
/// plan.
///
/// # Safety
///
/// `session` must be null or point at one readable value. `headers` must
/// address `header_count` readable values.
///
/// # Lifetime
///
/// The bytes that `headers` points at must stay valid, and must not move, for
/// as long as you use the returned outcome.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_list_head(
    session: *const Session,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (session, headers) =
        unsafe { (ptr::session(session), ptr::headers(headers, header_count)) };
    open(session)
        .and_then(|blobs| blobs.accept_list_head(head_of(status, headers)))
        .map_or_else(invalid, |outcome| list_outcome(&outcome))
}

/// Finishes a listing whose head asked for the error body.
///
/// # Safety
///
/// As `borink_finish_get_error_body`.
///
/// # Lifetime
///
/// As `borink_finish_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_finish_list_error_body(
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
        .map(|(blobs, status, id)| blobs.accept_list_error_body(status, id, body))
        .map_or_else(invalid, |outcome| list_outcome(&outcome))
}

/// Reads a page out of the response body of a listing.
///
/// Pass the whole body that the `Page` outcome announced, and an array of
/// `capacity` entries to write it into. Reading is destructive. This call
/// decodes the text of the body where it stands, so a body that has been read
/// is no longer a document.
///
/// Your array is the budget. A page that does not fit fills the array and
/// reports `Partial`. Read the rest of that page with `borink_resume_listing`
/// and the `resume` it reported. An array of `max_results` entries always
/// holds a whole page.
///
/// # Safety
///
/// `session` must be null or point at one readable value. `body` must address
/// its stated length and be reached through nothing else during the call.
/// `into` must address `capacity` writable entries.
///
/// # Lifetime
///
/// Every entry, and `next_marker`, point into `body`. They are valid until you
/// release or reuse that buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_fill_listing(
    session: *const Session,
    body: BytesMut,
    into: *mut ListEntry,
    capacity: usize,
) -> Fill {
    // SAFETY: the caller states the contract of this function.
    let (session, body, into) = unsafe {
        (
            ptr::session(session),
            ptr::slice_mut(body),
            ptr::items_mut(into, capacity),
        )
    };
    open(session)
        .and_then(|blobs| filling(&blobs, body, None, into))
        .unwrap_or_else(|error| refused_fill(&error))
}

/// Reads the rest of a page that a fill stopped in.
///
/// Pass the same `body`, unchanged, and the `resume` that came with the
/// entries you have finished with.
///
/// # Safety
///
/// As `borink_fill_listing`, and `from` must be null or point at one readable
/// value.
///
/// # Lifetime
///
/// As `borink_fill_listing`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_resume_listing(
    session: *const Session,
    body: BytesMut,
    from: *const Resume,
    into: *mut ListEntry,
    capacity: usize,
) -> Fill {
    // SAFETY: the caller states the contract of this function.
    let (session, body, from, into) = unsafe {
        (
            ptr::session(session),
            ptr::slice_mut(body),
            from.as_ref(),
            ptr::items_mut(into, capacity),
        )
    };
    open(session)
        .and_then(|blobs| {
            let from = from.ok_or(crate::plan::UNKNOWN)?;
            filling(&blobs, body, Some(resume(from)), into)
        })
        .unwrap_or_else(|error| refused_fill(&error))
}

/// Returns the value that one entry gave for a property.
///
/// The name is matched exactly, against the elements of the entry and of its
/// properties element: `AccessTier` and `Creation-Time` on Azure. An absent
/// value means the entry wrote no such property, which is not the same fact as
/// a property it wrote empty.
///
/// Each call reads the entry again. Read more than one or two with
/// `borink_entry_properties`, which reads it once.
///
/// The three values that the entry carries are not read back this way. Reading
/// the page decoded them where they stood, so the element that held one now
/// holds the decoded text and what the decoding left behind. Read `key`,
/// `e_tag` and `last_modified` from the entry.
///
/// # Safety
///
/// `entry` must be null or point at one readable value whose `raw` addresses
/// its stated length. `name` must address its stated length.
///
/// # Lifetime
///
/// The value points into the body that the entry points into.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_entry_property(entry: *const ListEntry, name: Bytes) -> MaybeBytes {
    // SAFETY: the caller states the contract of this function.
    let (raw, name) = unsafe {
        match entry.as_ref() {
            Some(entry) => (ptr::slice(entry.raw), ptr::slice(name)),
            None => return MaybeBytes::default(),
        }
    };
    maybe_bytes(
        proto::Properties::new(raw)
            .find(|(found, _)| *found == name)
            .map(|(_, value)| value),
    )
}

/// Starts a walk over the values that one entry holds.
///
/// Step it with `borink_next_property`, which reports one value per call and
/// reads the entry once over the whole walk. A null `entry` starts a walk that
/// has already ended.
///
/// # Safety
///
/// As `borink_entry_property`.
///
/// # Lifetime
///
/// The walk points into the body that the entry points into.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_entry_properties(entry: *const ListEntry) -> Properties {
    // SAFETY: the caller states the contract of this function.
    let raw = unsafe {
        match entry.as_ref() {
            Some(entry) => ptr::slice(entry.raw),
            None => return Properties::default(),
        }
    };
    properties_view(proto::Properties::new(raw))
}

/// Reads the next value of a walk, and steps the walk past it.
///
/// An element that holds other elements, such as the metadata of an object,
/// reports those bytes as its value. The properties element is stepped into
/// rather than reported. A walk that has ended reports an absent value, and
/// stays ended.
///
/// # Safety
///
/// `walk` must be null or point at one writable `borink_properties` whose
/// `remaining` addresses its stated length.
///
/// # Lifetime
///
/// The value points into the body that the walk points into.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_next_property(walk: *mut Properties) -> Property {
    // SAFETY: the caller states the contract of this function.
    let Some(walk) = (unsafe { walk.as_mut() }) else {
        return Property::default();
    };
    // SAFETY: as above, for the bytes that the walk has not read.
    let mut reading =
        proto::Properties::from_parts(unsafe { ptr::slice(walk.remaining) }, walk.within);
    let found = property_view(reading.next());
    *walk = properties_view(reading);
    found
}

/// Writes the text of a listed value with its references resolved.
///
/// Use this on a value that `borink_entry_property` returned, which holds the
/// bytes that the service wrote. XML writes an `&` as `&amp;`, and a character
/// that the document cannot carry as `&#233;`.
///
/// Copies `value` into `into` and returns what it wrote, which is never longer
/// than `value`. An absent value means that `into` is shorter than `value`, or
/// that `value` holds a reference that no listing declares.
///
/// # Safety
///
/// `value` must address its stated length. `into` must address its stated
/// length and be reached through nothing else during the call.
///
/// # Lifetime
///
/// The bytes returned are `into`, so they are valid until you release or
/// reuse it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_decode_into(value: Bytes, into: BytesMut) -> MaybeBytes {
    // SAFETY: the caller states the contract of this function.
    let (value, into) = unsafe { (ptr::slice(value), ptr::slice_mut(into)) };
    maybe_bytes(layered::decode_into(value, into))
}

/// Writes an entity tag from a listing in the quoted form that HTTP defines.
///
/// A listing writes an entity tag without the quotes that the `ETag` header
/// carries. Pass `borink_list_entry.e_tag` here to get the form that a
/// condition takes. `into` needs at most two bytes more than `listed`, and a
/// shorter one writes nothing and returns an absent value.
///
/// # Safety
///
/// `listed` must address its stated length. `into` must address its stated
/// length and be reached through nothing else during the call.
///
/// # Lifetime
///
/// The bytes returned are `into`, so they are valid until you release or
/// reuse it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_quoted_etag(listed: Bytes, into: BytesMut) -> MaybeBytes {
    // SAFETY: the caller states the contract of this function.
    let (listed, into) = unsafe { (ptr::slice(listed), ptr::slice_mut(into)) };
    maybe_bytes(layered::quoted_etag(listed, into))
}

/// Reads an HTTP date as milliseconds since the Unix epoch.
///
/// Pass `borink_list_entry.last_modified`, or the same value of a
/// `borink_object_meta`. A value that is not an RFC 1123 date is absent.
///
/// # Safety
///
/// `value` must address its stated length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_http_date_ms(value: Bytes) -> MaybeU64 {
    // SAFETY: the caller states the contract of this function.
    let value = unsafe { ptr::slice(value) };
    maybe_number(layered::http_date_ms(value))
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
