//! What the entry points do between reading their pointers and calling the
//! core crate.
//!
//! Every value here is a Rust value already: [`crate::ptr`] read the pointers.
//! Each function answers for one stage that several entry points share, and a
//! refusal at any stage is one of the core crate's own errors.

#![forbid(unsafe_code)]

use crate::{
    outcome::{page_fill, status_of},
    plan::UNKNOWN,
    types::*,
};

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{Blobs, Container, Error, ResponseHead, WireRequest};

// What every call needs before the core crate sees it: a session that was
// passed, whose three values name a container that can be addressed.
pub(crate) fn open(session: Option<[&[u8]; 3]>) -> proto::Result<Blobs<'_>> {
    let [endpoint, container, token] = session.ok_or(UNKNOWN)?;
    // A value that is not text cannot be the thing it names. It fails as that
    // thing, not as a fourth kind of fault.
    let endpoint = text(endpoint, Error::InvalidEndpoint)?;
    let container = text(container, Error::InvalidContainer)?;
    let token = text(token, Error::InvalidToken)?;
    Blobs::new(Container::new(endpoint, container)?, token)
}

// What every call with a shape needs on top of that: the shape was passed,
// and it is one that the core crate can read.
pub(crate) fn ready<'a, V, S>(
    session: Option<[&'a [u8]; 3]>,
    shape: Option<&V>,
    convert: impl FnOnce(&V) -> proto::Result<S>,
) -> proto::Result<(Blobs<'a>, S)> {
    Ok((open(session)?, convert(shape.ok_or(UNKNOWN)?)?))
}

// What every finishing call needs: the failure was passed, and its status and
// request identifier are the values the outcome carried.
pub(crate) fn finishing<'a>(
    session: Option<[&'a [u8]; 3]>,
    failure: Option<(u16, Option<&'a [u8]>)>,
) -> proto::Result<(Blobs<'a>, u16, Option<&'a [u8]>)> {
    let blobs = open(session)?;
    let (status, request_id) = failure.ok_or(UNKNOWN)?;
    Ok((blobs, status, request_id))
}

// The bytes as text, or `error` if they are not.
pub(crate) fn text(bytes: &[u8], error: impl Into<Error>) -> proto::Result<&str> {
    core::str::from_utf8(bytes).map_err(|_| error.into())
}

// An empty value is an absent one. A request without a condition carries no
// entity tag, and the first page of a listing carries no marker.
pub(crate) fn optional(value: &[u8]) -> Option<&[u8]> {
    (!value.is_empty()).then_some(value)
}

// Reads a page into the caller's array.
//
// The core crate writes the caller's entries itself: a C entry is built from
// a core entry as each is read, so no array stands in between and the page is
// read in one call.
pub(crate) fn filling(
    blobs: &Blobs<'_>,
    body: &mut [u8],
    into: &mut [ListEntry],
) -> proto::Result<Fill> {
    let page = blobs.fill_listing(body, into)?;
    Ok(page_fill(page.filled, page.next_marker.map(str::as_bytes)))
}

// The head, read where your HTTP library already put it. A name that is not
// text is skipped: the core crate looks for its headers by text, so such a
// name is none of them.
pub(crate) fn head_of<'a>(
    status: u16,
    headers: impl Iterator<Item = (&'a [u8], &'a [u8])>,
) -> ResponseHead<'a> {
    let named = headers.filter_map(|(name, value)| Some((core::str::from_utf8(name).ok()?, value)));
    ResponseHead::from_headers(status, named)
}

// The written head, or the exact size that it needed, or why the plan was
// refused. All three are one status and one `required`.
pub(crate) fn written(request: proto::Result<WireRequest<'_>>) -> RequestHead {
    let request = match request {
        Ok(request) => request,
        Err(error) => return refused(&error),
    };
    let mut head = RequestHead {
        method: request.method() as u16,
        url: span(request.url_span()),
        header_count: request.header_spans().len(),
        ..Default::default()
    };
    head.required = head.url.start + head.url.len;
    for (slot, (name, value)) in head.headers.iter_mut().zip(request.header_spans()) {
        *slot = RequestHeader {
            name: span(name),
            value: span(value),
        };
        head.required = head.required.max(value.start + value.len);
    }
    head
}

fn refused(error: &Error) -> RequestHead {
    RequestHead {
        status: status_of(error),
        // A capacity error carries the exact size. No other error has one.
        required: error.capacity().map_or(0, |capacity| capacity.required),
        method: Method::Get as u16,
        ..Default::default()
    }
}

fn span(span: proto::Span) -> Span {
    Span {
        start: span.start,
        len: span.len,
    }
}
