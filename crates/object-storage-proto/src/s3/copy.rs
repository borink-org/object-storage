// S3 copies, which the service carries out without sending the bytes
// through the client: CopyObject, and UploadPartCopy, which stages a part of
// an upload from a range of another object.
//
// Each names its source in `x-amz-copy-source`, which the request signs
// with the conditions on the source. S3 answers both with status 200 before
// it has finished, and says in the body whether the copy succeeded.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::common::{
    decimal_header, encoded, failure, finish_with_body, validate_condition, validate_revision,
    validate_tags,
};
use crate::common::{validate_revision_item, validate_source_container, validate_template_options};
use crate::request::{HeadWriter, Pass};
use crate::s3::template::{
    Detail, Field, Operation, Template, Unsigned, Value, encode as encode_template,
};
use crate::s3::{Bucket, Request, S3Values, prepare_request, push_stored, validate_stored};
use crate::s3::{
    Objects, PayloadHash, Service, Signed, SignedCopy, Stores, body_kind, failed,
    is_error_document, s3_tag_char, stored_headers, validate_content, validate_key,
    validate_metadata, validate_write_condition,
};
use crate::sigv4::Credentials;
use crate::sigv4::{EMPTY_SHA256, Token};
use crate::template::{Hole, ParamValue, Values};
use crate::url;
use crate::{
    ConditionKind, CopyHeadOutcome, CopyShape, CopySource, Failure, HeaderSpan, InvalidPlan,
    LegacyRequest, Method, ObjectMeta, Payload, PhysicalCopy, RequestedRange, ResponseFault,
    ResponseHead, Result, ServiceErrorKind, StageHeadOutcome, Timestamps,
};
use crate::{CopyItem, CopyOptions, Ranging, RevisionKind, WriteOptions};

/// One UploadPartCopy: a part of an upload staged from the bytes of another
/// object, which S3 reads itself.
///
/// Commit the part as you commit one that
/// [`Objects::encode_stage_part`](crate::s3::Objects::encode_stage_part)
/// staged, by its number and the entity tag that the result carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalStagePartCopy<'a> {
    /// The key of the upload, under the rules of
    /// [Object keys](crate#object-keys).
    pub key: &'a str,
    /// The ID of the upload, as
    /// [`Objects::read_upload_id`](crate::s3::Objects::read_upload_id) read
    /// it.
    pub upload_id: &'a str,
    /// The number of the part, under the rules of
    /// [`PhysicalStagePart::number`](crate::s3::PhysicalStagePart::number).
    pub number: u32,
    /// The object whose bytes the part holds.
    pub source: CopySource<'a>,
    /// The bytes of the source that the part holds:
    /// [`RequestedRange::Whole`], or a [`RequestedRange::Bounded`] range,
    /// sent in `x-amz-copy-source-range`. A client refuses any other range
    /// with [`InvalidPlan::UnsupportedRange`], and an empty one with
    /// [`InvalidPlan::Range`].
    pub range: RequestedRange,
}

impl<'a> PhysicalStagePartCopy<'a> {
    /// Creates a plan that stages all of `source` as part `number` of the
    /// upload `upload_id` of `key`.
    pub const fn new(
        key: &'a str,
        upload_id: &'a str,
        number: u32,
        source: CopySource<'a>,
    ) -> Self {
        Self {
            key,
            upload_id,
            number,
            source,
            range: RequestedRange::Whole,
        }
    }
}

impl<'a> Objects<'a> {
    /// Writes the signed request head of a CopyObject into `buf`, which
    /// copies an object of the same service onto `plan.key`.
    ///
    /// New metadata or a content property replaces the source's metadata and
    /// content properties together, and tags replace its tags: see
    /// [`PhysicalCopy::options`]. S3 copies an object onto itself only if the
    /// copy changes something, and a general purpose bucket refuses one that
    /// does not with 400 `InvalidRequest`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an S3 request:
    ///
    /// - A key, a metadata pair, a content property, a tag, a storage class
    ///   or a condition on the target that [`encode_put`](crate::s3::encode_put) refuses.
    /// - [`InvalidPlan::Option`] for a checksum or a declared MD5, and for
    ///   a client that holds the credentials of a session: see
    ///   [directory buckets](crate::s3#directory-buckets).
    /// - [`InvalidPlan::CopySource`] for a source bucket that cannot be
    ///   written into the header, and [`InvalidPlan::Revision`] for a
    ///   snapshot or an empty version.
    /// - [`InvalidPlan::Condition`] for an invalid condition on the source.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::s3::copy_requirements`](crate::layered::s3::copy_requirements)
    /// first.
    pub fn encode_copy<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCopy<'_>,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
        self.validate_copy_credentials()?;
        validate_key(plan.key)?;
        validate_source(&plan.source)?;
        validate_metadata(plan.metadata.unwrap_or(&[]), self.bucket.service)?;
        // A copy names its tags apart, so that none can differ from the
        // source's.
        if plan.options.checksum.is_some() || !plan.options.tags.is_empty() {
            return Err(InvalidPlan::Option.into());
        }
        let limits = match self.bucket.service {
            Service::Aws | Service::AwsDirectory => Some((10, 128, 256)),
            Service::Compatible => None,
        };
        validate_tags(plan.tags.unwrap_or(&[]), s3_tag_char, limits)?;
        validate_content(
            &plan.options,
            Payload::Slice(&[]),
            PayloadHash::Compute,
            &self.checksums,
            Stores::Object(self.bucket.service),
        )?;
        validate_write_condition(plan.condition, plan.condition_value, self.bucket.service)?;
        let (stored, count) = stored_headers(&plan.options, None);
        let mut copy_headers = [("", &[][..]); 10];
        copy_headers[..count].copy_from_slice(&stored[..count]);
        let mut count = count;
        // S3 keeps the source's metadata and content properties unless told
        // to replace them, and its tags likewise. Replacing the tags with
        // none sends an empty `x-amz-tagging`.
        if plan.metadata.is_some() || !plan.options.properties.is_empty() {
            copy_headers[count] = ("x-amz-metadata-directive", b"REPLACE");
            count += 1;
        }
        let tags = plan.tags.unwrap_or(&[]);
        if plan.tags.is_some() {
            copy_headers[count] = ("x-amz-tagging-directive", b"REPLACE");
            count += 1;
            if tags.is_empty() {
                copy_headers[count] = ("x-amz-tagging", b"");
                count += 1;
            }
        }
        let signed = Signed {
            method: Method::Put,
            key: Some(plan.key),
            query: &[],
            headers: &copy_headers[..count],
            range: RequestedRange::Whole,
            condition: plan.condition,
            condition_value: plan.condition_value,
            metadata: plan.metadata.unwrap_or(&[]),
            content_sha256: EMPTY_SHA256.as_bytes(),
            tags,
            copy: Some(SignedCopy {
                source: plan.source,
                range: RequestedRange::Whole,
            }),
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        head.header("content-length", b"0");
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of a CopyObject and reports what to do next.
    ///
    /// S3 answers a copy that it takes with status 200 before it has
    /// finished, and says in the body whether it succeeded. So a success is
    /// [`CopyHeadOutcome::NeedResultBody`]: read the body and pass it to
    /// [`Self::accept_copy_body`]. A failure is
    /// [`CopyHeadOutcome::NeedErrorBody`]: read the body and pass it to
    /// [`Self::accept_copy_error_body`]. S3 answers a failed condition, on the
    /// target or on the source, with 412 `PreconditionFailed`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200, and a failed condition on a copy that carried
    /// none, are both [`ResponseFault::Status`].
    pub fn accept_copy_head<'h>(
        &self,
        shape: CopyShape,
        head: ResponseHead<'h>,
    ) -> Result<CopyHeadOutcome<'h>> {
        accept_copy_head(shape, head)
    }

    /// Finishes a [`CopyHeadOutcome::NeedResultBody`] with the response body.
    ///
    /// A body that holds the result is [`CopyHeadOutcome::Copied`], whose
    /// entity tag the body carries and whose version the head does. A body
    /// that holds an error is the outcome that the error has under its own
    /// status, and any other error is a [`CopyHeadOutcome::ServiceFailure`]
    /// under status 200, such as `InternalError`, which a retry can fix.
    ///
    /// The body is decoded in place. The outcome borrows both `body` and
    /// `head`, so `head` must not borrow the buffer that holds the body.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Status`] if `head`
    /// is not a success, and with [`ResponseFault::Body`] if `body` is
    /// neither a `CopyObjectResult` that holds an entity tag nor an error
    /// document.
    pub fn accept_copy_body<'h>(
        &self,
        shape: CopyShape,
        head: ResponseHead<'h>,
        body: &'h mut [u8],
    ) -> Result<CopyHeadOutcome<'h>> {
        accept_copy_body(shape, head, body)
    }

    /// Finishes a [`CopyHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`accept_get_error_body`](crate::s3::accept_get_error_body) for a copy, and reads the body
    /// the same way. A source that does not exist is
    /// [`CopyHeadOutcome::NotFound`].
    pub fn accept_copy_error_body<'h>(
        &self,
        shape: CopyShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> CopyHeadOutcome<'h> {
        accept_copy_error_body(shape, failure, body)
    }

    /// Writes the signed request head of an UploadPartCopy into `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key, an upload ID or a part
    /// number that [`Self::encode_stage_part`] refuses, a source or a client
    /// that [`Self::encode_copy`] refuses, and a range that
    /// [`PhysicalStagePartCopy::range`] does not allow.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::s3::stage_part_copy_requirements`](crate::layered::s3::stage_part_copy_requirements)
    /// first.
    pub fn encode_stage_part_copy<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalStagePartCopy<'_>,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
        self.validate_copy_credentials()?;
        validate_key(plan.key)?;
        super::parts::validate_upload_id(plan.upload_id)?;
        super::parts::validate_part_number(plan.number, self.bucket.service)?;
        validate_source(&plan.source)?;
        match plan.range {
            RequestedRange::Whole => {}
            RequestedRange::Bounded { start, end } if start >= end => {
                return Err(InvalidPlan::Range.into());
            }
            RequestedRange::Bounded { .. } => {}
            _ => return Err(InvalidPlan::UnsupportedRange.into()),
        }
        let query = [
            url::number("partNumber", plan.number),
            url::encoded("uploadId", plan.upload_id),
        ];
        let signed = Signed {
            method: Method::Put,
            key: Some(plan.key),
            query: &query,
            headers: &[],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
            tags: &[],
            copy: Some(SignedCopy {
                source: plan.source,
                range: plan.range,
            }),
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        head.header("content-length", b"0");
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of an UploadPartCopy and reports what to do
    /// next.
    ///
    /// As for a CopyObject, a success is [`StageHeadOutcome::NeedResultBody`]:
    /// read the body and pass it to [`Self::accept_stage_part_copy_body`]. A
    /// failure is [`StageHeadOutcome::NeedErrorBody`]: read the body and pass
    /// it to [`Self::accept_stage_part_error_body`]. A failed condition on
    /// the source is a service failure with status 412.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`].
    pub fn accept_stage_part_copy_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<StageHeadOutcome<'h>> {
        accept_stage_part_copy_head(head)
    }

    /// Finishes a [`StageHeadOutcome::NeedResultBody`] with the response
    /// body.
    ///
    /// A body that holds the result is [`StageHeadOutcome::Staged`], with
    /// the entity tag of the part. A missing upload is
    /// [`StageHeadOutcome::NotFound`] with
    /// [`ServiceErrorKind::NoSuchUpload`], and any other error a
    /// [`StageHeadOutcome::ServiceFailure`] under status 200.
    ///
    /// The body is decoded in place, and the outcome borrows it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Status`] if `head`
    /// is not a success, and with [`ResponseFault::Body`] if `body` is
    /// neither a `CopyPartResult` that holds an entity tag nor an error
    /// document.
    pub fn accept_stage_part_copy_body<'h>(
        &self,
        head: ResponseHead<'h>,
        body: &'h mut [u8],
    ) -> Result<StageHeadOutcome<'h>> {
        accept_stage_part_copy_body(head, body)
    }
}

impl Objects<'_> {
    // AWS authorizes a copy into a directory bucket by the caller's own
    // credentials, and refuses those of a session.
    fn validate_copy_credentials(&self) -> Result<()> {
        if let Token::S3Session(_) = self.credentials.token() {
            return Err(InvalidPlan::Option.into());
        }
        Ok(())
    }
}

// Whether a copy carried a condition on the target or on the source.
fn carries_condition(shape: CopyShape) -> bool {
    shape.condition != ConditionKind::None || shape.source_condition != ConditionKind::None
}

// A source bucket goes into the header as it is, so it may not hold what
// would change the header's structure. S3 copies a version, and keeps no
// snapshots.
fn validate_source(source: &CopySource<'_>) -> Result<()> {
    // A bucket of any account is named by its name alone.
    if source.endpoint.is_some() {
        return Err(InvalidPlan::CopySource.into());
    }
    if let Some(bucket) = source.container
        && (bucket.is_empty()
            || bucket
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_control()))
    {
        return Err(InvalidPlan::CopySource.into());
    }
    validate_key(source.key)?;
    validate_revision(source.revision, false)?;
    validate_condition(source.condition, source.condition_value)
}

/// One UploadPartCopy: a copy to the part `number` of the upload
/// `upload_id` of the object `copy.key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagePartCopyItem<'i> {
    /// The source, the target object, and the bytes of the source.
    pub copy: CopyItem<'i>,
    /// The ID that S3 gave the upload, never empty.
    pub upload_id: &'i str,
    /// The part's number, from 1.
    pub number: u32,
}

// The field that carries a condition on a copy's source.
const fn source_condition_header(condition: ConditionKind) -> Option<&'static str> {
    match condition {
        ConditionKind::None => None,
        ConditionKind::IfMatch => Some("x-amz-copy-source-if-match"),
        ConditionKind::IfNoneMatch => Some("x-amz-copy-source-if-none-match"),
        ConditionKind::IfModifiedSince => Some("x-amz-copy-source-if-modified-since"),
        ConditionKind::IfUnmodifiedSince => Some("x-amz-copy-source-if-unmodified-since"),
    }
}

// Writes the template of a copy, or of a copy that stages a part.
fn prepare_copy_kind(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &CopyOptions<'_>,
    staged: bool,
    buf: &mut [u8],
) -> Result<Template> {
    let service = bucket.service;
    // AWS authorizes a copy into a directory bucket by the caller's own
    // credentials, and refuses those of a session.
    if let Token::S3Session(_) = credentials.token() {
        return Err(InvalidPlan::Option.into());
    }
    // A bucket of any account is named by its name alone.
    if options.source_endpoint.is_some() {
        return Err(InvalidPlan::CopySource.into());
    }
    validate_source_container(options.source_container)?;
    if options.source_revision == RevisionKind::Snapshot {
        return Err(InvalidPlan::Revision.into());
    }
    // A part is no object: it takes a source, and nothing that an object
    // stores.
    if staged
        && (options.condition != ConditionKind::None
            || options.metadata.is_some()
            || options.tags.is_some()
            || options.options != WriteOptions::default())
    {
        return Err(InvalidPlan::Option.into());
    }
    if !staged && options.source_ranging == Ranging::Ranged {
        return Err(InvalidPlan::Range.into());
    }
    validate_metadata(options.metadata.unwrap_or(&[]), service)?;
    // A copy names its tags apart, so that none can differ from the
    // source's, and has nothing of its own to sum.
    validate_template_options(&options.options)?;
    if !options.options.tags.is_empty() {
        return Err(InvalidPlan::Option.into());
    }
    validate_tags(
        options.tags.unwrap_or(&[]),
        s3_tag_char,
        service.is_aws().then_some((10, 128, 256)),
    )?;
    validate_stored(&options.options, service)?;
    if service.is_aws() && options.condition.is_date() {
        return Err(InvalidPlan::Condition.into());
    }
    let (operation, method) = match staged {
        false => (Operation::Copy, Method::Put),
        true => (Operation::StagePartCopy, Method::Put),
    };
    let mut request = Request::new(
        operation,
        method,
        Detail::Copy {
            condition: options.condition,
            source_revision: options.source_revision,
            source_condition: options.source_condition,
            source_ranging: options.source_ranging,
        },
    );
    if staged {
        request.query[0] = Some(("partNumber", ParamValue::Hole(Hole::PartNumber)));
        request.query[1] = Some(("uploadId", ParamValue::Hole(Hole::UploadId)));
    }
    request.signed.push(Field::new(
        "x-amz-copy-source",
        Value::CopySource {
            bucket: options.source_container.unwrap_or(bucket.name),
            revision: options.source_revision,
        },
    ));
    if let Some(name) = source_condition_header(options.source_condition) {
        request.signed.push(Field::new(
            name,
            Value::Hole(Hole::SourceCondition, Hole::SignedSourceCondition),
        ));
    }
    if options.source_ranging == Ranging::Ranged {
        request.signed.push(Field::new(
            "x-amz-copy-source-range",
            Value::Hole(Hole::SourceRange, Hole::SourceRange),
        ));
    }
    push_stored(&mut request.signed, &options.options);
    // S3 keeps the source's metadata and content properties unless told to
    // replace them, and its tags likewise. Replacing the tags with none
    // sends an empty `x-amz-tagging`.
    if options.metadata.is_some() || !options.options.properties.is_empty() {
        request.signed.push(Field::new(
            "x-amz-metadata-directive",
            Value::Formatted(b"REPLACE"),
        ));
    }
    if let Some(tags) = options.tags {
        request.signed.push(Field::new(
            "x-amz-tagging-directive",
            Value::Formatted(b"REPLACE"),
        ));
        request
            .signed
            .push(Field::new("x-amz-tagging", Value::Tags(tags)));
    }
    request.condition(options.condition);
    request.metadata = options.metadata.unwrap_or(&[]);
    request
        .unsigned
        .push(Field::new("content-length", Value::Formatted(b"0")));
    prepare_request(bucket, credentials, &request, buf)
}

// Writes the copy `item`, to the part `part` of an upload when it stages
// one.
fn encode_copy_kind(
    template: &Template,
    bytes: &[u8],
    operation: Operation,
    item: &CopyItem<'_>,
    part: Option<(&str, u32)>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    let Detail::Copy {
        condition,
        source_revision,
        source_condition,
        source_ranging,
    } = template.detail
    else {
        return Err(InvalidPlan::Template.into());
    };
    if template.operation != operation {
        return Err(InvalidPlan::Template.into());
    }
    validate_key(item.key)?;
    validate_key(item.source_key)?;
    if let Some((upload_id, number)) = part {
        super::parts::validate_upload_id(upload_id)?;
        super::parts::validate_part_number(number, template.service)?;
    }
    validate_revision_item(source_revision, item.source_revision)?;
    validate_write_condition(condition, item.condition_value, template.service)?;
    validate_condition(source_condition, item.source_condition_value)?;
    crate::common::validate_source_range(source_ranging, item.source_range)?;
    let values = S3Values(Values {
        now: Some(now),
        key: Some(item.key),
        condition: item.condition_value,
        source_key: Some(item.source_key),
        source_revision: item.source_revision,
        source_condition: item.source_condition_value,
        source_range: Some(item.source_range),
        upload_id: part.map(|(upload_id, _)| upload_id),
        part_number: part.map(|(_, number)| number),
        ..Values::default()
    });
    encode_template(template, bytes, &values, Payload::Slice(&[]), buf)
}

/// Writes the template of CopyObjects with `options` into `buf`, signed with
/// `credentials`.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for options that S3 refuses:
/// [`InvalidPlan::Option`] for the credentials of a session, which AWS
/// refuses for a copy, for a checksum, and for tags in
/// [`CopyOptions::options`]; [`InvalidPlan::CopySource`] for a source
/// endpoint, or a source bucket that is empty or holds `/`, `?` or `#`;
/// [`InvalidPlan::Revision`] for a snapshot; [`InvalidPlan::Condition`] for
/// a date condition on the target, on AWS; and the refusals of
/// [`prepare_put`](crate::s3::prepare_put) for metadata, tags and stored
/// options.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_copy(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &CopyOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    prepare_copy_kind(bucket, credentials, options, false, buf)
}

/// Writes the CopyObject `item` at `now` from `template` into `buf`, ready
/// to sign.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key of the target or the source
/// that S3 refuses, see [Object keys](crate#object-keys), for a version or a
/// condition value that the template does not take, and for a template of
/// another operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_copy(
    template: &Template,
    bytes: &[u8],
    item: &CopyItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    encode_copy_kind(template, bytes, Operation::Copy, item, None, now, buf)
}

/// Writes the template of UploadPartCopies with `options` into `buf`,
/// signed with `credentials`.
///
/// # Errors
///
/// As [`prepare_copy`]. A part stores nothing of its own, so options beyond
/// the source's are [`InvalidPlan::Option`].
pub fn prepare_stage_part_copy(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &CopyOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    prepare_copy_kind(bucket, credentials, options, true, buf)
}

/// Writes the UploadPartCopy `item` at `now` from `template` into `buf`,
/// ready to sign.
///
/// # Errors
///
/// As [`encode_copy`], with [`InvalidPlan::UploadId`] for an empty upload
/// ID, [`InvalidPlan::PartId`] for a part number that S3 refuses, and a
/// range of the source that is empty, or that is not bounded where the
/// template names one.
pub fn encode_stage_part_copy(
    template: &Template,
    bytes: &[u8],
    item: &StagePartCopyItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    encode_copy_kind(
        template,
        bytes,
        Operation::StagePartCopy,
        &item.copy,
        Some((item.upload_id, item.number)),
        now,
        buf,
    )
}

/// Reads the response head of a CopyObject and reports what to do next. S3
/// answers with 200 before it has finished, and writes the result into the
/// body: read it with [`accept_copy_body`].
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200, and a failed condition on a copy that carried none, are
/// [`ResponseFault::Status`].
pub fn accept_copy_head<'h>(
    shape: CopyShape,
    head: ResponseHead<'h>,
) -> Result<CopyHeadOutcome<'h>> {
    match head.status {
        200 => Ok(CopyHeadOutcome::NeedResultBody {
            expected_len: decimal_header(head.content_length)?,
        }),
        412 if !carries_condition(shape) => Err(ResponseFault::Status.into()),
        412 => Ok(CopyHeadOutcome::PreconditionFailed),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Reads the result of a CopyObject out of its body, which may still be an
/// error.
///
/// # Errors
///
/// Returns [`Error::Response`] for a status other than 200, and for a body
/// that is neither a result nor an error document.
pub fn accept_copy_body<'h>(
    shape: CopyShape,
    head: ResponseHead<'h>,
    body: &'h mut [u8],
) -> Result<CopyHeadOutcome<'h>> {
    if head.status != 200 {
        return Err(ResponseFault::Status.into());
    }
    if is_error_document(body) {
        let kind = body_kind(body);
        return Ok(match kind {
            Some(ServiceErrorKind::Precondition) if carries_condition(shape) => {
                CopyHeadOutcome::PreconditionFailed
            }
            Some(ServiceErrorKind::NotFound) => CopyHeadOutcome::NotFound { kind },
            _ => finish_with_body(failure(head.status, None, head.request_id), kind),
        });
    }
    let e_tag = crate::xml::s3::copy::read_copied(body)?;
    Ok(CopyHeadOutcome::Copied {
        meta: ObjectMeta {
            e_tag: Some(e_tag.as_bytes()),
            version: head.version,
            ..ObjectMeta::default()
        },
    })
}

/// Finishes a [`CopyHeadOutcome::NeedErrorBody`] with the response body. S3
/// reports a failed condition in the head.
pub fn accept_copy_error_body<'h>(
    shape: CopyShape,
    failure: Failure<'h>,
    body: &[u8],
) -> CopyHeadOutcome<'h> {
    let _ = shape;
    finish_with_body(failure, body_kind(body))
}

/// Reads the response head of an UploadPartCopy and reports what to do
/// next: S3 writes the result into the body, which
/// [`accept_stage_part_copy_body`] reads.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_stage_part_copy_head<'h>(head: ResponseHead<'h>) -> Result<StageHeadOutcome<'h>> {
    match head.status {
        200 => Ok(StageHeadOutcome::NeedResultBody {
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Reads the result of an UploadPartCopy out of its body, which may still be
/// an error.
///
/// # Errors
///
/// Returns [`Error::Response`] for a status other than 200, and for a body
/// that is neither a result nor an error document.
pub fn accept_stage_part_copy_body<'h>(
    head: ResponseHead<'h>,
    body: &'h mut [u8],
) -> Result<StageHeadOutcome<'h>> {
    if head.status != 200 {
        return Err(ResponseFault::Status.into());
    }
    if is_error_document(body) {
        let kind = body_kind(body);
        return Ok(match kind {
            Some(ServiceErrorKind::NoSuchUpload) => StageHeadOutcome::NotFound { kind },
            _ => finish_with_body(failure(head.status, None, head.request_id), kind),
        });
    }
    let e_tag = crate::xml::s3::copy::read_part_copied(body)?;
    Ok(StageHeadOutcome::Staged {
        e_tag: Some(e_tag.as_bytes()),
    })
}
