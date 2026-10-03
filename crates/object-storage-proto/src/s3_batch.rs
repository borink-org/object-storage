// S3 DeleteObjects: the removal of up to 1,000 objects in one request, whose
// answer holds a result for each.

use crate::checksum::ChecksumKind;
use crate::common::{decimal_header, encoded_with_body, failure, finish_with_body, push_checksum};
use crate::encoding;
use crate::request::{ByteSink, HeadWriter, U64Decimal, Writer};
use crate::s3::{
    CHECKSUM_TEXT_LEN, Objects, Service, Signed, body_kind, refuse_error_document, validate_key,
    validate_s3_checksum,
};
use crate::url::QueryValue;
use crate::{
    ConditionKind, DeleteManyHeadOutcome, Failure, HeaderSpan, InvalidPlan, Method,
    PhysicalDeleteMany, RequestedRange, ResponseFault, ResponseHead, Result, Timestamps,
    TransactionalChecksum, WireRequest,
};

#[cfg(doc)]
use crate::Error;

/// The most objects that one DeleteObjects removes.
pub const MAX_DELETE_KEYS: usize = 1000;

const DELETE_OPEN: &[u8] = b"<Delete xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">";
const DELETE_CLOSE: &[u8] = b"</Delete>";

/// The result of the removal of one object in a DeleteObjects.
///
/// [`Objects::fill_delete_results`] writes one for each object that the
/// request named. S3 reports a key that held no object as removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteResult<'b> {
    /// The key, as S3 wrote it.
    pub key: &'b str,
    /// The error code if S3 did not remove the object, such as
    /// `AccessDenied`, or [`None`] if it did.
    pub code: Option<&'b str>,
}

impl<'a> Objects<'a> {
    /// Writes a DeleteObjects into `buf`: the signed head, then the XML body
    /// after it.
    ///
    /// The body is written into `buf` after the head, so
    /// [`WireRequest::body_span`] names it and [`WireRequest::payload`]
    /// borrows it. The request signs the SHA-256 of the body. A plan that
    /// asks for a CRC-64/NVME sends it as `x-amz-checksum-crc64nvme`, and one
    /// that asks for an MD5 as `Content-MD5`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an S3 request:
    ///
    /// - [`InvalidPlan::Keys`] if it names no key, or more than
    ///   [`MAX_DELETE_KEYS`].
    /// - A key that [`Self::encode_get`] refuses, and
    ///   [`InvalidPlan::KeyControlCharacter`] for a key with a control
    ///   character other than a tab, which the XML body cannot carry.
    /// - [`InvalidPlan::Option`] for a checksum whose provider the client
    ///   has not registered, or for no checksum, for [`Service::Aws`]. AWS
    ///   refuses the request without one.
    ///
    /// Returns [`Error::Capacity`] with the bytes that the head and the body
    /// need together, or call
    /// [`layered::s3::delete_many_requirements`](crate::layered::s3::delete_many_requirements)
    /// first.
    pub fn encode_delete_many<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalDeleteMany<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        if plan.keys.is_empty() || plan.keys.len() > MAX_DELETE_KEYS {
            return Err(InvalidPlan::Keys.into());
        }
        for key in plan.keys {
            validate_key(key)?;
            if key
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
            {
                return Err(InvalidPlan::KeyControlCharacter.into());
            }
        }
        let needs_checksum = match self.bucket.service {
            Service::Aws | Service::AwsDirectory => true,
            Service::Compatible => false,
        };
        if needs_checksum && plan.checksum.is_none() {
            return Err(InvalidPlan::Option.into());
        }
        let checksum = plan.checksum.map(TransactionalChecksum::Compute);
        validate_s3_checksum(checksum, true, &self.checksums)?;

        let mut counted = Writer::new(&mut []);
        write_delete(&mut counted, plan.keys);
        let length = counted.position();
        let dry = buf.is_empty();
        let content_sha256 = if dry {
            [b'0'; 64]
        } else {
            let mut sum = self.sha256.start();
            write_delete(&mut sum, plan.keys);
            encoding::hex(&sum.finish())
        };
        let mut text = [0; CHECKSUM_TEXT_LEN];
        let signed_checksum =
            self.signed_checksum(checksum, |sum| write_delete(sum, plan.keys), dry, &mut text);
        let signed = Signed {
            method: Method::Post,
            key: None,
            query: &[Some(("delete", QueryValue::Literal("")))],
            headers: signed_checksum.as_slice(),
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: &content_sha256,
            tags: &[],
        };
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        head.header("content-length", |out| {
            out.push(U64Decimal::new(length as u64).as_bytes());
        });
        let md5 = checksum.filter(|_| plan.checksum == Some(ChecksumKind::Md5));
        push_checksum(&mut head, md5, &self.checksums, |sum| {
            write_delete(sum, plan.keys);
        });
        encoded_with_body(head, Method::Post, |out| write_delete(out, plan.keys))
    }

    /// Reads the response head of a DeleteObjects and reports what to do
    /// next. A failure is [`DeleteManyHeadOutcome::NeedErrorBody`]: read the
    /// body and pass it to [`Self::accept_delete_many_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`].
    pub fn accept_delete_many_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<DeleteManyHeadOutcome<'h>> {
        match head.status {
            200 => Ok(DeleteManyHeadOutcome::Results {
                expected_len: decimal_header(head.content_length)?,
            }),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(DeleteManyHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`DeleteManyHeadOutcome::NeedErrorBody`] with the response
    /// body. A missing bucket is [`DeleteManyHeadOutcome::NotFound`].
    pub fn accept_delete_many_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> DeleteManyHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Reads the result of each removal out of the response body of a
    /// DeleteObjects into `into`, and returns how many it read.
    ///
    /// The body is decoded in place, and the results borrow it. An array as
    /// long as the plan's keys holds every result.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`] if the body holds more results than the
    /// array, with `required` set to the number it holds.
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a `DeleteResult`, and [`Error::Service`] if it is an error
    /// document, which S3 can send under status 200.
    pub fn fill_delete_results<'b>(
        &self,
        body: &'b mut [u8],
        into: &mut [DeleteResult<'b>],
    ) -> Result<usize> {
        refuse_error_document(body)?;
        crate::xml::s3_batch::fill_delete_results(body, into)
    }
}

fn write_delete(out: &mut dyn ByteSink, keys: &[&str]) {
    out.push(DELETE_OPEN);
    for key in keys {
        out.push(b"<Object><Key>");
        encoding::write_xml_text(out, key.as_bytes());
        out.push(b"</Key></Object>");
    }
    out.push(DELETE_CLOSE);
}
