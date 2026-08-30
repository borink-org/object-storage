//! What the entry points check before the core crate sees anything.
//!
//! Each function here answers for one stage that several entry points share:
//! a session that names a container, a plan that was passed and is text, a
//! head that can be read, and a finish that reads nothing twice.

use crate::{ptr::*, types::*};

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{
    Blobs, Container, Error, InvalidPlan, ResponseHead, WireRequest,
};

// ----------------------------------------------------------------- the steps

// What every call needs before the core crate sees it: a session whose three
// values name a container that can be addressed.
//
// # Safety
//
// As `borink_validate`.
pub(crate) unsafe fn usable<'a>(session: *const Session) -> Result<Blobs<'a>, Status> {
    if session.is_null() {
        return Err(unknown());
    }
    // SAFETY: the caller states that `session` points at one readable value.
    let session = unsafe { *session };
    // SAFETY: as above, for the three values it holds.
    let (endpoint, container, token) = unsafe {
        (
            slice(session.endpoint),
            slice(session.container),
            slice(session.token),
        )
    };
    // A value that is not text cannot be the thing it names. It fails as that
    // thing, not as a fourth kind of fault.
    let (Ok(endpoint), Ok(container), Ok(token)) = (
        core::str::from_utf8(endpoint),
        core::str::from_utf8(container),
        core::str::from_utf8(token),
    ) else {
        let code = match (
            core::str::from_utf8(endpoint).is_err(),
            core::str::from_utf8(container).is_err(),
        ) {
            (true, _) => ErrorCode::InvalidEndpoint,
            (_, true) => ErrorCode::InvalidContainer,
            _ => ErrorCode::InvalidToken,
        };
        return Err(Status {
            code: code as u16,
            detail: 0,
        });
    };
    Blobs::new(
        Container::new(endpoint, container).map_err(|error| status_of(&error))?,
        token,
    )
    .map_err(|error| status_of(&error))
}

// What every request needs on top of that: a shape that was passed, a key that
// is text, and the plan's shape as the core crate spells it.
//
// # Safety
//
// As `borink_encode_get`.
pub(crate) unsafe fn planning<'a, V, S>(
    session: *const Session,
    shape: *const V,
    convert: impl FnOnce(&V) -> Result<S, Status>,
    key: Bytes,
) -> Result<(Blobs<'a>, S, &'a str), Status> {
    // SAFETY: the caller states the contract of this function.
    let blobs = unsafe { usable(session) }?;
    if shape.is_null() {
        return Err(unknown());
    }
    // SAFETY: as above.
    let Ok(key) = core::str::from_utf8(unsafe { slice(key) }) else {
        return Err(status_of(&Error::InvalidPlan(InvalidPlan::Key)));
    };
    // SAFETY: as above.
    Ok((blobs, convert(unsafe { &*shape })?, key))
}

// What every reading call needs: the same shape the request was planned with,
// and the head where your HTTP library already put it.
//
// # Safety
//
// As `borink_accept_get_head`.
pub(crate) unsafe fn reading<'a, V, S>(
    session: *const Session,
    shape: *const V,
    convert: impl FnOnce(&V) -> Result<S, Status>,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Result<(Blobs<'a>, S, ResponseHead<'a>), Status> {
    // SAFETY: the caller states the contract of this function.
    let blobs = unsafe { usable(session) }?;
    if shape.is_null() {
        return Err(unknown());
    }
    // SAFETY: as above.
    let shape = convert(unsafe { &*shape })?;
    // SAFETY: as above.
    Ok((blobs, shape, unsafe {
        head_of(status, parts(headers, header_count))
    }))
}

// What every finishing call needs. The status and the request identifier are
// the plain values the outcome carried, so nothing is read twice.
//
// # Safety
//
// As `borink_finish_get_error_body`.
pub(crate) unsafe fn finishing<'a>(
    session: *const Session,
    failure: *const Failure,
) -> Result<(Blobs<'a>, u16, Option<&'a [u8]>), Status> {
    // SAFETY: the caller states the contract of this function.
    let blobs = unsafe { usable(session) }?;
    if failure.is_null() {
        return Err(unknown());
    }
    // SAFETY: as above.
    let failure = unsafe { *failure };
    // SAFETY: as above, for the request identifier it borrows.
    Ok((blobs, failure.status, unsafe {
        maybe_slice(failure.request_id)
    }))
}

// The head, read where your HTTP library already put it. A name that is not
// text is skipped: the core crate looks for its headers by text, so such a
// name is none of them.
//
// # Safety
//
// Every `HeaderRef` in `headers` must address its stated bytes.
pub(crate) unsafe fn head_of<'a>(status: u16, headers: &[HeaderRef]) -> ResponseHead<'a> {
    ResponseHead::from_headers(
        status,
        headers.iter().filter_map(|header| {
            // SAFETY: the caller states that both values are readable.
            let (name, value) = unsafe { (slice(header.name), slice(header.value)) };
            Some((core::str::from_utf8(name).ok()?, value))
        }),
    )
}

// The written head, or the exact size that it needed, or why the plan was
// refused. All three are one status and one `required`.
pub(crate) fn written(request: proto::Result<WireRequest<'_>>) -> RequestHead {
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            let required = error.capacity().map_or(0, |capacity| capacity.required);
            return refused(status_of(&error), required);
        }
    };
    let mut headers = empty_headers();
    let mut end = request.url_span().start + request.url_span().len;
    for (slot, (name, value)) in headers.iter_mut().zip(request.header_spans()) {
        slot.name = span(name);
        slot.value = span(value);
        end = end.max(value.start + value.len);
    }
    RequestHead {
        status: Status { code: 0, detail: 0 },
        required: end,
        method: request.method() as u16,
        url: span(request.url_span()),
        header_count: request.header_spans().len(),
        headers,
    }
}

pub(crate) fn refused(status: Status, required: usize) -> RequestHead {
    RequestHead {
        status,
        required,
        method: Method::Get as u16,
        url: Span { start: 0, len: 0 },
        header_count: 0,
        headers: empty_headers(),
    }
}

pub(crate) fn empty_headers() -> [RequestHeader; BORINK_MAX_HEADERS] {
    [RequestHeader {
        name: Span { start: 0, len: 0 },
        value: Span { start: 0, len: 0 },
    }; BORINK_MAX_HEADERS]
}

pub(crate) fn span(span: proto::Span) -> Span {
    Span {
        start: span.start,
        len: span.len,
    }
}

pub(crate) fn status_of(error: &Error) -> Status {
    Status {
        code: error.code() as u16,
        detail: error.detail(),
    }
}

// A number that names no value of the core crate's enum. It is refused as an
// invalid plan, and the plan is never read as the value that happens to be
// oldest.
pub(crate) fn unknown() -> Status {
    status_of(&Error::InvalidPlan(InvalidPlan::Unknown))
}

// # Safety
//
// `value` must address its stated bytes.
pub(crate) unsafe fn condition<'a>(value: Bytes) -> Option<&'a [u8]> {
    // SAFETY: the caller states that `value` is readable.
    let value = unsafe { slice(value) };
    (!value.is_empty()).then_some(value)
}
