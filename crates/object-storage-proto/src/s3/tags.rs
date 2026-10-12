// S3 object tags: PutObjectTagging and GetObjectTagging. A write stores tags
// with the object as well, through `WriteOptions`.

use crate::checksum::ChecksumKind;
use crate::common::{
    decimal_header, encoded_with_body, finish_with_body, meta_of, push_checksum, validate_revision,
    validate_tags, write_tag_set,
};
use crate::encoding;
use crate::request::{ByteSink, HeadWriter, Pass, U64Decimal, Writer};
use crate::s3::template::{Operation, Template, Unsigned};
use crate::s3::{Bucket, Request, encode_object, prepare_request};
use crate::s3::{
    CHECKSUM_TEXT_LEN, Objects, Service, Signed, body_kind, failed, refuse_error_document,
    s3_tag_char, validate_key, validate_s3_checksum, version_parameter,
};
use crate::sigv4::Credentials;
use crate::template::{Body, ParamValue};
use crate::url;
use crate::url::QueryValue;
use crate::{
    ConditionKind, Failure, HeaderSpan, LegacyRequest, Method, PhysicalSetTags, RequestedRange,
    ResponseFault, ResponseHead, Result, Revision, Tag, TagsHeadOutcome, Timestamps,
    TransactionalChecksum, UpdateHeadOutcome,
};
use crate::{ObjectItem, RevisionKind};

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
    /// [`LegacyRequest::body_span`] names it and [`LegacyRequest::payload`]
    /// borrows it. The request signs the SHA-256 of the body. A plan that
    /// asks for a CRC-64/NVME sends it as `x-amz-checksum-crc64nvme`, and one
    /// that asks for an MD5 as `Content-MD5`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key that [`encode_get`](crate::s3::encode_get)
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
    ) -> Result<LegacyRequest<'r>> {
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
        let pass = Pass::of(buf);
        let content_sha256 = if pass == Pass::Measure {
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
            pass,
            &mut text,
        );
        let query = [
            url::literal("tagging", ""),
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
        self.write_head(&mut head, &signed, pass, now);
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
        accept_put_tagging_head(head)
    }

    /// Finishes an [`UpdateHeadOutcome::NeedErrorBody`] with the response
    /// body. A missing object is [`UpdateHeadOutcome::NotFound`].
    pub fn accept_update_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> UpdateHeadOutcome<'h> {
        accept_update_error_body(failure, body)
    }

    /// Writes the signed request head of a GetObjectTagging into `buf`,
    /// which reads the tags of `key`, or of the version that `revision`
    /// names.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key or a revision that
    /// [`encode_get`](crate::s3::encode_get) refuses.
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
    ) -> Result<LegacyRequest<'r>> {
        validate_key(key)?;
        validate_revision(revision, false)?;
        let query = [url::literal("tagging", ""), version_parameter(revision)];
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
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
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
        accept_get_tagging_head(head)
    }

    /// Finishes a [`TagsHeadOutcome::NeedErrorBody`] with the response body.
    /// A missing object is [`TagsHeadOutcome::NotFound`].
    pub fn accept_get_tagging_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> TagsHeadOutcome<'h> {
        accept_get_tagging_error_body(failure, body)
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
        fill_tags(body, into)
    }
}

fn write_tagging(out: &mut dyn ByteSink, tags: &[Tag<'_>]) {
    out.push(TAGGING_OPEN);
    write_tag_set(out, tags);
    out.push(TAGGING_CLOSE);
}

/// Writes the template of PutObjectTaggings that give objects `tags` and no
/// others into `buf`, signed with `credentials`. Each request names a
/// version of the kind `revision`, or the object as it is now, and sends a
/// checksum of its body of the kind `checksum`.
///
/// The template holds the body. Sign each request with its SHA-256: see
/// [`set_body_hash`](crate::s3::set_body_hash).
///
/// # Errors
///
/// Returns [`InvalidPlan::Tag`] for a tag that breaks the rules of [`Tag`],
/// and [`InvalidPlan::Revision`] for a snapshot.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_put_tagging(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    tags: &[Tag<'_>],
    revision: RevisionKind,
    checksum: Option<ChecksumKind>,
    buf: &mut [u8],
) -> Result<Template> {
    let limits = bucket.service.is_aws().then_some((10, 128, 256));
    validate_tags(tags, s3_tag_char, limits)?;
    let write = |out: &mut dyn ByteSink| write_tagging(out, tags);
    let mut request = Request::object(Operation::PutTagging, Method::Put, revision)?;
    request.query[0] = Some(("tagging", ParamValue::Fixed(QueryValue::Literal(""))));
    request.with_body(Body::Fixed(&write), checksum);
    prepare_request(bucket, credentials, &request, buf)
}

/// Writes the PutObjectTagging of `item` at `now` from `template` into
/// `buf`, with its body after the canonical request, ready to sign.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key that S3 refuses, see
/// [Object keys](crate#object-keys), for a version that is not the
/// template's kind, for a condition value, and for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_put_tagging(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    encode_object(template, bytes, Operation::PutTagging, item, now, buf)
}

/// Reads the response head of a PutObjectTagging and reports what S3 did.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_put_tagging_head<'h>(head: ResponseHead<'h>) -> Result<UpdateHeadOutcome<'h>> {
    match head.status {
        200 => Ok(UpdateHeadOutcome::Updated {
            meta: meta_of(head),
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes an [`UpdateHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_update_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> UpdateHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Writes the template of GetObjectTaggings into `buf`, signed with
/// `credentials`. Each request names a version of the kind `revision`, or
/// the object as it is now.
///
/// # Errors
///
/// Returns [`InvalidPlan::Revision`] for a snapshot.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_get_tagging(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    revision: RevisionKind,
    buf: &mut [u8],
) -> Result<Template> {
    let mut request = Request::object(Operation::GetTagging, Method::Get, revision)?;
    request.query[0] = Some(("tagging", ParamValue::Fixed(QueryValue::Literal(""))));
    prepare_request(bucket, credentials, &request, buf)
}

/// Writes the GetObjectTagging of `item` at `now` from `template` into
/// `buf`, ready to sign.
///
/// # Errors
///
/// As [`encode_put_tagging`].
pub fn encode_get_tagging(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    encode_object(template, bytes, Operation::GetTagging, item, now, buf)
}

/// Reads the response head of a GetObjectTagging and reports what to do
/// next.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_get_tagging_head<'h>(head: ResponseHead<'h>) -> Result<TagsHeadOutcome<'h>> {
    match head.status {
        200 => Ok(TagsHeadOutcome::Tags {
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`TagsHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_get_tagging_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> TagsHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Reads the tags out of the response body of a GetObjectTagging into
/// `into`, and returns how many it read. The tags borrow the body, which is
/// decoded in place.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if the object holds more tags than the array,
/// with `required` set to the number it holds, [`Error::Service`] for an
/// error document, and [`Error::Response`] for a body that is not a
/// `Tagging` document.
pub fn fill_tags<'b>(body: &'b mut [u8], into: &mut [Tag<'b>]) -> Result<usize> {
    refuse_error_document(body)?;
    crate::xml::tags::fill_tags(body, b"Tagging", into)
}
