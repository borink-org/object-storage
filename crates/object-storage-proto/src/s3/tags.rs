// S3 object tags: PutObjectTagging and GetObjectTagging. A write stores tags
// with the object as well, through `WriteOptions`.

use crate::checksum::ChecksumKind;
use crate::common::{
    decimal_header, encoded_with_body, failure, finish_with_body, push_checksum, validate_revision,
    validate_tags, write_tag_set,
};
use crate::encoding;
use crate::request::{ByteSink, HeadWriter, U64Decimal, Writer};
use crate::s3::{
    CHECKSUM_TEXT_LEN, Objects, Service, Signed, body_kind, refuse_error_document, s3_tag_char,
    validate_key, validate_s3_checksum, version_parameter,
};
use crate::url::QueryValue;
use crate::{
    ConditionKind, Failure, HeaderSpan, Method, PhysicalSetTags, RequestedRange, ResponseFault,
    ResponseHead, Result, Revision, Tag, TagsHeadOutcome, Timestamps, TransactionalChecksum,
    UpdateHeadOutcome, WireRequest,
};

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::{Error, InvalidPlan};

const TAGGING_OPEN: &[u8] = b"<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">";
const TAGGING_CLOSE: &[u8] = b"</Tagging>";

impl<'a> Objects<'a> {
    /// Writes a PutObjectTagging into `buf`: the signed head, then the XML
    /// body after it. The object then holds these tags and no others.
    ///
    /// The body is written into `buf` after the head, so
    /// [`WireRequest::body_span`] names it and [`WireRequest::payload`]
    /// borrows it. The request signs the SHA-256 of the body. A plan that
    /// asks for a CRC-64/NVME sends it as `x-amz-checksum-crc64nvme`, and one
    /// that asks for an MD5 as `Content-MD5`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key that [`Self::encode_get`]
    /// refuses, [`InvalidPlan::Tag`] for a tag that breaks the rules of
    /// [`Tag`], and [`InvalidPlan::Option`] for a checksum whose provider
    /// the client has not registered.
    ///
    /// Returns [`Error::Capacity`] with the bytes that the head and the body
    /// need together, or call
    /// [`layered::s3::put_tagging_requirements`](crate::layered::s3::put_tagging_requirements)
    /// first.
    pub fn encode_put_tagging<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalSetTags<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key)?;
        validate_revision(plan.revision, false)?;
        let limits = match self.bucket.service {
            Service::Aws | Service::AwsDirectory => Some((10, 128, 256)),
            Service::Compatible => None,
        };
        validate_tags(plan.tags, s3_tag_char, limits)?;
        let checksum = plan.checksum.map(TransactionalChecksum::Compute);
        validate_s3_checksum(checksum, true, &self.checksums)?;

        let mut counted = Writer::new(&mut []);
        write_tagging(&mut counted, plan.tags);
        let length = counted.position();
        let dry = buf.is_empty();
        let content_sha256 = if dry {
            [b'0'; 64]
        } else {
            let mut sum = self.sha256.start();
            write_tagging(&mut sum, plan.tags);
            encoding::hex(&sum.finish())
        };
        let mut text = [0; CHECKSUM_TEXT_LEN];
        let signed_checksum = self.signed_checksum(
            checksum,
            |sum| write_tagging(sum, plan.tags),
            dry,
            &mut text,
        );
        let query = [
            Some(("tagging", QueryValue::Literal(""))),
            version_parameter(plan.revision),
        ];
        let signed = Signed {
            method: Method::Put,
            key: Some(plan.key),
            query: &query,
            headers: signed_checksum.as_slice(),
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: &content_sha256,
            tags: &[],
            copy: None,
        };
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        head.header("content-length", U64Decimal::new(length as u64).as_bytes());
        let md5 = checksum.filter(|_| plan.checksum == Some(ChecksumKind::Md5));
        push_checksum(&mut head, md5, &self.checksums, |sum| {
            write_tagging(sum, plan.tags);
        });
        encoded_with_body(head, Method::Put, |out| write_tagging(out, plan.tags))
    }

    /// Reads the response head of a PutObjectTagging and reports what S3
    /// did. A failure is [`UpdateHeadOutcome::NeedErrorBody`]: read the body
    /// and pass it to [`Self::accept_update_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`].
    pub fn accept_put_tagging_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<UpdateHeadOutcome<'h>> {
        match head.status {
            200 => Ok(UpdateHeadOutcome::Updated),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(UpdateHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes an [`UpdateHeadOutcome::NeedErrorBody`] with the response
    /// body. A missing object is [`UpdateHeadOutcome::NotFound`].
    pub fn accept_update_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> UpdateHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Writes the signed request head of a GetObjectTagging into `buf`,
    /// which reads the tags of `key`, or of the version that `revision`
    /// names.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key or a revision that
    /// [`Self::encode_get`] refuses.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::s3::get_tagging_requirements`](crate::layered::s3::get_tagging_requirements)
    /// first.
    pub fn encode_get_tagging<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        key: &str,
        revision: Option<Revision<'_>>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(key)?;
        validate_revision(revision, false)?;
        let query = [
            Some(("tagging", QueryValue::Literal(""))),
            version_parameter(revision),
        ];
        let signed = Signed {
            method: Method::Get,
            key: Some(key),
            query: &query,
            headers: &[],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: crate::sigv4::EMPTY_SHA256.as_bytes(),
            tags: &[],
            copy: None,
        };
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        crate::common::encoded(head, Method::Get, crate::Payload::Slice(&[]))
    }

    /// Reads the response head of a GetObjectTagging and reports what to do
    /// next. A failure is [`TagsHeadOutcome::NeedErrorBody`]: read the body
    /// and pass it to [`Self::accept_get_tagging_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`].
    pub fn accept_get_tagging_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<TagsHeadOutcome<'h>> {
        match head.status {
            200 => Ok(TagsHeadOutcome::Tags {
                expected_len: decimal_header(head.content_length)?,
            }),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(TagsHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`TagsHeadOutcome::NeedErrorBody`] with the response body.
    /// A missing object is [`TagsHeadOutcome::NotFound`].
    pub fn accept_get_tagging_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> TagsHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Reads the tags out of the response body of a GetObjectTagging into
    /// `into`, and returns how many it read.
    ///
    /// The body is decoded in place, and the tags borrow it. An array of ten
    /// tags holds any object's on AWS.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`] if the object holds more tags than the
    /// array, with `required` set to the number it holds.
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a `Tagging` document, and [`Error::Service`] if it is an error
    /// document, which S3 can send under status 200.
    pub fn fill_tags<'b>(&self, body: &'b mut [u8], into: &mut [Tag<'b>]) -> Result<usize> {
        refuse_error_document(body)?;
        crate::xml::tags::fill_tags(body, b"Tagging", into)
    }
}

fn write_tagging(out: &mut dyn ByteSink, tags: &[Tag<'_>]) {
    out.push(TAGGING_OPEN);
    write_tag_set(out, tags);
    out.push(TAGGING_CLOSE);
}
