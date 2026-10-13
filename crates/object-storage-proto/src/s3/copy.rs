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
use crate::head::read_decimal_header;
use crate::outcome::finish_with_body;
use crate::s3::parts::{validate_part_number, validate_upload_id};
use crate::s3::template::{
    self, CopyFrom, Describe, Detail, Field, FieldList, FieldValue, Operation, Template, Unsigned,
    encode_template,
};
use crate::s3::{
    Bucket, S3Values, is_error_document, kind_in_body, push_stored_fields, read_failure_head,
    s3_tag_char, validate_key, validate_metadata, validate_stored_properties,
    validate_write_condition,
};
use crate::sigv4::{Credentials, Token};
use crate::template::{Hole, HoleValues, ParamValue};
use crate::validate::{
    validate_condition, validate_revision_item, validate_source_container, validate_source_range,
    validate_tags,
};
use crate::xml::s3::copy::{read_copied, read_part_copied};
use crate::{
    ConditionKind, CopyHeadOutcome, CopyItem, CopyOptions, CopyShape, Failure, InvalidRequest,
    Method, ObjectMeta, Payload, Ranging, ResponseFault, ResponseHead, Result, RevisionKind,
    ServiceErrorKind, StageHeadOutcome, Timestamps, WriteOptions,
};

// Whether a copy carried a condition on the target or on the source.
fn carries_condition(shape: CopyShape) -> bool {
    shape.condition != ConditionKind::None || shape.source_condition != ConditionKind::None
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
        return Err(InvalidRequest::Option.into());
    }
    // A bucket of any account is named by its name alone.
    if options.source_endpoint.is_some() {
        return Err(InvalidRequest::CopySource.into());
    }
    validate_source_container(options.source_container)?;
    if options.source_revision == RevisionKind::Snapshot {
        return Err(InvalidRequest::Revision.into());
    }
    // A part is no object: it takes a source, and nothing that an object
    // stores.
    if staged
        && (options.condition != ConditionKind::None
            || options.metadata.is_some()
            || options.tags.is_some()
            || options.options != WriteOptions::default())
    {
        return Err(InvalidRequest::Option.into());
    }
    if !staged && options.source_ranging == Ranging::Ranged {
        return Err(InvalidRequest::Range.into());
    }
    validate_metadata(options.metadata.unwrap_or(&[]), service)?;
    // A copy names its tags apart, so that none can differ from the
    // source's, and has nothing of its own to sum.
    if !options.options.tags.is_empty() {
        return Err(InvalidRequest::Option.into());
    }
    validate_tags(
        options.tags.unwrap_or(&[]),
        s3_tag_char,
        service.is_aws().then_some((10, 128, 256)),
    )?;
    validate_stored_properties(&options.options, service)?;
    if service.is_aws() && options.condition.is_date() {
        return Err(InvalidRequest::Condition.into());
    }
    let detail = Detail::Copy {
        condition: options.condition,
        source_revision: options.source_revision,
        source_condition: options.source_condition,
        source_ranging: options.source_ranging,
    };
    let mut signed = FieldList::new();
    push_stored_fields(&mut signed, &options.options);
    // S3 keeps the source's metadata and content properties unless told to
    // replace them, and its tags likewise. Replacing the tags with none
    // sends an empty `x-amz-tagging`.
    if options.metadata.is_some() || !options.options.properties.is_empty() {
        signed.push(Field::new(
            "x-amz-metadata-directive",
            FieldValue::Formatted(b"REPLACE"),
        ));
    }
    if let Some(tags) = options.tags {
        signed.push(Field::new(
            "x-amz-tagging-directive",
            FieldValue::Formatted(b"REPLACE"),
        ));
        signed.push(Field::new("x-amz-tagging", FieldValue::Tags(tags)));
    }
    let describe = Describe {
        signed: signed.as_slice(),
        unsigned: &[Field::new("content-length", FieldValue::Formatted(b"0"))],
        metadata: options.metadata.unwrap_or(&[]),
        ..describe_copy(
            staged,
            detail,
            options.source_container.unwrap_or(bucket.name),
        )
    };
    template::prepare_template(bucket, credentials, &describe, buf)
}

// The request of a copy, or of a copy that stages a part, for a template of
// `detail`, from the bucket `source_bucket`, which is the template's text
// and empty for a request.
fn describe_copy(staged: bool, detail: Detail, source_bucket: &str) -> Describe<'_, 4> {
    let Detail::Copy {
        condition,
        source_revision,
        source_condition,
        source_ranging,
    } = detail
    else {
        unreachable!("a copy's template has the detail of a copy");
    };
    let operation = match staged {
        false => Operation::Copy,
        true => Operation::StagePartCopy,
    };
    let mut describe = Describe::new(operation, Method::Put, detail);
    if staged {
        describe.query[0] = Some(("partNumber", ParamValue::Hole(Hole::PartNumber)));
        describe.query[1] = Some(("uploadId", ParamValue::Hole(Hole::UploadId)));
    }
    describe.holes.copy_source = Some(CopyFrom {
        bucket: source_bucket,
        revision: source_revision,
    });
    describe.holes.source_condition = source_condition_header(source_condition);
    describe.holes.source_range = source_ranging == Ranging::Ranged;
    describe.condition(condition);
    describe
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
    let detail @ Detail::Copy {
        condition,
        source_revision,
        source_condition,
        source_ranging,
    } = template.detail
    else {
        return Err(InvalidRequest::Template.into());
    };
    if template.operation != operation {
        return Err(InvalidRequest::Template.into());
    }
    let key = template.request_key(bytes, item.key)?;
    validate_key(item.source_key)?;
    if let Some((upload_id, number)) = part {
        validate_upload_id(upload_id)?;
        validate_part_number(number, template.service)?;
    }
    validate_revision_item(source_revision, item.source_revision)?;
    validate_write_condition(condition, item.condition_value, template.service)?;
    validate_condition(source_condition, item.source_condition_value)?;
    validate_source_range(source_ranging, item.source_range)?;
    let values = S3Values(HoleValues {
        now: Some(now),
        key,
        condition: item.condition_value,
        source_key: Some(item.source_key),
        source_revision: item.source_revision,
        source_condition: item.source_condition_value,
        source_range: Some(item.source_range),
        upload_id: part.map(|(upload_id, _)| upload_id),
        part_number: part.map(|(_, number)| number),
        ..HoleValues::default()
    });
    encode_template(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_copy(operation == Operation::StagePartCopy, detail, ""),
    )
}

/// Writes the template of CopyObjects with `options` into `buf`, signed with
/// `credentials`.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for options that S3 refuses:
/// [`InvalidRequest::Option`] for the credentials of a session, which AWS
/// refuses for a copy, for a checksum, and for tags in
/// [`CopyOptions::options`]; [`InvalidRequest::CopySource`] for a source
/// endpoint, or a source bucket that is empty or holds `/`, `?` or `#`;
/// [`InvalidRequest::Revision`] for a snapshot; [`InvalidRequest::Condition`] for
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
/// Returns [`Error::InvalidRequest`] for a key of the target or the source
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
/// the source's are [`InvalidRequest::Option`].
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
/// As [`encode_copy`], with [`InvalidRequest::UploadId`] for an empty upload
/// ID, [`InvalidRequest::PartId`] for a part number that S3 refuses, and a
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
            expected_len: read_decimal_header(head.content_length)?,
        }),
        412 if !carries_condition(shape) => Err(ResponseFault::Status.into()),
        412 => Ok(CopyHeadOutcome::PreconditionFailed),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
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
        let kind = kind_in_body(body);
        return Ok(match kind {
            Some(ServiceErrorKind::Precondition) if carries_condition(shape) => {
                CopyHeadOutcome::PreconditionFailed
            }
            Some(ServiceErrorKind::NotFound) => CopyHeadOutcome::NotFound { kind },
            _ => finish_with_body(Failure::new(head.status, None, head.request_id), kind),
        });
    }
    let e_tag = read_copied(body)?;
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
    finish_with_body(failure, kind_in_body(body))
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
            expected_len: read_decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
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
        let kind = kind_in_body(body);
        return Ok(match kind {
            Some(ServiceErrorKind::NoSuchUpload) => StageHeadOutcome::NotFound { kind },
            _ => finish_with_body(Failure::new(head.status, None, head.request_id), kind),
        });
    }
    let e_tag = read_part_copied(body)?;
    Ok(StageHeadOutcome::Staged {
        e_tag: Some(e_tag.as_bytes()),
    })
}
