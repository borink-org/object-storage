// S3 object tags: PutObjectTagging and GetObjectTagging. A write stores tags
// with the object as well, through `WriteOptions`.

#[cfg(doc)]
use crate::{Error, InvalidRequest};
// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
use crate::checksum::ChecksumKind;
use crate::head::read_decimal_header;
use crate::outcome::finish_with_body;
use crate::request::{ByteSink, write_tag_set};
use crate::s3::template::{Operation, Template, Unsigned};
use crate::s3::{
    Bucket, ObjectText, encode_object, kind_in_body, prepare_object, read_failure_head,
    refuse_error_document, s3_tag_char,
};
use crate::sigv4::Credentials;
use crate::validate::validate_tags;
use crate::{
    ConditionKind, Failure, ObjectItem, ResponseFault, ResponseHead, Result, RevisionKind, Tag,
    TagsHeadOutcome, Timestamps, UpdateHeadOutcome,
};

const TAGGING_OPEN: &[u8] = b"<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">";
const TAGGING_CLOSE: &[u8] = b"</Tagging>";

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
/// Returns [`InvalidRequest::Tag`] for a tag that breaks the rules of [`Tag`],
/// and [`InvalidRequest::Revision`] for a snapshot.
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
    prepare_object(
        bucket,
        credentials,
        Operation::PutTagging,
        ConditionKind::None,
        revision,
        checksum,
        &ObjectText {
            body: Some(&write),
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the PutObjectTagging of `item` at `now` from `template` into
/// `buf`, with its body after the canonical request, ready to sign.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a key that S3 refuses, see
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
            meta: head.object_meta(),
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

/// Finishes an [`UpdateHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_update_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> UpdateHeadOutcome<'h> {
    finish_with_body(failure, kind_in_body(body))
}

/// Writes the template of GetObjectTaggings into `buf`, signed with
/// `credentials`. Each request names a version of the kind `revision`, or
/// the object as it is now.
///
/// # Errors
///
/// Returns [`InvalidRequest::Revision`] for a snapshot.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_get_tagging(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    revision: RevisionKind,
    buf: &mut [u8],
) -> Result<Template> {
    prepare_object(
        bucket,
        credentials,
        Operation::GetTagging,
        ConditionKind::None,
        revision,
        None,
        &ObjectText::default(),
        buf,
    )
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
            expected_len: read_decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

/// Finishes a [`TagsHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_get_tagging_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> TagsHeadOutcome<'h> {
    finish_with_body(failure, kind_in_body(body))
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
