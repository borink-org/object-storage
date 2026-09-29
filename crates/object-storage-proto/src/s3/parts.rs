// S3 uploads in parts: CreateMultipartUpload, UploadPart,
// CompleteMultipartUpload, AbortMultipartUpload and ListParts.
//
// An upload is S3's form of what Azure calls writing in blocks, and it takes
// the same plan for the commit, `PhysicalCommit`, and answers with the same
// outcomes for the stage, the commit and the listing. What differs is how a
// part is named. Azure names a block by an ID that the caller chooses. S3
// names a part by its number within an upload that S3 created, and by the
// entity tag that S3 returned when it staged the part.

use super::{
    Objects, PayloadHash, Service, Signed, body_kind, validate_content, validate_key,
    validate_metadata, validate_write_condition,
};
use crate::common::{
    FailureOutcome, decimal_header, encoded, encoded_with_body, failure, finish_with_body,
    push_checksum, valid_header,
};
use crate::encoding;
use crate::request::{ByteSink, HeadWriter, U64Decimal, Writer};
use crate::sigv4::EMPTY_SHA256;
use crate::url::QueryValue;
use crate::{
    CommitHeadOutcome, CommitShape, ConditionKind, DeleteHeadOutcome, Failure, HeaderSpan,
    InvalidPlan, ListPartsHeadOutcome, Listing, MetadataPair, Method, ObjectMeta, Payload,
    PhysicalCommit, RequestedRange, ResponseFault, ResponseHead, Result, ServiceErrorKind,
    StageHeadOutcome, Timestamps, WireRequest, WriteOptions,
};

/// The most bytes that S3 takes in one part.
///
/// [`Objects::encode_stage_part`] refuses a longer payload with
/// [`InvalidPlan::PayloadTooLarge`].
pub const MAX_PART_LEN: u64 = 5 * 1024 * 1024 * 1024;

/// The fewest bytes that AWS takes in a part other than the last.
///
/// AWS checks this when it commits, and refuses a commit whose parts before
/// the last are shorter with `EntityTooSmall`, which reaches you as
/// [`ServiceErrorKind::InvalidUpload`]. This crate does not check it: a
/// stage does not know whether its part will be the last.
pub const MIN_PART_LEN: u64 = 5 * 1024 * 1024;

/// The highest part number that AWS takes, which is also the most parts of
/// one upload.
///
/// A client of [`Service::Aws`] or [`Service::AwsDirectory`] refuses a
/// higher number with [`InvalidPlan::PartId`].
pub const MAX_PARTS: u32 = 10_000;

/// A CreateMultipartUpload plan: start an upload of an object in parts.
///
/// S3 answers with the ID of the upload, which every later request of the
/// upload names. Read it with [`Objects::read_upload_id`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalCreateUpload<'a> {
    /// The object key, under the rules of
    /// [`PhysicalGet::key`](crate::PhysicalGet::key).
    pub key: &'a str,
    /// The metadata pairs to store with the object that the upload commits.
    ///
    /// The rules of [`PhysicalPut::metadata`](crate::PhysicalPut::metadata)
    /// apply. An S3 commit takes no metadata of its own.
    pub metadata: &'a [MetadataPair<'a>],
}

impl<'a> PhysicalCreateUpload<'a> {
    /// Creates a plan that starts an upload to `key`, with no metadata.
    pub const fn new(key: &'a str) -> Self {
        Self { key, metadata: &[] }
    }
}

/// An UploadPart plan: stage one part of an upload.
///
/// Staging a number again replaces the part that the number held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalStagePart<'a> {
    /// The object key that the upload was created for.
    pub key: &'a str,
    /// The ID of the upload, as [`Objects::read_upload_id`] read it.
    pub upload_id: &'a str,
    /// The number of the part, from 1. The commit orders the parts by it.
    ///
    /// AWS takes the numbers up to [`MAX_PARTS`]. The numbers of an upload
    /// need not be consecutive.
    pub number: u32,
    /// The options of the stage, such as an MD5 of the part.
    pub options: WriteOptions<'a>,
}

impl<'a> PhysicalStagePart<'a> {
    /// Creates a plan that stages part `number` of an upload, with no
    /// options.
    pub const fn new(key: &'a str, upload_id: &'a str, number: u32) -> Self {
        Self {
            key,
            upload_id,
            number,
            options: WriteOptions::new(),
        }
    }
}

/// One entry of the list of parts that a commit publishes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartRef<'a> {
    /// The number the part was staged under.
    pub number: u32,
    /// The entity tag of the part, with its quotes, as
    /// [`StageHeadOutcome::Staged`] or [`Part::e_tag`] gave it.
    pub e_tag: &'a [u8],
}

impl<'b> From<Part<'b>> for PartRef<'b> {
    fn from(part: Part<'b>) -> Self {
        Self {
            number: part.number,
            e_tag: part.e_tag.as_bytes(),
        }
    }
}

/// An AbortMultipartUpload plan: end an upload without an object, and drop
/// the parts it holds.
///
/// An upload that is neither committed nor aborted keeps its parts, and AWS
/// bills them as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalAbortUpload<'a> {
    /// The object key that the upload was created for.
    pub key: &'a str,
    /// The ID of the upload.
    pub upload_id: &'a str,
}

impl<'a> PhysicalAbortUpload<'a> {
    /// Creates a plan that aborts the upload `upload_id` to `key`.
    pub const fn new(key: &'a str, upload_id: &'a str) -> Self {
        Self { key, upload_id }
    }
}

/// A ListParts plan: read one page of the parts that an upload holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalListParts<'a> {
    /// The object key that the upload was created for.
    pub key: &'a str,
    /// The ID of the upload.
    pub upload_id: &'a str,
    /// Where the previous page ended.
    ///
    /// Pass the [`Listing::next_marker`] that the previous page reported. The
    /// first page carries [`None`].
    pub marker: Option<&'a str>,
    /// The most parts that this page reports.
    ///
    /// [`None`] asks for the service's maximum, which is 1,000 on AWS.
    pub max_parts: Option<u32>,
}

impl<'a> PhysicalListParts<'a> {
    /// Creates a plan for the first page of the parts of an upload.
    pub const fn new(key: &'a str, upload_id: &'a str) -> Self {
        Self {
            key,
            upload_id,
            marker: None,
            max_parts: None,
        }
    }
}

/// One part of an upload, borrowing the response body of a ListParts after
/// in-place decoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Part<'b> {
    /// The number the part was staged under.
    pub number: u32,
    /// The entity tag of the part, with its quotes.
    pub e_tag: &'b str,
    /// The length of the part.
    pub size: u64,
    /// When the part was staged, in ISO 8601, if the listing says. Read it
    /// with [`layered::iso8601_ms`](crate::layered::iso8601_ms).
    pub last_modified: Option<&'b str>,
}

/// The result of reading the response head of a CreateMultipartUpload.
///
/// A head that reports a failure is one of these too.
/// [`Objects::accept_create_upload_head`] returns an [`Err`] only for a head
/// it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CreateUploadHeadOutcome<'h> {
    /// The upload exists, and its ID follows in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`Objects::read_upload_id`].
    Created {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// The bucket does not exist.
    NotFound {
        /// The specific error, if the body names one.
        kind: Option<ServiceErrorKind>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
    /// [`Objects::accept_create_upload_error_body`], which returns the final
    /// outcome. If you cannot read the body, pass an empty one and the error
    /// stays unnamed.
    NeedErrorBody(Failure<'h>),
    /// The service refused to create the upload, or it failed to.
    ServiceFailure(Failure<'h>),
}

impl<'h> FailureOutcome<'h> for CreateUploadHeadOutcome<'h> {
    fn not_found(kind: Option<ServiceErrorKind>) -> Self {
        Self::NotFound { kind }
    }

    fn service_failure(failure: Failure<'h>) -> Self {
        Self::ServiceFailure(failure)
    }
}

impl core::fmt::Display for CreateUploadHeadOutcome<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Created { .. } => f.write_str("the upload ID follows in the response body"),
            Self::NotFound { .. } => f.write_str(ServiceErrorKind::NoSuchContainer.as_str()),
            Self::NeedErrorBody(failure) | Self::ServiceFailure(failure) => {
                core::fmt::Display::fmt(failure, f)
            }
        }
    }
}

impl<'a> Objects<'a> {
    /// Writes the signed request head of a CreateMultipartUpload into `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, for the reasons of the key and the
    /// metadata that [`Self::encode_put`] states.
    ///
    /// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` or
    /// `headers` is too small, with the required bytes and header slots, or
    /// call [`layered::s3::create_upload_requirements`](crate::layered::s3::create_upload_requirements)
    /// first.
    pub fn encode_create_upload<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCreateUpload<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key)?;
        validate_metadata(plan.metadata, self.bucket.service)?;
        let signed = Signed {
            method: Method::Post,
            key: Some(plan.key),
            query: &[Some(("uploads", QueryValue::Literal("")))],
            headers: &[],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: plan.metadata,
            content_sha256: EMPTY_SHA256.as_bytes(),
        };
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        // A POST states its length even when it has no content.
        head.header("content-length", |out| out.push(b"0"));
        encoded(head, Method::Post, Payload::Slice(&[]))
    }

    /// Reads the response head of a CreateMultipartUpload and reports what
    /// to do next.
    ///
    /// A failure is [`CreateUploadHeadOutcome::NeedErrorBody`]: read the
    /// body and pass it to [`Self::accept_create_upload_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`](crate::Error::Response) if the head cannot
    /// be read. A success status other than 200 is [`ResponseFault::Status`],
    /// and a `Content-Length` that is not a number is
    /// [`ResponseFault::Head`].
    pub fn accept_create_upload_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<CreateUploadHeadOutcome<'h>> {
        match head.status {
            200 => Ok(CreateUploadHeadOutcome::Created {
                expected_len: decimal_header(head.content_length)?,
            }),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(CreateUploadHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`CreateUploadHeadOutcome::NeedErrorBody`] with the
    /// response body.
    ///
    /// This is [`Self::accept_get_error_body`] for a CreateMultipartUpload,
    /// and reads the body the same way. A missing bucket is
    /// [`CreateUploadHeadOutcome::NotFound`].
    pub fn accept_create_upload_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> CreateUploadHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Reads the ID of the upload out of the response body of a
    /// CreateMultipartUpload.
    ///
    /// The body is decoded in place, and the ID borrows it. Copy it into
    /// your own storage: every later request of the upload names it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`](crate::Error::Response) with
    /// [`ResponseFault::Body`] if `body` is not an
    /// `InitiateMultipartUploadResult` that holds an upload ID.
    pub fn read_upload_id<'b>(&self, body: &'b mut [u8]) -> Result<&'b str> {
        crate::xml::s3::read_upload_id(body)
    }

    /// Writes the signed request head of an UploadPart into `buf`.
    ///
    /// The head states the length of `content`, which stays where you put
    /// it, and `hash` says how the request signs it, as for
    /// [`Self::encode_put`]. Keep the entity tag that the answer carries: the
    /// commit names the part by it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request:
    ///
    /// - [`InvalidPlan::UploadId`] for an empty upload ID.
    /// - [`InvalidPlan::PartId`] for part number 0, or one above
    ///   [`MAX_PARTS`] for AWS.
    /// - [`InvalidPlan::PayloadTooLarge`] if `content` is longer than
    ///   [`MAX_PART_LEN`].
    /// - [`InvalidPlan::Option`] for the options and the hash that
    ///   [`Self::encode_put`] refuses.
    ///
    /// Returns [`Error::Capacity`](crate::Error::Capacity) as
    /// [`Self::encode_put`] does, or call
    /// [`layered::s3::stage_part_requirements`](crate::layered::s3::stage_part_requirements)
    /// first.
    pub fn encode_stage_part<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalStagePart<'_>,
        content: Payload<'r>,
        hash: PayloadHash,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key)?;
        validate_upload_id(plan.upload_id)?;
        validate_part_number(plan.number, self.bucket.service)?;
        if content.len() > MAX_PART_LEN {
            return Err(InvalidPlan::PayloadTooLarge.into());
        }
        validate_content(&plan.options, content, hash, &self.checksums)?;
        let dry = buf.is_empty();
        let content_sha256 = self.content_sha256(hash, content, dry);
        let query = [
            Some(("partNumber", QueryValue::Number(plan.number))),
            Some(("uploadId", QueryValue::Encoded(plan.upload_id.as_bytes()))),
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
            content_sha256: content_sha256.as_bytes(),
        };
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        self.push_content(&mut head, content, plan.options.checksum);
        encoded(head, Method::Put, content)
    }

    /// Reads the response head of an UploadPart and reports what S3 did.
    ///
    /// A failure is [`StageHeadOutcome::NeedErrorBody`]: read the body and
    /// pass it to [`Self::accept_stage_part_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`](crate::Error::Response) if the head cannot
    /// be read. A success status other than 200 is [`ResponseFault::Status`],
    /// and a success without an `ETag` is [`ResponseFault::Head`]: a commit
    /// could not name the part.
    pub fn accept_stage_part_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<StageHeadOutcome<'h>> {
        match head.status {
            200 if head.e_tag.is_none() => Err(ResponseFault::Head.into()),
            200 => Ok(StageHeadOutcome::Staged { e_tag: head.e_tag }),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(StageHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`StageHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`Self::accept_get_error_body`] for an UploadPart, and reads
    /// the body the same way. A missing upload is
    /// [`StageHeadOutcome::NotFound`] with
    /// [`ServiceErrorKind::NoSuchUpload`].
    pub fn accept_stage_part_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> StageHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Writes a CompleteMultipartUpload into `buf`: the signed head, then the
    /// XML body after it.
    ///
    /// `parts` become the object in this order. The body is written into
    /// `buf` after the head, so [`WireRequest::body_span`] names it and
    /// [`WireRequest::payload`] borrows it; send both before reusing `buf`.
    /// The request signs the SHA-256 of the body.
    ///
    /// The commit ends the upload. The parts it does not name are dropped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if the
    /// commit cannot become an S3 request:
    ///
    /// - [`InvalidPlan::UploadId`] for an empty `upload_id`.
    /// - [`InvalidPlan::Option`] if the plan carries metadata, which S3 takes
    ///   when it creates the upload, or a declared MD5, or a checksum other
    ///   than an MD5.
    /// - [`InvalidPlan::Condition`] for the conditions that
    ///   [`Self::encode_put`] refuses.
    /// - [`InvalidPlan::PartId`] for a part number that
    ///   [`Self::encode_stage_part`] refuses, or an entity tag that is not
    ///   one header value.
    /// - [`InvalidPlan::Parts`] if `parts` is empty, or for AWS, if their
    ///   numbers do not ascend. AWS refuses a list out of order with
    ///   `InvalidPartOrder`.
    ///
    /// Returns [`Error::Capacity`](crate::Error::Capacity) with the bytes
    /// that the head and the body need together, or call
    /// [`layered::s3::commit_parts_requirements`](crate::layered::s3::commit_parts_requirements)
    /// first.
    pub fn encode_commit_parts<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCommit<'_>,
        upload_id: &str,
        parts: &[PartRef<'_>],
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        self.encode_commit_parts_from_iter(
            buf,
            headers,
            plan,
            upload_id,
            parts.iter().map(|part| (part.number, part.e_tag)),
            now,
        )
    }

    /// [`Self::encode_commit_parts`] over parts that are not in one array.
    ///
    /// This is
    /// [`Blobs::encode_commit_blocks_from_iter`](crate::Blobs::encode_commit_blocks_from_iter)
    /// for S3. `parts` yields each part's number and entity tag. It is
    /// traversed more than once, and every traversal must yield the same
    /// items in the same order.
    ///
    /// # Errors
    ///
    /// As [`Self::encode_commit_parts`].
    pub fn encode_commit_parts_from_iter<'r, E>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCommit<'_>,
        upload_id: &str,
        parts: impl Iterator<Item = (u32, E)> + Clone,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>>
    where
        E: AsRef<[u8]>,
    {
        let service = self.bucket.service;
        validate_key(plan.key)?;
        validate_upload_id(upload_id)?;
        // The body is the encoder's own, so it always holds the bytes.
        validate_content(
            &plan.options,
            Payload::Slice(&[]),
            PayloadHash::Compute,
            &self.checksums,
        )?;
        if !plan.metadata.is_empty() {
            return Err(InvalidPlan::Option.into());
        }
        validate_write_condition(plan.condition, plan.condition_value, service)?;
        validate_parts(parts.clone(), service)?;

        let mut counted = Writer::new(&mut []);
        write_part_list(&mut counted, parts.clone());
        let length = counted.position();
        let dry = buf.is_empty();
        let content_sha256 = if dry {
            [b'0'; 64]
        } else {
            let mut sum = self.sha256.start();
            write_part_list(&mut sum, parts.clone());
            encoding::hex(&sum.finish())
        };
        let query = [Some((
            "uploadId",
            QueryValue::Encoded(upload_id.as_bytes()),
        ))];
        let signed = Signed {
            method: Method::Post,
            key: Some(plan.key),
            query: &query,
            headers: &[],
            range: RequestedRange::Whole,
            condition: plan.condition,
            condition_value: plan.condition_value,
            metadata: &[],
            content_sha256: &content_sha256,
        };
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        head.header("content-length", |out| {
            out.push(U64Decimal::new(length as u64).as_bytes());
        });
        push_checksum(&mut head, plan.options.checksum, &self.checksums, |sum| {
            write_part_list(sum, parts.clone());
        });
        encoded_with_body(head, Method::Post, |out| write_part_list(out, parts))
    }

    /// Reads the response head of a CompleteMultipartUpload and reports
    /// what to do next.
    ///
    /// Pass the `shape` that [`PhysicalCommit::shape`] gave you before the
    /// request. S3 answers a commit that it takes with status 200 before it
    /// has finished, and says in the body whether it succeeded. So a success
    /// is [`CommitHeadOutcome::NeedResultBody`]: read the body and pass it to
    /// [`Self::accept_commit_parts_body`]. A failure is
    /// [`CommitHeadOutcome::NeedErrorBody`]: read the body and pass it to
    /// [`Self::accept_commit_parts_error_body`].
    ///
    /// A conditional commit that another conditional write overtook is
    /// refused with 409 `ConditionalRequestConflict`, which reaches you as a
    /// service failure. Retry it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`](crate::Error::Response) if the head cannot
    /// be read. A success status other than 200, and a failed condition on a
    /// commit that carried none, are both [`ResponseFault::Status`].
    pub fn accept_commit_parts_head<'h>(
        &self,
        shape: CommitShape,
        head: ResponseHead<'h>,
    ) -> Result<CommitHeadOutcome<'h>> {
        match head.status {
            200 => Ok(CommitHeadOutcome::NeedResultBody {
                expected_len: decimal_header(head.content_length)?,
            }),
            412 if shape.condition == ConditionKind::None => Err(ResponseFault::Status.into()),
            412 => Ok(CommitHeadOutcome::PreconditionFailed),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(CommitHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`CommitHeadOutcome::NeedResultBody`] with the response
    /// body.
    ///
    /// Pass the `shape` of the commit, the head that
    /// [`Self::accept_commit_parts_head`] read, and the whole body. A body
    /// that holds the result is [`CommitHeadOutcome::Committed`], whose
    /// entity tag the body carries and whose version the head does.
    ///
    /// A body that holds an error is the outcome that the error has under
    /// its own status. A missing upload is [`CommitHeadOutcome::NotFound`]
    /// with [`ServiceErrorKind::NoSuchUpload`], and a failed condition is
    /// [`CommitHeadOutcome::PreconditionFailed`]. Any other error is a
    /// [`CommitHeadOutcome::ServiceFailure`] under status 200, such as one
    /// with [`ServiceErrorKind::Service`], which a retry can fix.
    ///
    /// The body is decoded in place. The outcome borrows both `body` and
    /// `head`, so `head` must not borrow the buffer that holds the body. If
    /// you read the whole response into one buffer, split it where the body
    /// starts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`](crate::Error::Response) with
    /// [`ResponseFault::Status`] if `head` is not a success, and with
    /// [`ResponseFault::Body`] if `body` is neither a
    /// `CompleteMultipartUploadResult` that holds an entity tag nor an error
    /// document.
    pub fn accept_commit_parts_body<'h>(
        &self,
        shape: CommitShape,
        head: ResponseHead<'h>,
        body: &'h mut [u8],
    ) -> Result<CommitHeadOutcome<'h>> {
        if head.status != 200 {
            return Err(ResponseFault::Status.into());
        }
        // A result holds no `Code` element, and an error document does. A
        // directory bucket reports under status 200 what a general purpose
        // bucket reports under 404 or 412, so those become the same outcomes.
        if crate::xml::error_code(body).is_some() {
            let kind = body_kind(body);
            return Ok(match kind {
                Some(ServiceErrorKind::Precondition) if shape.condition != ConditionKind::None => {
                    CommitHeadOutcome::PreconditionFailed
                }
                Some(ServiceErrorKind::NoSuchUpload) => CommitHeadOutcome::NotFound { kind },
                _ => finish_with_body(failure(head.status, None, head.request_id), kind),
            });
        }
        let e_tag = crate::xml::s3::read_committed(body)?;
        Ok(CommitHeadOutcome::Committed {
            meta: ObjectMeta {
                e_tag: Some(e_tag.as_bytes()),
                version: head.version,
                ..ObjectMeta::default()
            },
        })
    }

    /// Finishes a [`CommitHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`Self::accept_get_error_body`] for a commit, and reads the
    /// body the same way. A missing upload is [`CommitHeadOutcome::NotFound`]
    /// with [`ServiceErrorKind::NoSuchUpload`], and a list of parts that S3
    /// cannot commit is a service failure with
    /// [`ServiceErrorKind::InvalidUpload`].
    pub fn accept_commit_parts_error_body<'h>(
        &self,
        shape: CommitShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> CommitHeadOutcome<'h> {
        // S3 reports a failed condition in the head.
        let _ = shape;
        finish_with_body(failure, body_kind(body))
    }

    /// Writes the signed request head of an AbortMultipartUpload into `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, such as [`InvalidPlan::UploadId`] for an
    /// empty upload ID.
    ///
    /// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` or
    /// `headers` is too small, or call
    /// [`layered::s3::abort_upload_requirements`](crate::layered::s3::abort_upload_requirements)
    /// first.
    pub fn encode_abort_upload<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalAbortUpload<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key)?;
        validate_upload_id(plan.upload_id)?;
        let query = [Some((
            "uploadId",
            QueryValue::Encoded(plan.upload_id.as_bytes()),
        ))];
        let signed = Signed {
            method: Method::Delete,
            key: Some(plan.key),
            query: &query,
            headers: &[],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
        };
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        encoded(head, Method::Delete, Payload::Slice(&[]))
    }

    /// Reads the response head of an AbortMultipartUpload and reports what
    /// S3 did.
    ///
    /// A failure is [`DeleteHeadOutcome::NeedErrorBody`]: read the body and
    /// pass it to [`Self::accept_abort_upload_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`](crate::Error::Response) if the head cannot
    /// be read. A success status other than 204 is [`ResponseFault::Status`].
    /// A [`Service::Compatible`] client also takes 200 as success.
    pub fn accept_abort_upload_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<DeleteHeadOutcome<'h>> {
        let accepts_200 = match self.bucket.service {
            Service::Compatible => true,
            Service::Aws | Service::AwsDirectory => false,
        };
        match head.status {
            204 => Ok(DeleteHeadOutcome::Accepted),
            200 if accepts_200 => Ok(DeleteHeadOutcome::Accepted),
            200..=299 => Err(ResponseFault::Status.into()),
            status => Ok(DeleteHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`DeleteHeadOutcome::NeedErrorBody`] of an abort with the
    /// response body.
    ///
    /// This is [`Self::accept_get_error_body`] for an abort, and reads the
    /// body the same way. An upload that is gone is
    /// [`DeleteHeadOutcome::NotFound`] with
    /// [`ServiceErrorKind::NoSuchUpload`].
    pub fn accept_abort_upload_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> DeleteHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Writes the signed request head for one page of a ListParts into
    /// `buf`.
    ///
    /// Read the whole response body and pass it to [`Self::fill_parts`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request: [`InvalidPlan::UploadId`] for an empty
    /// upload ID, and [`InvalidPlan::Marker`] for an empty marker.
    ///
    /// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` or
    /// `headers` is too small, or call
    /// [`layered::s3::list_parts_requirements`](crate::layered::s3::list_parts_requirements)
    /// first.
    pub fn encode_list_parts<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalListParts<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key)?;
        validate_upload_id(plan.upload_id)?;
        if plan.marker.is_some_and(str::is_empty) {
            return Err(InvalidPlan::Marker.into());
        }
        // SigV4 signs the parameters in the order of their names.
        let query = [
            plan.max_parts
                .map(|max| ("max-parts", QueryValue::Number(max))),
            plan.marker
                .map(|marker| ("part-number-marker", QueryValue::Encoded(marker.as_bytes()))),
            Some(("uploadId", QueryValue::Encoded(plan.upload_id.as_bytes()))),
        ];
        let signed = Signed {
            method: Method::Get,
            key: Some(plan.key),
            query: &query,
            headers: &[],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
        };
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        encoded(head, Method::Get, Payload::Slice(&[]))
    }

    /// Reads the response head of a ListParts and reports what S3 did.
    ///
    /// S3 may send the page without `Content-Length`, so cap what you read.
    /// A failure is [`ListPartsHeadOutcome::NeedErrorBody`]: read the body
    /// and pass it to [`Self::accept_list_parts_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`](crate::Error::Response) if the head cannot
    /// be read. A success status other than 200 is [`ResponseFault::Status`],
    /// and a `Content-Length` that is not a number is
    /// [`ResponseFault::Head`].
    pub fn accept_list_parts_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<ListPartsHeadOutcome<'h>> {
        match head.status {
            200 => Ok(ListPartsHeadOutcome::Parts {
                meta: ObjectMeta::default(),
                expected_len: decimal_header(head.content_length)?,
            }),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(ListPartsHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`ListPartsHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`Self::accept_get_error_body`] for a ListParts, and reads the
    /// body the same way. A missing upload is
    /// [`ListPartsHeadOutcome::NotFound`] with
    /// [`ServiceErrorKind::NoSuchUpload`].
    pub fn accept_list_parts_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> ListPartsHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Reads a page of parts out of the response body of a ListParts.
    ///
    /// This is [`Blobs::fill_blocks`](crate::Blobs::fill_blocks) for S3. The
    /// parts are read in the order S3 wrote them, which is the order of
    /// their numbers. Reading is destructive, and your array must hold the
    /// whole page. An array of `max_parts` entries always does, and so does
    /// one of 1,000 for AWS. Pass a listed part to a commit as a
    /// [`PartRef`], which converts from it.
    ///
    /// Pass the page's [`Listing::next_marker`] as the marker of the next
    /// plan. It is [`None`] on the last page.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`](crate::Error::Capacity) if the page holds
    /// more parts than the array, with `required` set to the number it
    /// holds. Ask the service for the page again, with a larger array.
    ///
    /// Returns [`Error::Response`](crate::Error::Response) with
    /// [`ResponseFault::Body`] if `body` is not a `ListPartsResult`, or if
    /// the page does not say whether it is the last.
    pub fn fill_parts<'b, E: From<Part<'b>>>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
    ) -> Result<Listing<'b>> {
        crate::xml::s3::fill_parts(body, into)
    }
}

// Part number 0 is not a part anywhere. AWS numbers parts up to
// `MAX_PARTS`.
fn validate_part_number(number: u32, service: Service) -> Result<()> {
    let limited = match service {
        Service::Aws | Service::AwsDirectory => true,
        Service::Compatible => false,
    };
    if number == 0 || (limited && number > MAX_PARTS) {
        return Err(InvalidPlan::PartId.into());
    }
    Ok(())
}

fn validate_upload_id(upload_id: &str) -> Result<()> {
    if upload_id.is_empty() {
        return Err(InvalidPlan::UploadId.into());
    }
    Ok(())
}

// A commit names at least one part. AWS takes the parts in ascending order
// of their numbers, each once, and refuses any other with
// `InvalidPartOrder`. That also bounds the list by `MAX_PARTS`.
fn validate_parts<E: AsRef<[u8]>>(
    parts: impl Iterator<Item = (u32, E)>,
    service: Service,
) -> Result<()> {
    let ordered = match service {
        Service::Aws | Service::AwsDirectory => true,
        Service::Compatible => false,
    };
    let mut previous = None;
    for (number, e_tag) in parts {
        validate_part_number(number, service)?;
        if !valid_header(e_tag.as_ref()) {
            return Err(InvalidPlan::PartId.into());
        }
        if ordered && previous.is_some_and(|previous| number <= previous) {
            return Err(InvalidPlan::Parts.into());
        }
        previous = Some(number);
    }
    if previous.is_none() {
        return Err(InvalidPlan::Parts.into());
    }
    Ok(())
}

const COMMIT_OPEN: &[u8] =
    b"<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">";
const COMMIT_CLOSE: &[u8] = b"</CompleteMultipartUpload>";

// Writes the list of parts of a commit into `out`, one piece at a time. The
// list is written into the length, the SHA-256 that the request signs, a
// checksum if the plan asks for one, and the body. Every one of those goes
// through this function, so they all see the same bytes.
fn write_part_list<E: AsRef<[u8]>>(out: &mut dyn ByteSink, parts: impl Iterator<Item = (u32, E)>) {
    out.push(COMMIT_OPEN);
    for (number, e_tag) in parts {
        out.push(b"<Part><PartNumber>");
        out.push(U64Decimal::new(number.into()).as_bytes());
        out.push(b"</PartNumber><ETag>");
        write_xml_text(out, e_tag.as_ref());
        out.push(b"</ETag></Part>");
    }
    out.push(COMMIT_CLOSE);
}

// Writes `text` as the text of an XML element. An entity tag is one header
// value, which may hold the bytes that XML text writes as references. The
// quotes around an entity tag are text as they are.
fn write_xml_text(out: &mut dyn ByteSink, text: &[u8]) {
    let mut start = 0;
    for (at, byte) in text.iter().enumerate() {
        let reference: &[u8] = match byte {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            _ => continue,
        };
        out.push(&text[start..at]);
        out.push(reference);
        start = at + 1;
    }
    out.push(&text[start..]);
}
