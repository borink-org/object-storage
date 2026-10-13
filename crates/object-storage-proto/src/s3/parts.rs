// S3 uploads in parts: CreateMultipartUpload, UploadPart,
// CompleteMultipartUpload, AbortMultipartUpload and ListParts.
//
// The stage, the commit and the listing answer with the shared outcomes. A part is named by
// its number within an upload that S3 created, and by the entity tag that S3
// returned when it staged the part.

use crate::checksum::{BASE64_LEN, ChecksumKind};
use crate::common::{FailureOutcome, decimal_header, failure, finish_with_body, valid_header};
use crate::common::{checksum_text, validate_checksum_item};
use crate::encoding;
use crate::request::{ByteSink, U64Decimal, Writer};
use crate::s3::template::{
    self, Describe, Detail, Field, FieldList, Operation, Template, Unsigned, Value,
    encode as encode_template,
};
use crate::s3::{
    Bucket, ObjectText, PayloadSigning, S3Values, encode_object, prepare_object, push_stored,
    validate_stored,
};
use crate::s3::{
    Service, body_kind, failed, is_error_document, refuse_error_document, validate_metadata,
    validate_write_condition,
};
use crate::sigv4::Credentials;
use crate::template::{Body, Fill, Hole, ParamValue, Values};
use crate::url::QueryValue;
use crate::{ChecksumValue, ConditionValue, ObjectItem, RevisionKind, StageOptions};
use crate::{
    CommitHeadOutcome, CommitShape, ConditionKind, DeleteHeadOutcome, Failure, InvalidPlan,
    ListPartsHeadOutcome, Listing, MetadataPair, Method, ObjectMeta, Payload, ResponseFault,
    ResponseHead, Result, ServiceErrorKind, StageHeadOutcome, Timestamps, WriteOptions,
};

/// The most bytes that S3 takes in one part.
///
/// [`encode_stage_part`] refuses a longer payload with
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
    /// with [`iso8601_ms`](crate::iso8601_ms).
    pub last_modified: Option<&'b str>,
}

/// The result of reading the response head of a CreateMultipartUpload.
///
/// A head that reports a failure is one of these too.
/// [`accept_create_upload_head`] returns an [`Err`] only for a head
/// it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CreateUploadHeadOutcome<'h> {
    /// The upload exists, and its ID follows in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`read_upload_id`].
    Created {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
    /// [`accept_create_upload_error_body`], which returns the final
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
