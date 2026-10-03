// S3 RestoreObject: a temporary copy of an object in an archive storage
// class, readable for a number of days. S3 answers that it started, and
// restores in hours. A HEAD reports the progress in `x-amz-restore`.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::checksum::ChecksumKind;
use crate::common::{
    encoded_with_body, failure, finish_with_body, push_checksum, validate_revision,
};
use crate::encoding;
use crate::request::{ByteSink, HeadWriter, U64Decimal, Writer};
use crate::s3::{
    CHECKSUM_TEXT_LEN, Objects, Service, Signed, body_kind, validate_key, validate_s3_checksum,
    version_parameter,
};
use crate::url::QueryValue;
use crate::{
    ConditionKind, Failure, HeaderSpan, InvalidPlan, Method, PhysicalRestore, RequestedRange,
    ResponseFault, ResponseHead, RestoreHeadOutcome, RestorePriority, Result, Timestamps,
    TransactionalChecksum, WireRequest,
};

const RESTORE_OPEN: &[u8] = b"<RestoreRequest xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">";
const RESTORE_CLOSE: &[u8] = b"</RestoreRequest>";

impl<'a> Objects<'a> {
    /// Writes a RestoreObject into `buf`: the signed head, then the XML body
    /// after it, which names the days and the retrieval tier.
    ///
    /// The body is written into `buf` after the head, so
    /// [`WireRequest::body_span`] names it and [`WireRequest::payload`]
    /// borrows it. The request signs the SHA-256 of the body, and sends the
    /// checksum that the plan asks for as [`Self::encode_put_tagging`] does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key or a revision that
    /// [`Self::encode_get`] refuses, and [`InvalidPlan::Option`] for a plan
    /// that names a tier or zero days, for a checksum whose provider the
    /// client has not registered, and for a directory bucket, which keeps no
    /// archive.
    ///
    /// Returns [`Error::Capacity`] with the bytes that the head and the body
    /// need together, or call
    /// [`layered::s3::restore_requirements`](crate::layered::s3::restore_requirements)
    /// first.
    pub fn encode_restore<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalRestore<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key)?;
        validate_revision(plan.revision, false)?;
        // S3 restores a copy in the object's own storage class, for at least
        // a day.
        if matches!(self.bucket.service, Service::AwsDirectory)
            || plan.tier.is_some()
            || plan.days == Some(0)
        {
            return Err(InvalidPlan::Option.into());
        }
        let checksum = plan.checksum.map(TransactionalChecksum::Compute);
        validate_s3_checksum(checksum, true, &self.checksums)?;

        let mut counted = Writer::new(&mut []);
        write_restore(&mut counted, plan);
        let length = counted.position();
        let dry = buf.is_empty();
        let content_sha256 = if dry {
            [b'0'; 64]
        } else {
            let mut sum = self.sha256.start();
            write_restore(&mut sum, plan);
            encoding::hex(&sum.finish())
        };
        let mut text = [0; CHECKSUM_TEXT_LEN];
        let signed_checksum =
            self.signed_checksum(checksum, |sum| write_restore(sum, plan), dry, &mut text);
        let query = [
            Some(("restore", QueryValue::Literal(""))),
            version_parameter(plan.revision),
        ];
        let signed = Signed {
            method: Method::Post,
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
            write_restore(sum, plan);
        });
        encoded_with_body(head, Method::Post, |out| write_restore(out, plan))
    }

    /// Reads the response head of a RestoreObject and reports what S3 did.
    ///
    /// S3 answers 202 when it starts a restore, which is
    /// [`RestoreHeadOutcome::Started`], and 200 when it holds a restored copy
    /// already, whose days it set anew, which is
    /// [`RestoreHeadOutcome::Readable`]. A failure is
    /// [`RestoreHeadOutcome::NeedErrorBody`]: read the body and pass it to
    /// [`Self::accept_restore_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. Any other
    /// success status is [`ResponseFault::Status`].
    pub fn accept_restore_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<RestoreHeadOutcome<'h>> {
        match head.status {
            202 => Ok(RestoreHeadOutcome::Started),
            200 => Ok(RestoreHeadOutcome::Readable),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(RestoreHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`RestoreHeadOutcome::NeedErrorBody`] with the response
    /// body. A key that holds no object is [`RestoreHeadOutcome::NotFound`].
    pub fn accept_restore_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> RestoreHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }
}

// The body of a RestoreObject. S3 takes the days only for a storage class
// that it restores a copy of, so the element is left out without them.
fn write_restore(out: &mut dyn ByteSink, plan: &PhysicalRestore<'_>) {
    out.push(RESTORE_OPEN);
    if let Some(days) = plan.days {
        out.push(b"<Days>");
        out.push(U64Decimal::new(days.into()).as_bytes());
        out.push(b"</Days>");
    }
    out.push(b"<GlacierJobParameters><Tier>");
    let tier = match plan.priority {
        RestorePriority::Bulk => "Bulk",
        RestorePriority::Standard => "Standard",
        RestorePriority::High => "Expedited",
    };
    out.push(tier.as_bytes());
    out.push(b"</Tier></GlacierJobParameters>");
    out.push(RESTORE_CLOSE);
}
