// S3 uploads in parts: CreateMultipartUpload, UploadPart,
// CompleteMultipartUpload, AbortMultipartUpload and ListParts.
//
// The commit takes the shared plan `PhysicalCommit`, and the stage, the
// commit and the listing answer with the shared outcomes. A part is named by
// its number within an upload that S3 created, and by the entity tag that S3
// returned when it staged the part.

use crate::checksum::{BASE64_LEN, ChecksumKind};
use crate::common::{
    FailureOutcome, decimal_header, encoded, encoded_with_body, failure, finish_with_body,
    push_checksum, valid_header,
};
use crate::common::{checksum_text, validate_checksum_item, validate_template_options};
use crate::encoding;
use crate::request::{ByteSink, HeadWriter, Pass, U64Decimal, Writer};
use crate::s3::template::{
    self, Describe, Detail, Field, FieldList, Operation, Template, Unsigned, Value,
    encode as encode_template,
};
use crate::s3::{
    Bucket, ObjectText, PayloadSigning, S3Values, encode_object, prepare_object, push_stored,
    validate_stored,
};
use crate::s3::{
    Objects, PayloadHash, Service, Signed, Stores, body_kind, failed, is_error_document,
    refuse_error_document, stored_headers, validate_content, validate_key, validate_metadata,
    validate_write_condition,
};
use crate::sigv4::Credentials;
use crate::sigv4::EMPTY_SHA256;
use crate::template::{Body, Fill, Hole, ParamValue, Values};
use crate::url;
use crate::url::QueryValue;
use crate::{ChecksumValue, ConditionValue, ObjectItem, RevisionKind, StageOptions};
use crate::{
    CommitHeadOutcome, CommitShape, ConditionKind, DeleteHeadOutcome, Failure, HeaderSpan,
    InvalidPlan, LegacyRequest, ListPartsHeadOutcome, Listing, MetadataPair, Method, ObjectMeta,
    Payload, PhysicalCommit, RequestedRange, ResponseFault, ResponseHead, Result, ServiceErrorKind,
    StageHeadOutcome, Timestamps, WriteOptions,
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
    /// [Object keys](crate#object-keys).
    pub key: &'a str,
    /// The metadata pairs to store with the object that the upload commits.
    ///
    /// The rules of [`PutOptions::metadata`](crate::PutOptions::metadata)
    /// apply. An S3 commit takes no metadata of its own.
    pub metadata: &'a [MetadataPair<'a>],
    /// The content properties, the tags and the storage class of the object
    /// that the upload commits, under the rules of
    /// [`PutOptions::options`](crate::PutOptions::options). The upload
    /// takes no checksum here: each part carries its own.
    pub options: WriteOptions<'a>,
}

impl<'a> PhysicalCreateUpload<'a> {
    /// Creates a plan that starts an upload to `key`, with no metadata and
    /// no options.
    pub const fn new(key: &'a str) -> Self {
        Self {
            key,
            metadata: &[],
            options: WriteOptions::new(),
        }
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
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
    /// [`Objects::accept_create_upload_error_body`], which returns the final
    /// outcome. If you cannot read the body, pass an empty one and the error
    /// stays unnamed.
    NeedErrorBody(Failure<'h>),
    /// The service refused to create the upload, or it failed to.
    ///
    /// A bucket that does not exist is refused here, with
    /// [`ServiceErrorKind::NoSuchContainer`].
    ServiceFailure(Failure<'h>),
}

crate::common::container_failure_outcome!(CreateUploadHeadOutcome<'h>);

impl core::fmt::Display for CreateUploadHeadOutcome<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Created { .. } => f.write_str("the upload ID follows in the response body"),
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
    /// cannot become an S3 request, for the reasons of the key, the metadata
    /// and the options that [`encode_put`](crate::s3::encode_put) states. A plan whose options
    /// carry a checksum is refused with [`InvalidPlan::Option`].
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
    ) -> Result<LegacyRequest<'r>> {
        validate_key(plan.key)?;
        validate_metadata(plan.metadata, self.bucket.service)?;
        if plan.options.checksum.is_some() {
            return Err(InvalidPlan::Option.into());
        }
        validate_content(
            &plan.options,
            Payload::Slice(&[]),
            PayloadHash::Compute,
            &self.checksums,
            Stores::Object(self.bucket.service),
        )?;
        let (stored, count) = stored_headers(&plan.options, None);
        let signed = Signed {
            method: Method::Post,
            key: Some(plan.key),
            query: &[url::literal("uploads", "")],
            headers: &stored[..count],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: plan.metadata,
            content_sha256: EMPTY_SHA256.as_bytes(),
            tags: plan.options.tags,
            copy: None,
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        // A POST states its length even when it has no content.
        head.header("content-length", b"0");
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
        accept_create_upload_head(head)
    }

    /// Finishes a [`CreateUploadHeadOutcome::NeedErrorBody`] with the
    /// response body.
    ///
    /// This is [`accept_get_error_body`](crate::s3::accept_get_error_body) for a CreateMultipartUpload,
    /// and reads the body the same way. A missing bucket is a
    /// [`CreateUploadHeadOutcome::ServiceFailure`] with
    /// [`ServiceErrorKind::NoSuchContainer`].
    pub fn accept_create_upload_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> CreateUploadHeadOutcome<'h> {
        accept_create_upload_error_body(failure, body)
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
    ///
    /// Returns [`Error::Service`](crate::Error::Service) if `body` is an
    /// error document, which S3 can send under status 200.
    pub fn read_upload_id<'b>(&self, body: &'b mut [u8]) -> Result<&'b str> {
        read_upload_id(body)
    }

    /// Writes the signed request head of an UploadPart into `buf`.
    ///
    /// The head states the length of `content`, which stays where you put
    /// it, and `hash` says how the request signs it, as for
    /// [`encode_put`](crate::s3::encode_put). Keep the entity tag that the answer carries: the
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
    ///   [`encode_put`](crate::s3::encode_put) refuses.
    ///
    /// Returns [`Error::Capacity`](crate::Error::Capacity) as
    /// [`encode_put`](crate::s3::encode_put) does, or call
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
    ) -> Result<LegacyRequest<'r>> {
        validate_key(plan.key)?;
        validate_upload_id(plan.upload_id)?;
        validate_part_number(plan.number, self.bucket.service)?;
        if content.len() > MAX_PART_LEN {
            return Err(InvalidPlan::PayloadTooLarge.into());
        }
        validate_content(&plan.options, content, hash, &self.checksums, Stores::Part)?;
        let pass = Pass::of(buf);
        let content_sha256 = self.content_sha256(hash, content, pass);
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
            content_sha256: content_sha256.as_bytes(),
            tags: &[],
            copy: None,
        };
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
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
        accept_stage_part_head(head)
    }

    /// Finishes a [`StageHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`accept_get_error_body`](crate::s3::accept_get_error_body) for an UploadPart, and reads
    /// the body the same way. A missing upload is
    /// [`StageHeadOutcome::NotFound`] with
    /// [`ServiceErrorKind::NoSuchUpload`].
    pub fn accept_stage_part_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> StageHeadOutcome<'h> {
        accept_stage_part_error_body(failure, body)
    }

    /// Writes a CompleteMultipartUpload into `buf`: the signed head, then the
    /// XML body after it.
    ///
    /// `parts` become the object in this order. The body is written into
    /// `buf` after the head, so [`LegacyRequest::body_span`] names it and
    /// [`LegacyRequest::payload`] borrows it; send both before reusing `buf`.
    /// The request signs the SHA-256 of the body.
    ///
    /// The commit ends the upload. The parts it does not name are dropped.
    /// A plan that sets [`PhysicalCommit::size`] sends it as
    /// `x-amz-mp-object-size`.
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
    ///   [`encode_put`](crate::s3::encode_put) refuses.
    /// - [`InvalidPlan::PartId`] for a part number that
    ///   [`Self::encode_stage_part`] refuses, or an entity tag that is not
    ///   one header value.
    /// - [`InvalidPlan::Parts`] if `parts` is empty, or for AWS, if their
    ///   numbers do not ascend. AWS refuses a list out of order with
    ///   `InvalidPartOrder`. To commit an empty object, stage one empty part
    ///   and commit it.
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
    ) -> Result<LegacyRequest<'r>> {
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
    /// Use this when the parts are produced rather than stored, such as from
    /// an array in another language. `parts` yields each part's number and
    /// entity tag. It is traversed more than once, and every traversal must
    /// yield the same items in the same order.
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
    ) -> Result<LegacyRequest<'r>>
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
            Stores::Part,
        )?;
        if !plan.metadata.is_empty() {
            return Err(InvalidPlan::Option.into());
        }
        validate_write_condition(plan.condition, plan.condition_value, service)?;
        validate_parts(parts.clone(), service)?;

        let mut counted = Writer::new(&mut []);
        write_part_list(&mut counted, parts.clone());
        let length = counted.position();
        let pass = Pass::of(buf);
        let content_sha256 = if pass == Pass::Measure {
            [b'0'; 64]
        } else {
            let mut sum = self.sha256.start();
            write_part_list(&mut sum, parts.clone());
            encoding::hex(&sum.finish())
        };
        let query = [url::encoded("uploadId", upload_id)];
        let size = plan.size.map(U64Decimal::new);
        let size_header = size
            .as_ref()
            .map(|size| ("x-amz-mp-object-size", size.as_bytes()));
        let signed = Signed {
            method: Method::Post,
            key: Some(plan.key),
            query: &query,
            headers: size_header.as_slice(),
            range: RequestedRange::Whole,
            condition: plan.condition,
            condition_value: plan.condition_value,
            metadata: &[],
            content_sha256: &content_sha256,
            tags: &[],
            copy: None,
        };
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        head.header("content-length", U64Decimal::new(length as u64).as_bytes());
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
    /// service failure. Do not retry the commit. Create a new upload and
    /// stage every part again, as AWS documents for this error.
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
        accept_commit_parts_head(shape, head)
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
        accept_commit_parts_body(shape, head, body)
    }

    /// Finishes a [`CommitHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`accept_get_error_body`](crate::s3::accept_get_error_body) for a commit, and reads the
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
        accept_commit_parts_error_body(shape, failure, body)
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
    ) -> Result<LegacyRequest<'r>> {
        validate_key(plan.key)?;
        validate_upload_id(plan.upload_id)?;
        let query = [url::encoded("uploadId", plan.upload_id)];
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
            tags: &[],
            copy: None,
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
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
        accept_abort_upload_head(self.bucket.service, head)
    }

    /// Finishes a [`DeleteHeadOutcome::NeedErrorBody`] of an abort with the
    /// response body.
    ///
    /// This is [`accept_get_error_body`](crate::s3::accept_get_error_body) for an abort, and reads the
    /// body the same way. An upload that is gone is
    /// [`DeleteHeadOutcome::NotFound`] with
    /// [`ServiceErrorKind::NoSuchUpload`].
    pub fn accept_abort_upload_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> DeleteHeadOutcome<'h> {
        accept_abort_upload_error_body(failure, body)
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
    ) -> Result<LegacyRequest<'r>> {
        validate_key(plan.key)?;
        validate_upload_id(plan.upload_id)?;
        if plan.marker.is_some_and(str::is_empty) {
            return Err(InvalidPlan::Marker.into());
        }
        // SigV4 signs the parameters in the order of their names.
        let query = [
            url::number("max-parts", plan.max_parts),
            url::encoded("part-number-marker", plan.marker),
            url::encoded("uploadId", plan.upload_id),
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
            tags: &[],
            copy: None,
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
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
        accept_list_parts_head(head)
    }

    /// Finishes a [`ListPartsHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`accept_get_error_body`](crate::s3::accept_get_error_body) for a ListParts, and reads the
    /// body the same way. A missing upload is
    /// [`ListPartsHeadOutcome::NotFound`] with
    /// [`ServiceErrorKind::NoSuchUpload`].
    pub fn accept_list_parts_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> ListPartsHeadOutcome<'h> {
        accept_list_parts_error_body(failure, body)
    }

    /// Reads a page of parts out of the response body of a ListParts.
    ///
    /// The parts are read in the order S3 wrote them, which is the order of
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
    ///
    /// Returns [`Error::Service`](crate::Error::Service) if `body` is an
    /// error document, which S3 can send under status 200.
    pub fn fill_parts<'b, E: From<Part<'b>>>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
    ) -> Result<Listing<'b>> {
        fill_parts(body, into)
    }
}

// Part number 0 is not a part anywhere. AWS numbers parts up to
// `MAX_PARTS`.
pub(super) fn validate_part_number(number: u32, service: Service) -> Result<()> {
    let limited = service.is_aws();
    if number == 0 || (limited && number > MAX_PARTS) {
        return Err(InvalidPlan::PartId.into());
    }
    Ok(())
}

pub(super) fn validate_upload_id(upload_id: &str) -> Result<()> {
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
    let ordered = service.is_aws();
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

// The URL is the XML namespace of S3's API, whose only version is dated
// 2006-03-01. Nothing fetches it. S3 declares it on its own documents, and
// the AWS SDKs send it on this one.
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
        // The quotes around an entity tag are text as they are.
        encoding::write_xml_text(out, e_tag.as_ref());
        out.push(b"</ETag></Part>");
    }
    out.push(COMMIT_CLOSE);
}

/// What every CreateMultipartUpload of a template does alike: what each
/// object it starts stores once it is committed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CreateUploadOptions<'o> {
    /// The metadata pairs of each object, under the rules of
    /// [`PutOptions::metadata`](crate::PutOptions::metadata).
    pub metadata: &'o [MetadataPair<'o>],
    /// The content properties, the tags and the storage class of each
    /// object. A checksum belongs to each part, and is refused here.
    pub options: WriteOptions<'o>,
}

/// One UploadPart, which stages the part `number` of an upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagePartItem<'i> {
    /// The key of the upload's object.
    pub key: &'i str,
    /// The ID that S3 gave the upload, never empty.
    pub upload_id: &'i str,
    /// The part's number, from 1, and at most [`MAX_PARTS`] on AWS.
    pub number: u32,
    /// The part's bytes, or the length of those you send yourself. At most
    /// [`MAX_PART_LEN`].
    pub content: Payload<'i>,
    /// The checksum of the part, of the template's kind. Present exactly when
    /// the template names a kind.
    pub checksum: Option<ChecksumValue<'i>>,
}

impl<'i> StagePartItem<'i> {
    /// Creates the stage of `content` as the part `number` of the upload
    /// `upload_id` of `key`.
    pub const fn new(key: &'i str, upload_id: &'i str, number: u32, content: Payload<'i>) -> Self {
        Self {
            key,
            upload_id,
            number,
            content,
            checksum: None,
        }
    }
}

/// What every CompleteMultipartUpload of a template does alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitOptions {
    /// The condition that each commit carries, on the object that the key
    /// holds before. AWS takes no date condition on a write.
    pub condition: ConditionKind,
    /// The checksum of the body of each commit, its list of parts.
    pub checksum: Option<ChecksumKind>,
    /// Whether each commit states the length of the object it publishes, in
    /// [`CommitItem::size`]. S3 refuses the commit with 400
    /// `InvalidRequest` if the parts add up to another length, which catches
    /// a part left out of the list.
    pub sized: bool,
}

/// One CompleteMultipartUpload, which commits an object from its parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitItem<'i> {
    /// The key of the upload's object.
    pub key: &'i str,
    /// What the template's condition compares against. Present exactly when
    /// the template carries one.
    pub condition_value: Option<ConditionValue<'i>>,
    /// The ID that S3 gave the upload, never empty.
    pub upload_id: &'i str,
    /// The parts of the object, at least one. AWS takes them in ascending
    /// order of their numbers, each once.
    pub parts: &'i [PartRef<'i>],
    /// The length of the object. Present exactly when the template's
    /// [`CommitOptions::sized`] is set.
    pub size: Option<u64>,
}

impl<'i> CommitItem<'i> {
    /// Creates the commit of `parts` of the upload `upload_id` of `key`.
    pub const fn new(key: &'i str, upload_id: &'i str, parts: &'i [PartRef<'i>]) -> Self {
        Self {
            key,
            condition_value: None,
            upload_id,
            parts,
            size: None,
        }
    }

    /// Returns the part of this commit that holds no borrows, for a template
    /// with `options`.
    pub const fn shape(&self, options: &CommitOptions) -> CommitShape {
        CommitShape {
            condition: options.condition,
        }
    }
}

/// One request that names an upload: an AbortMultipartUpload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadItem<'i> {
    /// The key of the upload's object.
    pub key: &'i str,
    /// The ID that S3 gave the upload, never empty.
    pub upload_id: &'i str,
}

/// One page of a ListParts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListPartsItem<'i> {
    /// The key of the upload's object.
    pub key: &'i str,
    /// The ID that S3 gave the upload, never empty.
    pub upload_id: &'i str,
    /// Where the previous page ended, as its
    /// [`Listing::next_marker`](crate::Listing::next_marker) names it, or
    /// [`None`] for the first page. Never empty.
    pub marker: Option<&'i str>,
}

/// Writes the template of CreateMultipartUploads with `options` into
/// `buf`, signed with `credentials`.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) for a metadata pair or a stored option
/// that [`prepare_put`](crate::s3::prepare_put) refuses, and
/// [`InvalidPlan::Option`] for a checksum or a declared MD5.
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_create_upload(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &CreateUploadOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    validate_template_options(&options.options)?;
    validate_metadata(options.metadata, bucket.service)?;
    validate_stored(&options.options, bucket.service)?;
    let mut signed = FieldList::new();
    push_stored(&mut signed, &options.options);
    prepare_object(
        bucket,
        credentials,
        Operation::CreateUpload,
        ConditionKind::None,
        RevisionKind::Current,
        None,
        &ObjectText {
            signed: signed.as_slice(),
            // A POST states its length even when it has no content.
            unsigned: &[Field::new("content-length", Value::Formatted(b"0"))],
            metadata: options.metadata,
            body: None,
        },
        buf,
    )
}

/// Writes the CreateMultipartUpload of `item` at `now` from `template` into
/// `buf`, ready to sign. Read the upload's ID out of the answer with
/// [`read_upload_id`].
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) for a key that S3 refuses, see
/// [Object keys](crate#object-keys), for a revision or a condition value,
/// and for a template of another operation.
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn encode_create_upload(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    encode_object(template, bytes, Operation::CreateUpload, item, now, buf)
}

/// Reads the response head of a CreateMultipartUpload and reports what to
/// do next.
///
/// # Errors
///
/// Returns [`Error::Response`](crate::Error::Response) if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_create_upload_head<'h>(
    head: ResponseHead<'h>,
) -> Result<CreateUploadHeadOutcome<'h>> {
    match head.status {
        200 => Ok(CreateUploadHeadOutcome::Created {
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`CreateUploadHeadOutcome::NeedErrorBody`] with the response
/// body.
pub fn accept_create_upload_error_body<'h>(
    failure: Failure<'h>,
    body: &[u8],
) -> CreateUploadHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Reads the ID of the upload out of the response body of a
/// CreateMultipartUpload. The ID borrows the body.
///
/// # Errors
///
/// Returns [`Error::Service`](crate::Error::Service) for an error document, and
/// [`Error::Response`](crate::Error::Response) for a body that names no upload.
pub fn read_upload_id(body: &mut [u8]) -> Result<&str> {
    refuse_error_document(body)?;
    crate::xml::s3::parts::read_upload_id(body)
}

/// Writes the template of UploadParts with `options` into `buf`, signed with
/// `credentials`. `signing` says whether each part's SHA-256 is signed.
///
/// # Errors
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_stage_part(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &StageOptions,
    signing: PayloadSigning,
    buf: &mut [u8],
) -> Result<Template> {
    template::prepare(
        bucket,
        credentials,
        &describe_stage(options.checksum, signing),
        buf,
    )
}

// The request of an UploadPart of a template with `checksum` and `signing`.
fn describe_stage(checksum: Option<ChecksumKind>, signing: PayloadSigning) -> Describe<'static, 4> {
    let mut describe = Describe::new(
        Operation::StagePart,
        Method::Put,
        Detail::Stage { checksum, signing },
    );
    describe.query[0] = Some(("partNumber", ParamValue::Hole(Hole::PartNumber)));
    describe.query[1] = Some(("uploadId", ParamValue::Hole(Hole::UploadId)));
    describe.holes.content_length = Some(Hole::ContentLength);
    if let Some(kind) = checksum {
        describe.checksum(kind, Hole::Checksum);
    }
    match signing {
        PayloadSigning::Unsigned => {
            describe.content_sha256 = super::UNSIGNED_PAYLOAD.as_bytes();
        }
        PayloadSigning::Signed => describe.holes.content_sha256 = Some(Hole::PayloadHash),
    }
    describe
}

/// Writes the UploadPart `item` at `now` from `template` into `buf`, ready
/// to sign. Pass the SHA-256 of the part's content exactly when the
/// template signs it.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) for a key that S3 refuses, see
/// [Object keys](crate#object-keys), [`InvalidPlan::UploadId`] for an empty
/// upload ID, [`InvalidPlan::PartId`] for a part number that S3 refuses,
/// [`InvalidPlan::PayloadTooLarge`] for content longer than
/// [`MAX_PART_LEN`], a checksum that is not the template's, a SHA-256 that
/// the template does not sign, and a template of another operation.
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn encode_stage_part<'i>(
    template: &Template,
    bytes: &[u8],
    item: &StagePartItem<'i>,
    content_sha256: Option<&[u8; 32]>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'i>> {
    let Detail::Stage { checksum, signing } = template.detail else {
        return Err(InvalidPlan::Template.into());
    };
    let key = template.request_key(bytes, item.key)?;
    validate_upload_id(item.upload_id)?;
    validate_part_number(item.number, template.service)?;
    if item.content.len() > MAX_PART_LEN {
        return Err(InvalidPlan::PayloadTooLarge.into());
    }
    validate_checksum_item(checksum, item.checksum)?;
    let hex = match (signing, content_sha256) {
        (PayloadSigning::Signed, Some(digest)) => Some(encoding::hex(digest)),
        (PayloadSigning::Unsigned, None) => None,
        _ => return Err(InvalidPlan::Option.into()),
    };
    let mut text = [0; BASE64_LEN];
    let values = S3Values(Values {
        now: Some(now),
        key,
        upload_id: Some(item.upload_id),
        part_number: Some(item.number),
        content_length: Some(item.content.len()),
        checksum: checksum_text(item.checksum, &mut text, true),
        payload_hash: hex.as_ref(),
        ..Values::default()
    });
    encode_template(
        template,
        bytes,
        &values,
        item.content,
        buf,
        &describe_stage(checksum, signing),
    )
}

/// Reads the response head of an UploadPart and reports what S3 did. Keep
/// the entity tag of [`StageHeadOutcome::Staged`]: the commit names the part
/// by it.
///
/// # Errors
///
/// Returns [`Error::Response`](crate::Error::Response) if the head cannot be read. A success status
/// other than 200, and a 200 without an entity tag, are faults.
pub fn accept_stage_part_head<'h>(head: ResponseHead<'h>) -> Result<StageHeadOutcome<'h>> {
    match head.status {
        200 if head.e_tag.is_none() => Err(ResponseFault::Head.into()),
        200 => Ok(StageHeadOutcome::Staged { e_tag: head.e_tag }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`StageHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_stage_part_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> StageHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Writes the template of CompleteMultipartUploads with `options` into
/// `buf`, signed with `credentials`.
///
/// Sign each request with the SHA-256 of its body, see
/// [`set_body_hash`](crate::s3::set_body_hash), and write in the checksum of
/// its body with [`set_body_checksum`](crate::s3::set_body_checksum) first
/// when the template sends one.
///
/// # Errors
///
/// Returns [`InvalidPlan::Condition`] for a date condition, for
/// [`Service::Aws`] and [`Service::AwsDirectory`].
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_commit_parts(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &CommitOptions,
    buf: &mut [u8],
) -> Result<Template> {
    if bucket.service.is_aws() && options.condition.is_date() {
        return Err(InvalidPlan::Condition.into());
    }
    template::prepare(
        bucket,
        credentials,
        &describe_commit(options.condition, options.checksum, options.sized),
        buf,
    )
}

// The request of a CompleteMultipartUpload of a template with `condition`,
// `checksum`, and the size of the object when `sized` says so. Its list
// of parts is its body.
fn describe_commit(
    condition: ConditionKind,
    checksum: Option<ChecksumKind>,
    sized: bool,
) -> Describe<'static, 4> {
    let mut describe = Describe::new(
        Operation::CommitParts,
        Method::Post,
        Detail::Commit {
            condition,
            checksum,
            sized,
        },
    );
    describe.query[0] = Some(("uploadId", ParamValue::Hole(Hole::UploadId)));
    describe.condition(condition);
    describe.holes.object_size = sized;
    describe.with_body(Body::Each, checksum);
    describe
}

// Writes the parts of a commit as the body of its request.
struct CommitFill<'i> {
    values: S3Values<'i>,
    parts: &'i [PartRef<'i>],
}

impl Fill for CommitFill<'_> {
    const BODY: bool = true;

    fn values(&self) -> &Values<'_> {
        &self.values.0
    }

    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        match hole {
            Hole::Body => {
                write_part_list(out, self.parts.iter().map(|part| (part.number, part.e_tag)));
                true
            }
            hole => self.values.write(hole, out),
        }
    }
}

/// Writes the CompleteMultipartUpload `item` at `now` from `template` into
/// `buf`, with its list of parts as the body after the canonical request,
/// ready to sign.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) for a key that S3 refuses, see
/// [Object keys](crate#object-keys), [`InvalidPlan::UploadId`] for an empty
/// upload ID, a condition value that the template does not take,
/// [`InvalidPlan::Parts`] for no part or, on AWS, parts out of order,
/// [`InvalidPlan::PartId`] for a part number or an entity tag that S3
/// refuses, [`InvalidPlan::Option`] for a size that the template does not
/// state, and [`InvalidPlan::Template`] for a template of another operation.
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn encode_commit_parts(
    template: &Template,
    bytes: &[u8],
    item: &CommitItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    let Detail::Commit {
        condition,
        checksum,
        sized,
    } = template.detail
    else {
        return Err(InvalidPlan::Template.into());
    };
    let key = template.request_key(bytes, item.key)?;
    validate_upload_id(item.upload_id)?;
    validate_write_condition(condition, item.condition_value, template.service)?;
    validate_parts(
        item.parts.iter().map(|part| (part.number, part.e_tag)),
        template.service,
    )?;
    if sized != item.size.is_some() {
        return Err(InvalidPlan::Option.into());
    }
    let fill = CommitFill {
        values: S3Values(Values {
            now: Some(now),
            key,
            condition: item.condition_value,
            upload_id: Some(item.upload_id),
            object_size: item.size,
            body_checksum: checksum,
            ..Values::default()
        }),
        parts: item.parts,
    };
    encode_template(
        template,
        bytes,
        &fill,
        Payload::Slice(&[]),
        buf,
        &describe_commit(condition, checksum, sized),
    )
}

/// Reads the response head of a CompleteMultipartUpload and reports what to
/// do next. S3 answers with 200 before it has finished, and writes the
/// result into the body: read it with [`accept_commit_parts_body`].
///
/// # Errors
///
/// Returns [`Error::Response`](crate::Error::Response) if the head cannot be read. A success status
/// other than 200, and a failed condition on a commit that carried none,
/// are [`ResponseFault::Status`].
pub fn accept_commit_parts_head<'h>(
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
        _ => Ok(failed(&head)),
    }
}

/// Reads the result of a CompleteMultipartUpload out of its body, which may
/// still be an error.
///
/// # Errors
///
/// Returns [`Error::Response`](crate::Error::Response) for a status other than 200, and for a body
/// that is neither a result nor an error document.
pub fn accept_commit_parts_body<'h>(
    shape: CommitShape,
    head: ResponseHead<'h>,
    body: &'h mut [u8],
) -> Result<CommitHeadOutcome<'h>> {
    if head.status != 200 {
        return Err(ResponseFault::Status.into());
    }
    // A directory bucket reports under status 200 what a general purpose
    // bucket reports under 404 or 412, so those become the same outcomes.
    if is_error_document(body) {
        let kind = body_kind(body);
        return Ok(match kind {
            Some(ServiceErrorKind::Precondition) if shape.condition != ConditionKind::None => {
                CommitHeadOutcome::PreconditionFailed
            }
            Some(ServiceErrorKind::NoSuchUpload) => CommitHeadOutcome::NotFound { kind },
            _ => finish_with_body(failure(head.status, None, head.request_id), kind),
        });
    }
    let e_tag = crate::xml::s3::parts::read_committed(body)?;
    Ok(CommitHeadOutcome::Committed {
        meta: ObjectMeta {
            e_tag: Some(e_tag.as_bytes()),
            version: head.version,
            ..ObjectMeta::default()
        },
    })
}

/// Finishes a [`CommitHeadOutcome::NeedErrorBody`] with the response body.
/// S3 reports a failed condition in the head.
pub fn accept_commit_parts_error_body<'h>(
    shape: CommitShape,
    failure: Failure<'h>,
    body: &[u8],
) -> CommitHeadOutcome<'h> {
    let _ = shape;
    finish_with_body(failure, body_kind(body))
}

/// Writes the template of AbortMultipartUploads into `buf`, signed with
/// `credentials`.
///
/// # Errors
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_abort_upload(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    template::prepare(bucket, credentials, &describe_abort_upload(), buf)
}

// The request of an AbortMultipartUpload.
fn describe_abort_upload() -> Describe<'static, 4> {
    let mut describe = Describe::new(Operation::AbortUpload, Method::Delete, Detail::Upload);
    describe.query[0] = Some(("uploadId", ParamValue::Hole(Hole::UploadId)));
    describe
}

/// Writes the AbortMultipartUpload `item` at `now` from `template` into
/// `buf`, ready to sign.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) for a key that S3 refuses, see
/// [Object keys](crate#object-keys), [`InvalidPlan::UploadId`] for an empty
/// upload ID, and [`InvalidPlan::Template`] for a template of another
/// operation.
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn encode_abort_upload(
    template: &Template,
    bytes: &[u8],
    item: &UploadItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    if template.operation != Operation::AbortUpload {
        return Err(InvalidPlan::Template.into());
    }
    let key = template.request_key(bytes, item.key)?;
    validate_upload_id(item.upload_id)?;
    let values = S3Values(Values {
        now: Some(now),
        key,
        upload_id: Some(item.upload_id),
        ..Values::default()
    });
    encode_template(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_abort_upload(),
    )
}

/// Reads the response head of an AbortMultipartUpload and reports what S3
/// did. Pass the service of the template: a [`Service::Compatible`]
/// service may answer 200 as well as 204.
///
/// # Errors
///
/// Returns [`Error::Response`](crate::Error::Response) if the head cannot be read. Any other success
/// status is [`ResponseFault::Status`].
pub fn accept_abort_upload_head<'h>(
    service: Service,
    head: ResponseHead<'h>,
) -> Result<DeleteHeadOutcome<'h>> {
    let accepts_200 = !service.is_aws();
    match head.status {
        204 => Ok(DeleteHeadOutcome::Accepted),
        200 if accepts_200 => Ok(DeleteHeadOutcome::Accepted),
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`DeleteHeadOutcome::NeedErrorBody`] of an
/// AbortMultipartUpload with the response body.
pub fn accept_abort_upload_error_body<'h>(
    failure: Failure<'h>,
    body: &[u8],
) -> DeleteHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Writes the template of ListParts into `buf`, signed with `credentials`,
/// each page of at most `max_parts` parts, or S3's maximum.
///
/// # Errors
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_list_parts(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    max_parts: Option<u32>,
    buf: &mut [u8],
) -> Result<Template> {
    template::prepare(bucket, credentials, &describe_list_parts(max_parts), buf)
}

// The request of a page of a ListParts. `max_parts` is the template's, and
// none for a request: it comes before the parameter that a page may leave
// out, so a request writes none of it.
fn describe_list_parts(max_parts: Option<u32>) -> Describe<'static, 4> {
    let mut describe = Describe::new(Operation::ListParts, Method::Get, Detail::Upload);
    // SigV4 signs the parameters in the order of their names.
    describe.query[0] =
        max_parts.map(|count| ("max-parts", ParamValue::Fixed(QueryValue::Number(count))));
    describe.query[1] = Some(("part-number-marker", ParamValue::Optional(Hole::Marker)));
    describe.query[2] = Some(("uploadId", ParamValue::Hole(Hole::UploadId)));
    describe
}

/// Writes the ListParts page `item` at `now` from `template` into `buf`,
/// ready to sign. Read the whole response body and pass it to
/// [`fill_parts`].
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) for a key that S3 refuses, see
/// [Object keys](crate#object-keys), [`InvalidPlan::UploadId`] for an empty
/// upload ID, [`InvalidPlan::Marker`] for an empty marker, and
/// [`InvalidPlan::Template`] for a template of another operation.
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if `buf` is too small, with the bytes it
/// needs.
pub fn encode_list_parts(
    template: &Template,
    bytes: &[u8],
    item: &ListPartsItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    if template.operation != Operation::ListParts {
        return Err(InvalidPlan::Template.into());
    }
    let key = template.request_key(bytes, item.key)?;
    validate_upload_id(item.upload_id)?;
    if item.marker.is_some_and(str::is_empty) {
        return Err(InvalidPlan::Marker.into());
    }
    let values = S3Values(Values {
        now: Some(now),
        key,
        upload_id: Some(item.upload_id),
        marker: item.marker,
        ..Values::default()
    });
    encode_template(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_list_parts(None),
    )
}

/// Reads the response head of a ListParts and reports what to do next.
///
/// # Errors
///
/// Returns [`Error::Response`](crate::Error::Response) if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_list_parts_head<'h>(head: ResponseHead<'h>) -> Result<ListPartsHeadOutcome<'h>> {
    match head.status {
        200 => Ok(ListPartsHeadOutcome::Parts {
            meta: ObjectMeta::default(),
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`ListPartsHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_list_parts_error_body<'h>(
    failure: Failure<'h>,
    body: &[u8],
) -> ListPartsHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Reads the parts out of the response body of a ListParts into `into`. The
/// parts borrow the body, which is decoded in place.
///
/// # Errors
///
/// Returns [`Error::Capacity`](crate::Error::Capacity) if the page holds more parts than the array,
/// [`Error::Service`](crate::Error::Service) for an error document, and [`Error::Response`](crate::Error::Response) for a
/// body that is not a page of parts.
pub fn fill_parts<'b, E: From<Part<'b>>>(
    body: &'b mut [u8],
    into: &mut [E],
) -> Result<Listing<'b>> {
    refuse_error_document(body)?;
    crate::xml::s3::parts::fill_parts(body, into)
}
