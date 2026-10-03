// Azure Blob Batch, of Delete Blob requests: the removal of up to 256 blobs
// in one request. The request body is a `multipart/mixed` document that holds
// one Delete Blob for each blob, and the answer is one that holds a response
// for each.

#[cfg(doc)]
use crate::Error;
use crate::azure::{Blobs, body_kind, named, validate_key};
use crate::common::{decimal, decimal_header, encoded_with_body, failure, finish_with_body};
use crate::request::{ByteSink, HeadWriter, U64Decimal, Writer};
use crate::url::QueryValue;
use crate::{
    DeleteManyHeadOutcome, Failure, HeaderSpan, InvalidPlan, Method, PhysicalDeleteMany,
    RequestedRange, ResponseFault, ResponseHead, Result, Timestamps, WireRequest,
};

/// The most blobs that one Blob Batch removes.
pub const MAX_BATCH_KEYS: usize = 256;

// The boundary between the parts of the request body. A part holds the head
// of a Delete Blob, whose key is percent-encoded in its path, so no line of
// it can be the boundary.
const BOUNDARY: &str = "batch_borink-object-storage";

/// The result of one removal in a Blob Batch.
///
/// [`Blobs::fill_delete_results`] writes one for each response in the
/// answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchResult<'b> {
    /// The position of the key in [`PhysicalDeleteMany::keys`].
    pub index: usize,
    /// The status of the removal: 202 if Azure removed the blob.
    pub status: u16,
    /// The error code of a failed removal, such as `BlobNotFound`.
    pub code: Option<&'b str>,
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
        if plan.keys.is_empty() || plan.keys.len() > MAX_BATCH_KEYS {
            return Err(InvalidPlan::Keys.into());
        }
        if plan.checksum.is_some() {
            return Err(InvalidPlan::Option.into());
        }
        for key in plan.keys {
            validate_key(key, self.namespace)?;
        }
        let mut counted = Writer::new(&mut []);
        self.write_batch(&mut counted, plan.keys, now);
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
        head.header("content-type", |out| {
            out.push(b"multipart/mixed; boundary=");
            out.push(BOUNDARY.as_bytes());
        });
        head.header("content-length", |out| {
            out.push(U64Decimal::new(length as u64).as_bytes())
        });
        encoded_with_body(head, Method::Post, |out| {
            self.write_batch(out, plan.keys, now)
        })
    }

    // The body of a batch: one part that holds a Delete Blob for each key,
    // numbered by its position, then the closing boundary.
    fn write_batch(&self, out: &mut dyn ByteSink, keys: &[&str], now: &Timestamps) {
        for (index, key) in keys.iter().enumerate() {
            out.push(b"--");
            out.push(BOUNDARY.as_bytes());
            out.push(b"\r\nContent-Type: application/http\r\n");
            out.push(b"Content-Transfer-Encoding: binary\r\nContent-ID: ");
            out.push(U64Decimal::new(index as u64).as_bytes());
            out.push(b"\r\n\r\nDELETE ");
            self.write_path(out, Some(key));
            out.push(b" HTTP/1.1\r\nx-ms-date: ");
            out.push(now.rfc1123().as_bytes());
            out.push(b"\r\nAuthorization: Bearer ");
            out.push(self.token.as_bytes());
            out.push(b"\r\nContent-Length: 0\r\n\r\n");
        }
        out.push(b"--");
        out.push(BOUNDARY.as_bytes());
        out.push(b"--\r\n");
    }

    /// Reads the response head of a Blob Batch and reports what to do next.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 202 is [`ResponseFault::Status`].
    pub fn accept_delete_many_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<DeleteManyHeadOutcome<'h>> {
        match head.status {
            202 => Ok(DeleteManyHeadOutcome::Results {
                expected_len: decimal_header(head.content_length)?,
            }),
            200..=299 => Err(ResponseFault::Status.into()),
            404 if head.error_code.is_some() => {
                Ok(DeleteManyHeadOutcome::NotFound { kind: named(&head) })
            }
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
    /// Batch into `into`, and returns how many it read.
    ///
    /// The results borrow the body. An array as long as the plan's keys holds
    /// every result.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`] if the body holds more results than the
    /// array, with `required` set to the number it holds.
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a `multipart/mixed` document of HTTP responses.
    pub fn fill_delete_results<'b>(
        &self,
        body: &'b [u8],
        into: &mut [BatchResult<'b>],
    ) -> Result<usize> {
        fill_batch(body, into)
    }
}

// Reads the parts of a batch answer. The first line names the boundary. Each
// part holds a head of its own, which may name the `Content-ID` of the
// request it answers, a blank line, and the response: a status line, a head
// that may name an error code, a blank line, and a body up to the next
// boundary.
fn fill_batch<'b>(body: &'b [u8], into: &mut [BatchResult<'b>]) -> Result<usize> {
    let fault = || Err(ResponseFault::Body.into());
    let mut lines = body
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    let first = lines.find(|line| !line.is_empty()).unwrap_or_default();
    let Some(boundary) = first.strip_prefix(b"--").filter(|rest| !rest.is_empty()) else {
        return fault();
    };
    let mut held = 0;
    let mut closed = false;
    while !closed {
        let mut index = None;
        for line in lines.by_ref() {
            if line.is_empty() {
                break;
            }
            if let Some(value) = header_value(line, b"content-id") {
                index = Some(decimal(value).ok_or(ResponseFault::Body)?);
            }
        }
        let status_line = lines.next().unwrap_or_default();
        let status = match status_line.strip_prefix(b"HTTP/1.1 ") {
            Some(rest) if rest.len() >= 3 => decimal(&rest[..3]).ok_or(ResponseFault::Body)?,
            _ => return fault(),
        };
        let mut code = None;
        for line in lines.by_ref() {
            if line.is_empty() {
                break;
            }
            if let Some(value) = header_value(line, b"x-ms-error-code") {
                code = Some(core::str::from_utf8(value).or(Err(ResponseFault::Body))?);
            }
        }
        // The body of the response, up to the next boundary.
        let mut delimited = false;
        for line in lines.by_ref() {
            if let Some(rest) = line
                .strip_prefix(b"--")
                .and_then(|rest| rest.strip_prefix(boundary))
            {
                closed = rest == b"--";
                delimited = closed || rest.is_empty();
                if delimited {
                    break;
                }
            }
        }
        if !delimited {
            return fault();
        }
        let result = BatchResult {
            index: index.map_or(held, |index| index as usize),
            status: u16::try_from(status).or(Err(ResponseFault::Body))?,
            code,
        };
        if let Some(slot) = into.get_mut(held) {
            *slot = result;
        }
        held += 1;
    }
    if held > into.len() {
        return Err(crate::Error::Capacity(crate::CapacityError {
            required: held,
            ..crate::CapacityError::default()
        }));
    }
    Ok(held)
}

// The value of a header line if it names `name`, without case, trimmed.
fn header_value<'l>(line: &'l [u8], name: &[u8]) -> Option<&'l [u8]> {
    let (found, value) = line.split_at_checked(name.len())?;
    let value = value.strip_prefix(b":")?;
    found.eq_ignore_ascii_case(name).then(|| value.trim_ascii())
}
