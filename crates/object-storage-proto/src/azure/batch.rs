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
use crate::azure::{Blobs, body_kind, named, revision_parameter, validate_key};
use crate::common::{
    decimal, decimal_header, encoded_with_body, failure, finish_with_body, missing,
    validate_revision,
};
use crate::http_message::Response;
use crate::multipart::{self, Part};
use crate::request::{ByteSink, HeadWriter, U64Decimal, Writer};
use crate::url::{self, QueryValue};
use crate::{
    DeleteHeadOutcome, DeleteManyHeadOutcome, DeleteShape, DeleteTarget, Failure, HeaderSpan,
    InvalidPlan, Method, PhysicalDeleteMany, RequestedRange, ResponseFault, ResponseHead, Result,
    Timestamps, WireRequest,
};

/// The most blobs that one Blob Batch removes.
pub const MAX_BATCH_KEYS: usize = 256;

// The boundary between the parts of the request body. A part holds the head
// of a Delete Blob, whose key is percent-encoded in its path, so no line of
// it can be the boundary.
const BOUNDARY: &str = "batch_borink-object-storage";

/// The result of one removal in a Blob Batch.
///
/// [`Blobs::fill_delete_results`] writes one for each key of the plan, at
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

impl<'a> Blobs<'a> {
    /// Writes a Blob Batch of Delete Blob requests into `buf`: the head, then
    /// the `multipart/mixed` body after it.
    ///
    /// Each removal in the batch carries the date and the token of this
    /// client. The body is written into `buf` after the head, so
    /// [`WireRequest::body_span`] names it and [`WireRequest::payload`]
    /// borrows it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an Azure
    /// request: [`InvalidPlan::Keys`] if it names no key or more than
    /// [`MAX_BATCH_KEYS`], a key that [`Self::encode_get`] refuses, or
    /// [`InvalidPlan::Option`] for a checksum, which a batch does not take.
    ///
    /// Returns [`Error::Capacity`] with the bytes that the head and the body
    /// need together, or call
    /// [`layered::delete_many_requirements`](crate::layered::delete_many_requirements)
    /// first.
    pub fn encode_delete_many<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalDeleteMany<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        if plan.objects.is_empty() || plan.objects.len() > MAX_BATCH_KEYS {
            return Err(InvalidPlan::Keys.into());
        }
        if plan.checksum.is_some() {
            return Err(InvalidPlan::Option.into());
        }
        for object in plan.objects {
            validate_key(object.key, self.namespace)?;
            validate_revision(object.revision, true)?;
        }
        let mut counted = Writer::new(&mut []);
        self.write_batch(&mut counted, plan.objects, now);
        let length = counted.position();
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            None,
            &[
                Some(("restype", QueryValue::Literal("container"))),
                Some(("comp", QueryValue::Literal("batch"))),
            ],
            RequestedRange::Whole,
            now,
        )?;
        head.header("content-type", multipart::MixedContentType(BOUNDARY));
        head.header("content-length", U64Decimal::new(length as u64).as_bytes());
        encoded_with_body(head, Method::Post, |out| {
            self.write_batch(out, plan.objects, now)
        })
    }

    // The body of a batch: a part for each key, numbered by its position,
    // that holds a Delete Blob request. The request has no content, so the
    // CRLF of the delimiter after it stands for the empty line that ends its
    // head, as in the example of Azure's documentation.
    fn write_batch(&self, out: &mut dyn ByteSink, objects: &[DeleteTarget<'_>], now: &Timestamps) {
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
            self.write_path(out, Some(object.key));
            url::write_query_in_url(out, &[revision_parameter(object.revision)]);
            out.push(b" HTTP/1.1\r\nx-ms-date: ");
            out.push(now.rfc1123().as_bytes());
            out.push(b"\r\nAuthorization: Bearer ");
            out.push(self.token.as_bytes());
            out.push(b"\r\nContent-Length: 0\r\n");
        }
        multipart::write_close_delimiter(out, BOUNDARY);
    }

    /// Reads the response head of a Blob Batch and reports what to do next.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 202 is [`ResponseFault::Status`], and a 202 whose
    /// `Content-Type` is not `multipart/mixed` with a boundary is
    /// [`ResponseFault::Head`].
    pub fn accept_delete_many_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<DeleteManyHeadOutcome<'h>> {
        match head.status {
            202 => {
                multipart::boundary(head.content_type.ok_or(ResponseFault::Head)?)?;
                Ok(DeleteManyHeadOutcome::Results {
                    expected_len: decimal_header(head.content_length)?,
                })
            }
            200..=299 => Err(ResponseFault::Status.into()),
            404 if head.error_code.is_some() => Ok(missing(&head, named(&head))),
            status if head.error_code.is_none() => Ok(DeleteManyHeadOutcome::NeedErrorBody(
                failure(status, None, head.request_id),
            )),
            status => Ok(DeleteManyHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`DeleteManyHeadOutcome::NeedErrorBody`] with the response
    /// body.
    pub fn accept_delete_many_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> DeleteManyHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Reads the result of each removal out of the response body of a Blob
    /// Batch into `into`, at the position of each key in `plan`, and returns
    /// how many it read: one for each key.
    ///
    /// Pass the plan of the request and the head that
    /// [`Self::accept_delete_many_head`] read, whose `Content-Type` names the
    /// boundary between the parts. The results borrow `body`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] with [`InvalidPlan::Keys`] for a plan
    /// that [`Self::encode_delete_many`] refuses for its number of keys, and
    /// [`Error::Capacity`] if `into` is shorter than the plan's keys, with
    /// `required` set to their number. Both are reported before anything is
    /// read.
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Head`] if the head
    /// names no `multipart/mixed` boundary, and with [`ResponseFault::Body`]
    /// if `body` is not a batch answer: a `multipart/mixed` body whose parts
    /// each carry an HTTP/1.1 response and the `Content-ID` of a request,
    /// with every request answered once. A response that a single Delete
    /// Blob could not answer is the error that [`Self::accept_delete_head`]
    /// returns for it.
    pub fn fill_delete_results<'b>(
        &self,
        plan: &PhysicalDeleteMany<'_>,
        head: ResponseHead<'_>,
        body: &'b [u8],
        into: &mut [BatchResult<'b>],
    ) -> Result<usize> {
        let count = plan.objects.len();
        if count == 0 || count > MAX_BATCH_KEYS {
            return Err(InvalidPlan::Keys.into());
        }
        if into.len() < count {
            return Err(crate::Error::Capacity(crate::CapacityError {
                required: count,
                ..crate::CapacityError::default()
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
            into[index] = self.read_removal(part.content)?;
            held += 1;
        }
        if held != count {
            return Err(ResponseFault::Body.into());
        }
        Ok(count)
    }

    // Reads the response to one removal, as the answer to a single Delete
    // Blob is read.
    fn read_removal<'b>(&self, message: &'b [u8]) -> Result<BatchResult<'b>> {
        let response = Response::parse(message)?;
        let head = response.head();
        let shape = DeleteShape::default();
        let outcome = match self.accept_delete_head(shape, head)? {
            DeleteHeadOutcome::NeedErrorBody(failure) => {
                self.accept_delete_error_body(shape, failure, response.content)
            }
            outcome => outcome,
        };
        Ok(BatchResult {
            outcome,
            head,
            body: response.content,
        })
    }
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
