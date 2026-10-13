// S3 DeleteObjects: the removal of up to 1,000 objects in one request, whose
// answer holds a result for each.

use crate::Payload;
use crate::checksum::ChecksumKind;
use crate::common::{decimal_header, finish_with_body, validate_revision};
use crate::encoding;
use crate::request::{ByteSink, Writer};
use crate::s3::template::{
    self, Describe, Detail, Operation, Template, Unsigned, encode as encode_template,
};
use crate::s3::{Bucket, S3Values};
use crate::s3::{body_kind, failed, refuse_error_document, validate_key};
use crate::sigv4::Credentials;
use crate::template::{Body, Fill, Hole, ParamValue, Values};
use crate::url::QueryValue;
use crate::{
    DeleteManyHeadOutcome, DeleteTarget, Failure, InvalidRequest, Method, ResponseFault,
    ResponseHead, Result, Revision, Timestamps,
};

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;

/// The most objects that one DeleteObjects removes.
pub const MAX_DELETE_KEYS: usize = 1000;

const DELETE_OPEN: &[u8] = b"<Delete xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">";
const DELETE_CLOSE: &[u8] = b"</Delete>";

/// The result of the removal of one object in a DeleteObjects.
///
/// [`fill_delete_results`] writes one for each object that the
/// request named. S3 reports a key that held no object as removed.
///
/// S3 lists the results in an order of its own. Match each to the object it
/// answers by its key and its version: a request may name one key with two
/// versions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteResult<'b> {
    /// The key, as S3 wrote it.
    pub key: &'b str,
    /// The error code if S3 did not remove the object, such as
    /// `AccessDenied`, or [`None`] if it did.
    pub code: Option<&'b str>,
    /// The version that the request named, as S3 repeated it.
    pub version: Option<&'b str>,
    /// Whether the removal wrote a delete marker, which a removal without a
    /// version does on a bucket that keeps versions, or removed one.
    pub delete_marker: bool,
    /// The version of that delete marker.
    pub delete_marker_version: Option<&'b str>,
}

fn write_delete(out: &mut dyn ByteSink, objects: &[DeleteTarget<'_>]) {
    out.push(DELETE_OPEN);
    for object in objects {
        out.push(b"<Object><Key>");
        encoding::write_xml_text(out, object.key.as_bytes());
        out.push(b"</Key>");
        // Validation refused a snapshot.
        if let Some(Revision::Version(id)) = object.revision {
            out.push(b"<VersionId>");
            encoding::write_xml_text(out, id.as_bytes());
            out.push(b"</VersionId>");
        }
        out.push(b"</Object>");
    }
    out.push(DELETE_CLOSE);
}

// Writes the objects of a removal as the body of its request.
struct DeleteFill<'i> {
    values: S3Values<'i>,
    objects: &'i [DeleteTarget<'i>],
}

impl Fill for DeleteFill<'_> {
    const BODY: bool = true;

    fn values(&self) -> &Values<'_> {
        &self.values.0
    }

    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        match hole {
            Hole::Body => {
                write_delete(out, self.objects);
                true
            }
            hole => self.values.write(hole, out),
        }
    }
}

/// Writes the template of DeleteObjects into `buf`, signed with
/// `credentials`, each with a checksum of its body of the kind `checksum`.
///
/// Sign each request with the SHA-256 of its body, see
/// [`set_body_hash`](crate::s3::set_body_hash), and write in its checksum
/// with [`set_body_checksum`](crate::s3::set_body_checksum) first.
///
/// # Errors
///
/// Returns [`InvalidRequest::Option`] for no checksum, for
/// [`Service::Aws`](crate::s3::Service::Aws) and
/// [`Service::AwsDirectory`](crate::s3::Service::AwsDirectory): AWS takes a removal of
/// several objects only with one.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_delete_many(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    checksum: Option<ChecksumKind>,
    buf: &mut [u8],
) -> Result<Template> {
    if bucket.service.is_aws() && checksum.is_none() {
        return Err(InvalidRequest::Option.into());
    }
    template::prepare(bucket, credentials, &describe_delete_many(checksum), buf)
}

// The request of a DeleteObjects of a template with `checksum`. Its list of
// objects is its body.
fn describe_delete_many(checksum: Option<ChecksumKind>) -> Describe<'static, 4> {
    let mut describe = Describe::new(
        Operation::DeleteMany,
        Method::Post,
        Detail::DeleteMany(checksum),
    );
    describe.key = false;
    describe.query[0] = Some(("delete", ParamValue::Fixed(QueryValue::Literal(""))));
    describe.with_body(Body::Each, checksum);
    describe
}

/// Writes the DeleteObjects that removes `objects` at `now` from `template`
/// into `buf`, with its body after the canonical request, ready to sign.
///
/// # Errors
///
/// Returns [`InvalidRequest::Keys`] for no object or more than
/// [`MAX_DELETE_KEYS`], [`Error::InvalidRequest`] for a key that S3 refuses,
/// see [Object keys](crate#object-keys), [`InvalidRequest::KeyControlCharacter`]
/// for a key with a control character other than a tab, which XML cannot
/// carry, [`InvalidRequest::Revision`] for a snapshot, and
/// [`InvalidRequest::Template`] for a template of another operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_delete_many(
    template: &Template,
    bytes: &[u8],
    objects: &[DeleteTarget<'_>],
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    let Detail::DeleteMany(checksum) = template.detail else {
        return Err(InvalidRequest::Template.into());
    };
    if objects.is_empty() || objects.len() > MAX_DELETE_KEYS {
        return Err(InvalidRequest::Keys.into());
    }
    for object in objects {
        validate_key(object.key)?;
        if object
            .key
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(InvalidRequest::KeyControlCharacter.into());
        }
        validate_revision(object.revision, false)?;
    }
    let fill = DeleteFill {
        values: S3Values(Values {
            now: Some(now),
            body_checksum: checksum,
            ..Values::default()
        }),
        objects,
    };
    encode_template(
        template,
        bytes,
        &fill,
        Payload::Slice(&[]),
        buf,
        &describe_delete_many(checksum),
    )
}

/// Reads the response head of a DeleteObjects and reports what to do next.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_delete_many_head<'h>(head: ResponseHead<'h>) -> Result<DeleteManyHeadOutcome<'h>> {
    match head.status {
        200 => Ok(DeleteManyHeadOutcome::Results {
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`DeleteManyHeadOutcome::NeedErrorBody`] with the response
/// body.
pub fn accept_delete_many_error_body<'h>(
    failure: Failure<'h>,
    body: &[u8],
) -> DeleteManyHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Reads the result of each removal out of the response body of a
/// DeleteObjects into `into`, and returns how many it read. The results
/// borrow the body, which is decoded in place.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if the body holds more results than the
/// array, [`Error::Service`] for an error document, and [`Error::Response`]
/// for a body that is not a `DeleteResult` document.
pub fn fill_delete_results<'b>(body: &'b mut [u8], into: &mut [DeleteResult<'b>]) -> Result<usize> {
    refuse_error_document(body)?;
    crate::xml::s3::batch::fill_delete_results(body, into)
}
