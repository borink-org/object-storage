//! What the entry points do between reading their pointers and calling
//! the protocol crate.
//!
//! Every value here is a Rust value already: [`crate::ptr`] read the pointers.
//! Each function answers for one stage that several entry points share, and a
//! refusal at any stage is one of the protocol crate's own errors.

#![forbid(unsafe_code)]

use crate::{
    inputs::UNKNOWN,
    outcome::{entry_view, maybe_bytes, page_fill, status_of},
    types::*,
};

use crate::ptr::ContainerValues;
use borink_object_storage_proto as proto;
use borink_object_storage_proto::azure::Container;
use borink_object_storage_proto::{
    Error, InvalidRequest, ListMarker, Listing, ResponseHead, WireRequest as Request, azure,
};

// The protocol crate's container, which every template is written from. A
// value that is not text cannot be the thing it names, so it fails as that
// thing, not as a fault of its own.
pub(crate) fn container_from_values(
    values: Option<ContainerValues<'_>>,
) -> proto::Result<Container<'_>> {
    let values = values.ok_or(UNKNOWN)?;
    let endpoint = text_or(values.endpoint, Error::InvalidEndpoint)?;
    let name = text_or(values.name, Error::InvalidContainer)?;
    let container = if values.allow_http {
        Container::new_allowing_http(endpoint, name)?
    } else {
        Container::new(endpoint, name)?
    };
    Ok(match absent_if_empty(values.object) {
        Some(key) => container.object(text_or(key, InvalidRequest::KeyNotUtf8)?),
        None => container,
    })
}

// The bearer token, which a template holds as text.
pub(crate) fn token_text(token: &[u8]) -> proto::Result<&str> {
    text_or(token, Error::InvalidToken)
}

// The instant that the caller stored, read back.
pub(crate) fn timestamps(now: Option<&Timestamps>) -> proto::Result<proto::Timestamps> {
    proto::Timestamps::from_bytes(&now.ok_or(UNKNOWN)?.stored).ok_or(UNKNOWN)
}

// The kind of account, from its number.
pub(crate) fn namespace(number: u16) -> proto::Result<proto::azure::AzureNamespace> {
    match number {
        0 => Ok(proto::azure::AzureNamespace::Unknown),
        1 => Ok(proto::azure::AzureNamespace::Flat),
        2 => Ok(proto::azure::AzureNamespace::Hierarchical),
        _ => Err(UNKNOWN),
    }
}

// The template that the caller stored, read back.
pub(crate) fn stored_template(template: Option<&Template>) -> proto::Result<azure::Template> {
    azure::Template::from_bytes(&template.ok_or(UNKNOWN)?.stored)
}

// The template that a prepare call wrote, stored for the caller, or why it
// was refused. A capacity error carries the exact size, and no other error
// has one.
pub(crate) fn prepared_template(template: proto::Result<azure::Template>) -> Prepared {
    match template {
        Ok(template) => Prepared {
            required: template.bytes_len(),
            template: Template {
                stored: template.to_bytes(),
            },
            ..Prepared::default()
        },
        Err(error) => Prepared {
            status: status_of(&error),
            required: error.capacity().map_or(0, |capacity| capacity.required),
            ..Prepared::default()
        },
    }
}

// The written request as offsets into its buffer, or the exact size that it
// needed, or why it was refused. All three are one status and one
// `required`.
pub(crate) fn written_request(request: proto::Result<Request<'_>>) -> WireRequest {
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            return WireRequest {
                status: status_of(&error),
                required: error.capacity().map_or(0, |capacity| capacity.required),
                ..WireRequest::default()
            };
        }
    };
    let (url, head, fields) = (
        span(request.url_span()),
        span(request.head_span()),
        span(request.fields_span()),
    );
    let body = request.body_span().map(span);
    WireRequest {
        status: Status::default(),
        required: [Some(url), Some(head), body]
            .into_iter()
            .flatten()
            .map(|span| span.start + span.len)
            .max()
            .unwrap_or(0),
        method: request.method() as u16,
        url,
        head,
        fields,
        body: body.map_or_else(MaybeSpan::default, |span| MaybeSpan {
            present: true,
            span,
        }),
        payload_len: request.payload().len(),
    }
}

fn span(span: proto::Span) -> Span {
    Span {
        start: span.start,
        len: span.len,
    }
}

// The next field line of `fields`, and the rest after it, as
// `WireRequest::headers` reads them: a name, `: `, a value and a CRLF.
pub(crate) fn next_header(fields: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let end = fields.windows(2).position(|pair| pair == b"\r\n")?;
    let (line, rest) = fields.split_at(end);
    let colon = line.windows(2).position(|pair| pair == b": ")?;
    Some((
        &line[..colon],
        line.get(colon + 2..).unwrap_or_default(),
        rest.get(2..).unwrap_or_default(),
    ))
}

// The bytes as text, or `error` if they are not.
pub(crate) fn text_or(bytes: &[u8], error: impl Into<Error>) -> proto::Result<&str> {
    core::str::from_utf8(bytes).map_err(|_| error.into())
}

// An empty value is an absent one. A request without a condition carries no
// entity tag, and the first page of a listing carries no marker.
pub(crate) fn absent_if_empty(value: &[u8]) -> Option<&[u8]> {
    (!value.is_empty()).then_some(value)
}

// What a fill read, with the marker of the page after it.
fn filled(page: Listing<'_>) -> Fill {
    page_fill(
        page.filled,
        page.next_marker
            .and_then(ListMarker::text)
            .map(str::as_bytes),
    )
}

// Reads a page into the caller's array.
//
// The protocol crate writes the caller's entries itself: a C entry is built from
// a core entry as each is read, so no array stands in between and the page is
// read in one call.
pub(crate) fn fill_entries(body: &mut [u8], into: &mut [ListEntry]) -> proto::Result<Fill> {
    azure::fill_listing(body, into).map(filled)
}

// The same read, writing each entry's values into its row of `values`. The
// rows are `wanted.len()` wide and there is one per slot of `into`.
pub(crate) fn filling_with(
    body: &mut [u8],
    into: &mut [ListEntry],
    wanted: proto::azure::PropertySet,
    values: &mut [MaybeBytes],
) -> proto::Result<Fill> {
    let width = wanted.len();
    let mut rows = values.chunks_exact_mut(width.max(1));
    azure::fill_listing_with(body, into, wanted, |entry, given| {
        if let Some(row) = rows.next().filter(|_| width > 0) {
            for (slot, value) in row.iter_mut().zip(given.all()) {
                *slot = maybe_bytes(*value);
            }
        }
        entry_view(&entry)
    })
    .map(filled)
}

// The head, read where your HTTP library already put it. A name that is not
// text is skipped: the protocol crate looks for its headers by text, so such a
// name is none of them.
pub(crate) fn head_of<'a>(
    status: u16,
    headers: impl Iterator<Item = (&'a [u8], &'a [u8])>,
) -> ResponseHead<'a> {
    let named = headers.filter_map(|(name, value)| Some((core::str::from_utf8(name).ok()?, value)));
    ResponseHead::from_headers(status, named)
}

// A failure that the caller passed back, as the protocol crate's record. One
// whose category this crate does not define is refused.
pub(crate) fn proto_failure(
    failure: Option<proto::Failure<'_>>,
) -> proto::Result<proto::Failure<'_>> {
    failure.ok_or(UNKNOWN)
}
