// Azure blob tags and access tiers: Set Blob Tags, Get Blob Tags and Set
// Blob Tier. A write stores tags and a tier with the blob as well, through
// `WriteOptions`.

#[cfg(doc)]
use crate::Error;
use crate::azure::{
    Blobs, azure_tag_char, body_kind, named, validate_azure_checksum, validate_key,
};
use crate::common::{
    decimal_header, encoded, encoded_with_body, failure, finish_with_body, missing, push_checksum,
    valid_header, validate_tags, write_tag_set,
};
use crate::request::{HeadWriter, U64Decimal, Writer};
use crate::url::QueryValue;
use crate::{
    Failure, HeaderSpan, InvalidPlan, Method, Payload, PhysicalSetTags, RequestedRange,
    ResponseFault, ResponseHead, Result, Tag, TagsHeadOutcome, Timestamps, TransactionalChecksum,
    UpdateHeadOutcome, WireRequest,
};

// The document that Set Blob Tags sends and Get Blob Tags answers with.
const TAGS_OPEN: &[u8] = b"<?xml version=\"1.0\" encoding=\"utf-8\"?><Tags>";
const TAGS_CLOSE: &[u8] = b"</Tags>";

impl<'a> Blobs<'a> {
    /// Writes the request head of a Set Blob Tier into `buf`, which moves an
    /// object to the access tier `tier`, such as `Cool`.
    ///
    /// The client sends any one header value as the tier, and Azure refuses
    /// a tier it does not have with 400 `InvalidHeaderValue`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key that [`Self::encode_get`]
    /// refuses, and [`InvalidPlan::ContentProperty`] for a tier that is not
    /// one header value.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::set_tier_requirements`](crate::layered::set_tier_requirements)
    /// first.
    pub fn encode_set_tier<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        key: &str,
        tier: &str,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(key, self.namespace)?;
        if !valid_header(tier.as_bytes()) || tier.trim_ascii() != tier {
            return Err(InvalidPlan::ContentProperty.into());
        }
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(key),
            &[Some(("comp", QueryValue::Literal("tier")))],
            RequestedRange::Whole,
            now,
        )?;
        head.header("x-ms-access-tier", |out| out.push(tier.as_bytes()));
        head.header("content-length", |out| out.push(b"0"));
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of a Set Blob Tier and reports what Azure
    /// did.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 and 202 is [`ResponseFault::Status`].
    pub fn accept_set_tier_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<UpdateHeadOutcome<'h>> {
        accept_update(head, &[200, 202])
    }

    /// Finishes an [`UpdateHeadOutcome::NeedErrorBody`] of a Set Blob Tier,
    /// a Set Blob Tags or an Abort Copy Blob with the response body.
    pub fn accept_update_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> UpdateHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Writes a Set Blob Tags into `buf`: the head, then the XML body after
    /// it. The object then holds these tags and no others.
    ///
    /// The body is written into `buf` after the head, so
    /// [`WireRequest::body_span`] names it and [`WireRequest::payload`]
    /// borrows it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key that [`Self::encode_get`]
    /// refuses, [`InvalidPlan::Tag`] for a tag that breaks the rules of
    /// [`Tag`], and [`InvalidPlan::Option`] for a checksum whose provider
    /// the client has not registered.
    ///
    /// Returns [`Error::Capacity`] with the bytes that the head and the body
    /// need together, or call
    /// [`layered::set_tags_requirements`](crate::layered::set_tags_requirements)
    /// first.
    pub fn encode_set_tags<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalSetTags<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key, self.namespace)?;
        validate_tags(plan.tags, azure_tag_char, Some((10, 128, 256)))?;
        let checksum = plan.checksum.map(TransactionalChecksum::Compute);
        validate_azure_checksum(checksum, true, &self.checksums)?;
        let mut counted = Writer::new(&mut []);
        write_tags_document(&mut counted, plan.tags);
        let length = counted.position();
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &[Some(("comp", QueryValue::Literal("tags")))],
            RequestedRange::Whole,
            now,
        )?;
        head.header("content-type", |out| out.push(b"application/xml"));
        head.header("content-length", |out| {
            out.push(U64Decimal::new(length as u64).as_bytes())
        });
        push_checksum(&mut head, checksum, &self.checksums, |sum| {
            write_tags_document(sum, plan.tags);
        });
        encoded_with_body(head, Method::Put, |out| write_tags_document(out, plan.tags))
    }

    /// Reads the response head of a Set Blob Tags and reports what Azure
    /// did. A failure is [`UpdateHeadOutcome::NeedErrorBody`] when the head
    /// names no error: pass the body to [`Self::accept_update_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 204 is [`ResponseFault::Status`].
    pub fn accept_set_tags_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<UpdateHeadOutcome<'h>> {
        accept_update(head, &[204])
    }

    /// Writes the request head of a Get Blob Tags into `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key that [`Self::encode_get`]
    /// refuses.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::get_tags_requirements`](crate::layered::get_tags_requirements)
    /// first.
    pub fn encode_get_tags<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        key: &str,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(key, self.namespace)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(key),
            &[Some(("comp", QueryValue::Literal("tags")))],
            RequestedRange::Whole,
            now,
        )?;
        encoded(head, Method::Get, Payload::Slice(&[]))
    }

    /// Reads the response head of a Get Blob Tags and reports what to do
    /// next.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`].
    pub fn accept_get_tags_head<'h>(&self, head: ResponseHead<'h>) -> Result<TagsHeadOutcome<'h>> {
        match head.status {
            200 => Ok(TagsHeadOutcome::Tags {
                expected_len: decimal_header(head.content_length)?,
            }),
            201..=299 => Err(ResponseFault::Status.into()),
            404 if head.error_code.is_some() => Ok(missing(&head, named(&head))),
            status if head.error_code.is_none() => Ok(TagsHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(TagsHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`TagsHeadOutcome::NeedErrorBody`] with the response body.
    pub fn accept_get_tags_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> TagsHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Reads the tags out of the response body of a Get Blob Tags into
    /// `into`, and returns how many it read.
    ///
    /// The body is decoded in place, and the tags borrow it. An array of ten
    /// tags holds any object's.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`] if the object holds more tags than the
    /// array, with `required` set to the number it holds.
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a `Tags` document.
    pub fn fill_tags<'b>(&self, body: &'b mut [u8], into: &mut [Tag<'b>]) -> Result<usize> {
        crate::xml::tags::fill_tags(body, b"Tags", into)
    }
}

fn write_tags_document(out: &mut dyn crate::request::ByteSink, tags: &[Tag<'_>]) {
    out.push(TAGS_OPEN);
    write_tag_set(out, tags);
    out.push(TAGS_CLOSE);
}

// Reads the head of a request that changes the object and returns nothing.
// Azure names the error of a failure in its head.
pub(super) fn accept_update<'h>(
    head: ResponseHead<'h>,
    success: &[u16],
) -> Result<UpdateHeadOutcome<'h>> {
    match head.status {
        status if success.contains(&status) => Ok(UpdateHeadOutcome::Updated),
        200..=299 => Err(ResponseFault::Status.into()),
        404 if head.error_code.is_some() => Ok(missing(&head, named(&head))),
        status if head.error_code.is_none() => Ok(UpdateHeadOutcome::NeedErrorBody(failure(
            status,
            None,
            head.request_id,
        ))),
        status => Ok(UpdateHeadOutcome::ServiceFailure(failure(
            status,
            named(&head),
            head.request_id,
        ))),
    }
}
