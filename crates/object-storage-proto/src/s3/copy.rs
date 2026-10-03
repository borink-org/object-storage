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
use crate::request::HeadWriter;
use crate::s3::{
    Objects, PayloadHash, Service, Signed, SignedCopy, Stores, body_kind, is_error_document,
    s3_tag_char, stored_headers, validate_content, validate_key, validate_metadata,
    validate_write_condition,
};
use crate::sigv4::EMPTY_SHA256;
use crate::url;
use crate::{
    ConditionKind, CopyHeadOutcome, CopyShape, CopySource, Failure, HeaderSpan, InvalidPlan,
    Method, ObjectMeta, Payload, PhysicalCopy, RequestedRange, ResponseFault, ResponseHead, Result,
    ServiceErrorKind, StageHeadOutcome, Timestamps, WireRequest,
};

/// One UploadPartCopy: a part of an upload staged from the bytes of another
/// object, which S3 reads itself.
///
/// Commit the part as you commit one that
/// [`Objects::encode_stage_part`](crate::s3::Objects::encode_stage_part)
/// staged, by its number and the entity tag that the result carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalStagePartCopy<'a> {
    /// The key of the upload, under the rules of
    /// [`PhysicalGet::key`](crate::PhysicalGet::key).
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
    ///   or a condition on the target that [`Self::encode_put`] refuses.
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
    ) -> Result<WireRequest<'r>> {
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
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
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
        match head.status {
            200 => Ok(CopyHeadOutcome::NeedResultBody {
                expected_len: decimal_header(head.content_length)?,
            }),
            412 if !carries_condition(shape) => Err(ResponseFault::Status.into()),
            412 => Ok(CopyHeadOutcome::PreconditionFailed),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(CopyHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
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

    /// Finishes a [`CopyHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_get_error_body`] for a copy, and reads the body
    /// the same way. A source that does not exist is
    /// [`CopyHeadOutcome::NotFound`].
    pub fn accept_copy_error_body<'h>(
        &self,
        shape: CopyShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> CopyHeadOutcome<'h> {
        // S3 reports a failed condition in the head.
        let _ = shape;
        finish_with_body(failure, body_kind(body))
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
    ) -> Result<WireRequest<'r>> {
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
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
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
        match head.status {
            200 => Ok(StageHeadOutcome::NeedResultBody {
                expected_len: decimal_header(head.content_length)?,
            }),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(StageHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
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
}

impl Objects<'_> {
    // AWS authorizes a copy into a directory bucket by the caller's own
    // credentials, and refuses those of a session.
    fn validate_copy_credentials(&self) -> Result<()> {
        if self.credentials.is_s3_session() {
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
