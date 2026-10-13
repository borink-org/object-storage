// Azure Blob Batch, of Delete Blob requests: the removal of up to 256 blobs
// in one request. The request body is a `multipart/mixed` body that holds a
// Delete Blob request for each blob, and the answer is one that holds a
// response for each. `multipart.rs` reads and writes the parts, and
// `http_message.rs` reads each response. This file holds what Azure adds:
// the fields of each part, and the numbering that ties a response to its
// request.
//
// Each part of the answer carries `Content-Type: application/http` and the
// `Content-ID` of the request it answers, and no other field. Every request
// is answered once. Anything else is a fault.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::Payload;
use crate::azure::template::{
    Describe, Detail, Field, Holes, Operation, Template, Value, encode as encode_template,
    prepare as prepare_template,
};
use crate::azure::{AzureNamespace, Container};
use crate::azure::{body_kind, failed, revision_parameter, validate_key};
use crate::common::{decimal, decimal_header, finish_with_body, validate_revision};
use crate::http_message::Response;
use crate::multipart::MixedContentType;
use crate::multipart::{self, Part};
use crate::request::HeaderValue;
use crate::request::{ByteSink, U64Decimal, Writer};
use crate::template::{Body, Fill, Hole, ParamValue, Values, WireRequest};
use crate::url;
use crate::url::QueryValue;
use crate::{
    DeleteHeadOutcome, DeleteManyHeadOutcome, DeleteShape, DeleteTarget, Failure, InvalidPlan,
    Method, ResponseFault, ResponseHead, Result, Timestamps,
};

/// The most blobs that one Blob Batch removes.
pub const MAX_BATCH_KEYS: usize = 256;

// The boundary between the parts of the request body. A part holds the head
// of a Delete Blob, whose key is percent-encoded in its path, so no line of
// it can be the boundary.
const BOUNDARY: &str = "batch_borink-object-storage";

/// The result of one removal in a Blob Batch.
///
/// [`fill_delete_results`] writes one for each key of the request, at
/// the key's position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchResult<'b> {
    /// What the removal did, read as the answer to a single Delete Blob is:
    /// [`DeleteHeadOutcome::Accepted`] if Azure removed the blob.
    pub outcome: DeleteHeadOutcome<'b>,
    /// The head of the response to the removal. Pass it with `body` to
    /// [`azure::error_code`](crate::azure::error_code) for the error code that
    /// Azure named.
    pub head: ResponseHead<'b>,
    /// The content of the response to the removal: empty, or the error
    /// document.
    pub body: &'b [u8],
}

impl Default for BatchResult<'_> {
    fn default() -> Self {
        Self {
            outcome: DeleteHeadOutcome::Accepted,
            head: ResponseHead::default(),
            body: &[],
        }
    }
}

// The body of a batch: a part for each key, numbered by its position,
// that holds a Delete Blob request. `path` is the path of the container
// with a `/` after it, and `tail` the fields after `x-ms-date`, which
// `write_part_tail` writes; each writes its text into `out`. The request has no content, so the CRLF of the
// delimiter after it stands for the empty line that ends its head, as in
// the example of Azure's documentation.
fn write_batch(
    out: &mut dyn ByteSink,
    objects: &[DeleteTarget<'_>],
    path: &dyn Fn(&mut dyn ByteSink),
    tail: &dyn Fn(&mut dyn ByteSink),
    now: &Timestamps,
) {
    for (index, object) in objects.iter().enumerate() {
        if index == 0 {
            multipart::write_first_delimiter(out, BOUNDARY);
        } else {
            multipart::write_delimiter(out, BOUNDARY);
        }
        out.push(b"Content-Type: application/http\r\n");
        out.push(b"Content-Transfer-Encoding: binary\r\nContent-ID: ");
        out.push(U64Decimal::new(index as u64).as_bytes());
        out.push(b"\r\n\r\nDELETE ");
        path(out);
        for part in url::encode_object_key(object.key) {
            out.push(part);
        }
        url::write_query_in_url(out, &[revision_parameter(object.revision)]);
        out.push(b" HTTP/1.1\r\nx-ms-date: ");
        out.push(now.rfc1123_bytes());
        tail(out);
    }
    multipart::write_close_delimiter(out, BOUNDARY);
}

// The fields of each part of a batch after `x-ms-date`.
fn write_part_tail(out: &mut dyn ByteSink, token: &str) {
    out.push(b"\r\nAuthorization: Bearer ");
    out.push(token.as_bytes());
    out.push(b"\r\nContent-Length: 0\r\n");
}

// The position of the request that a part answers, which its `Content-ID`
// names. The part carries that field and `Content-Type: application/http`,
// each once, and no other.
fn request_index(part: &Part<'_>, count: usize) -> Result<usize> {
    let mut index = None;
    let mut application_http = false;
    for (name, value) in part.fields() {
        if name.eq_ignore_ascii_case("content-id") && index.is_none() {
            // Azure numbers the requests in decimal from 0, with no leading
            // zero.
            if value.len() > 1 && value[0] == b'0' {
                return Err(ResponseFault::Body.into());
            }
            let value = decimal(value).ok_or(ResponseFault::Body)?;
            index = Some(usize::try_from(value).or(Err(ResponseFault::Body))?);
        } else if name.eq_ignore_ascii_case("content-type")
            && !application_http
            && value.eq_ignore_ascii_case(b"application/http")
        {
            application_http = true;
        } else {
            return Err(ResponseFault::Body.into());
        }
    }
    match index {
        Some(index) if application_http && index < count => Ok(index),
        _ => Err(ResponseFault::Body.into()),
    }
}

// Writes the parts of a batch as the body of its request.
struct BatchFill<'i> {
    values: Values<'i>,
    objects: &'i [DeleteTarget<'i>],
    path: &'i [u8],
    tail: &'i [u8],
    now: &'i Timestamps,
}

impl Fill for BatchFill<'_> {
    const BODY: bool = true;

    fn values(&self) -> &Values<'_> {
        &self.values
    }

    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        if hole != Hole::Body {
            return false;
        }
        let path = |out: &mut dyn ByteSink| out.push(self.path);
        let tail = |out: &mut dyn ByteSink| out.push(self.tail);
        write_batch(out, self.objects, &path, &tail, self.now);
        true
    }
}

/// Writes the template of Blob Batches of Delete Blobs into `buf`.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_delete_many(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    buf: &mut [u8],
) -> Result<Template> {
    // The template holds the text that each part repeats, outside its
    // sections, for each request to read.
    let mut stash = [crate::Span::default(); 2];
    let content_type = MixedContentType(BOUNDARY);
    let mut type_text = [0u8; 80];
    let mut writer = Writer::new(&mut type_text);
    content_type.write_to(&mut writer);
    let type_len = writer.position();
    let fields = [Field::new(
        "content-type",
        Value::Bytes(&type_text[..type_len]),
    )];
    let mut template = prepare_template(
        container,
        token,
        namespace,
        &describe_batch(&fields),
        Operation::DeleteMany,
        Detail::Batch {
            path: crate::Span::default(),
            tail: crate::Span::default(),
        },
        buf,
        |b| {
            stash[0] = b.stash(|out| {
                out.push(b"/");
                out.push(container.name.as_bytes());
                out.push(b"/");
            });
            stash[1] = b.stash(|out| write_part_tail(out, token));
        },
    )?;
    template.detail = Detail::Batch {
        path: stash[0],
        tail: stash[1],
    };
    Ok(template)
}

// The request of a Blob Batch of Delete Blobs, which sends `fields`. Its
// parts are its body.
fn describe_batch<'p>(fields: &'p [Field<'p>]) -> Describe<'p, 2> {
    Describe {
        method: Method::Post,
        key: false,
        query: [
            Some((
                "restype",
                ParamValue::Fixed(QueryValue::Literal("container")),
            )),
            Some(("comp", ParamValue::Fixed(QueryValue::Literal("batch")))),
        ],
        holes: Holes {
            content_length: Some(Hole::BodyLength),
            ..Holes::default()
        },
        fields,
        metadata: &[],
        body: Body::Each,
    }
}

/// Writes the Blob Batch that removes `objects` at `now` from `template`
/// into `buf`, with its parts as the body after the head, and returns it.
///
/// # Errors
///
/// Returns [`InvalidPlan::Keys`] for no object or more than
/// [`MAX_BATCH_KEYS`], [`Error::InvalidPlan`] for a key or a revision that
/// Azure refuses, and [`InvalidPlan::Template`] for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_delete_many<'r>(
    template: &Template,
    bytes: &[u8],
    objects: &[DeleteTarget<'_>],
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    let Detail::Batch { path, tail } = template.detail else {
        return Err(InvalidPlan::Template.into());
    };
    if objects.is_empty() || objects.len() > MAX_BATCH_KEYS {
        return Err(InvalidPlan::Keys.into());
    }
    for object in objects {
        validate_key(object.key, template.namespace)?;
        validate_revision(object.revision, true)?;
    }
    let (Some(path), Some(tail)) = (
        bytes.get(path.start..path.start + path.len),
        bytes.get(tail.start..tail.start + tail.len),
    ) else {
        return Err(InvalidPlan::Template.into());
    };
    let fill = BatchFill {
        values: Values {
            now: Some(now),
            ..Values::default()
        },
        objects,
        path,
        tail,
        now,
    };
    encode_template(
        template,
        bytes,
        &fill,
        Payload::Slice(&[]),
        buf,
        &describe_batch(&[]),
    )
}

/// Reads the response head of a Blob Batch and reports what to do next.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 202, or a 202 without a multipart `Content-Type`, is a
/// [`ResponseFault`].
pub fn accept_delete_many_head<'h>(head: ResponseHead<'h>) -> Result<DeleteManyHeadOutcome<'h>> {
    match head.status {
        202 => {
            multipart::boundary(head.content_type.ok_or(ResponseFault::Head)?)?;
            Ok(DeleteManyHeadOutcome::Results {
                expected_len: decimal_header(head.content_length)?,
            })
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`DeleteManyHeadOutcome::NeedErrorBody`] with the response
/// body.
pub fn accept_delete_many_error_body<'h>(
    failure: Failure<'h>,
    body: &[u8],
) -> DeleteManyHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Reads the result of each removal of a batch of `count` objects out of
/// the response `body` into `into`, in the order the batch named them, and
/// returns how many it read. `head` is the response head, whose
/// `Content-Type` names the boundary of the parts.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if `into` holds fewer than `count` results.
///
/// Returns [`Error::Response`] with [`ResponseFault::Body`] for a body that
/// does not answer each removal once.
pub fn fill_delete_results<'b>(
    count: usize,
    head: ResponseHead<'_>,
    body: &'b [u8],
    into: &mut [BatchResult<'b>],
) -> Result<usize> {
    if count == 0 || count > MAX_BATCH_KEYS {
        return Err(InvalidPlan::Keys.into());
    }
    if into.len() < count {
        return Err(crate::Error::Capacity(crate::CapacityError {
            required: count,
        }));
    }
    let boundary = multipart::boundary(head.content_type.ok_or(ResponseFault::Head)?)?;
    // One bit for each request that an answer has named.
    let mut answered = [0u64; MAX_BATCH_KEYS.div_ceil(64)];
    let mut held = 0;
    for part in multipart::parts(body, boundary) {
        let part = part?;
        let index = request_index(&part, count)?;
        let bit = 1 << (index % 64);
        if answered[index / 64] & bit != 0 {
            return Err(ResponseFault::Body.into());
        }
        answered[index / 64] |= bit;
        into[index] = read_removal(part.content)?;
        held += 1;
    }
    if held != count {
        return Err(ResponseFault::Body.into());
    }
    Ok(count)
}

// Reads the response to one removal, as the answer to a single Delete Blob
// is read.
fn read_removal(message: &[u8]) -> Result<BatchResult<'_>> {
    let response = Response::parse(message)?;
    let head = response.head();
    let shape = DeleteShape::default();
    let outcome = match super::accept_delete_head(shape, head)? {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            super::accept_delete_error_body(shape, failure, response.content)
        }
        outcome => outcome,
    };
    Ok(BatchResult {
        outcome,
        head,
        body: response.content,
    })
}
