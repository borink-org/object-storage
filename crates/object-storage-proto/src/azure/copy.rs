// Azure copies, which the service carries out without sending the bytes
// through the client: Copy Blob, which may finish later; Copy Blob From URL
// and Put Blob From URL, which answer once the copy is written; Put Block From
// URL, which stages a block from a range of another blob; and Abort Copy
// Blob, which stops a copy that is pending.
//
// Each names its source by URL in `x-ms-copy-source`. The From URL
// operations read that URL as a client would, so they send the client's
// token in `x-ms-copy-source-authorization` as well.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::azure::{
    AzureNamespace, Bearer, Blobs, Container, Write, azure_tag_char, body_kind, named,
    names_failed_condition, push_metadata, push_stored, revision_parameter, validate_key,
    validate_metadata, validate_options,
};
use crate::common::{
    encoded, failure, finish_with_body, meta_of, missing, push_condition, text_header, trim_ascii,
    validate_condition, validate_revision, validate_tags,
};
use crate::http::PlainHttp;
use crate::request::{ByteSink, HeadWriter, HeaderValue};
use crate::url;
use crate::{
    ConditionKind, CopyHeadOutcome, CopyShape, CopySource, Failure, HeaderSpan, InvalidPlan,
    Method, ObjectMeta, Payload, PhysicalCopy, PutHeadOutcome, RequestedRange, ResponseFault,
    ResponseHead, Result, ServiceErrorKind, Timestamps, UpdateHeadOutcome, WireRequest,
};

/// One Put Block From URL: a block staged from the bytes of another blob,
/// which Azure reads itself.
///
/// Commit the block as you commit one that [`Blobs::encode_stage_block`]
/// staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalStageBlockFromUrl<'a> {
    /// The blob that the block belongs to, under the rules of
    /// [`PhysicalGet::key`](crate::PhysicalGet::key).
    pub key: &'a str,
    /// The block ID, under the rules of
    /// [`PhysicalStageBlock::id`](crate::azure::PhysicalStageBlock::id).
    pub id: &'a str,
    /// The blob whose bytes the block holds.
    pub source: CopySource<'a>,
    /// The bytes of the source that the block holds:
    /// [`RequestedRange::Whole`], or a [`RequestedRange::Bounded`] range,
    /// sent in `x-ms-source-range`. A client refuses any other range with
    /// [`InvalidPlan::UnsupportedRange`], and an empty one with
    /// [`InvalidPlan::Range`].
    pub range: RequestedRange,
}

impl<'a> PhysicalStageBlockFromUrl<'a> {
    /// Creates a plan that stages all of `source` as the block `id` of
    /// `key`.
    pub const fn new(key: &'a str, id: &'a str, source: CopySource<'a>) -> Self {
        Self {
            key,
            id,
            source,
            range: RequestedRange::Whole,
        }
    }
}

impl<'a> Blobs<'a> {
    /// Writes the request head of a Copy Blob into `buf`, which copies an
    /// object of the same account onto `plan.key`.
    ///
    /// Azure may finish the copy later: read the response with
    /// [`Self::accept_copy_head`], which says whether it is done. Azure
    /// authorizes the source with the client's token, as it does the target.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `plan` cannot become an Azure
    /// request:
    ///
    /// - A key, a metadata pair, a tag or a storage class that
    ///   [`Self::encode_put`] refuses, for the target or the source.
    /// - [`InvalidPlan::ContentProperty`] for a content property: a copy
    ///   takes the source's.
    /// - [`InvalidPlan::Option`] for an empty list of metadata, which Azure
    ///   cannot copy with, and for tags, a checksum or a declared MD5 in the
    ///   options.
    /// - [`InvalidPlan::CopySource`] for a source container that cannot be
    ///   written into a URL, and [`InvalidPlan::Revision`] for an empty
    ///   snapshot or version.
    /// - [`InvalidPlan::Condition`] for an invalid condition on either.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::copy_requirements`](crate::layered::copy_requirements)
    /// first.
    pub fn encode_copy<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCopy<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        self.validate_copy(plan, Write::Copy)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, Some(plan.key), &[], RequestedRange::Whole, now)?;
        self.push_copy(&mut head, plan);
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Writes the request head of a Copy Blob From URL into `buf`: the copy
    /// of [`Self::encode_copy`], with `x-ms-requires-sync: true`, which
    /// Azure answers once the copy is written.
    ///
    /// Azure copies a source of at most 256 MiB this way. It reads the
    /// source by URL, so the request sends the client's token in
    /// `x-ms-copy-source-authorization` too. The token must allow reading
    /// the source.
    ///
    /// # Errors
    ///
    /// As [`Self::encode_copy`], or call
    /// [`layered::copy_from_url_requirements`](crate::layered::copy_from_url_requirements)
    /// first.
    pub fn encode_copy_from_url<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCopy<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        self.validate_copy(plan, Write::Copy)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, Some(plan.key), &[], RequestedRange::Whole, now)?;
        head.header("x-ms-requires-sync", b"true");
        self.push_source_authorization(&mut head);
        self.push_copy(&mut head, plan);
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of a Copy Blob or a Copy Blob From URL and
    /// reports what Azure did.
    ///
    /// Pass the shape of the plan. A copy that Azure finished is
    /// [`CopyHeadOutcome::Copied`], and one that it finishes later is
    /// [`CopyHeadOutcome::Pending`]. A 412 is
    /// [`CopyHeadOutcome::PreconditionFailed`] only if the plan carried a
    /// condition and Azure names a failed one, `ConditionNotMet` for the
    /// target or `SourceConditionNotMet` for the source.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 202 is [`ResponseFault::Status`], and a 202 whose
    /// `x-ms-copy-status` is neither `success` nor `pending` with a copy ID
    /// is [`ResponseFault::Head`].
    pub fn accept_copy_head<'h>(
        &self,
        shape: CopyShape,
        head: ResponseHead<'h>,
    ) -> Result<CopyHeadOutcome<'h>> {
        match head.status {
            202 => {
                let meta = ObjectMeta {
                    last_modified: text_header(head.last_modified)?,
                    ..meta_of(head)
                };
                match head.copy_status.map(trim_ascii) {
                    Some(b"success") => Ok(CopyHeadOutcome::Copied { meta }),
                    Some(b"pending") if head.copy_id.is_some() => {
                        Ok(CopyHeadOutcome::Pending { meta })
                    }
                    _ => Err(ResponseFault::Head.into()),
                }
            }
            412 if carries_condition(shape)
                && named(&head) == Some(ServiceErrorKind::Precondition) =>
            {
                Ok(CopyHeadOutcome::PreconditionFailed)
            }
            404 if head.error_code.is_some() => Ok(missing(&head, named(&head))),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(CopyHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(CopyHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`CopyHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_get_error_body`] for a copy, and reads the body
    /// the same way.
    pub fn accept_copy_error_body<'h>(
        &self,
        shape: CopyShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> CopyHeadOutcome<'h> {
        let kind = body_kind(body);
        if names_failed_condition(failure.status, carries_condition(shape), kind) {
            return CopyHeadOutcome::PreconditionFailed;
        }
        finish_with_body(failure, kind)
    }

    /// Writes the request head of a Put Blob From URL into `buf`, which
    /// writes a block blob from the bytes of another blob. Azure answers
    /// once the blob is written.
    ///
    /// Azure writes a source of at most 5000 MiB this way, and reads it by
    /// URL, so the request sends the client's token in
    /// `x-ms-copy-source-authorization` too. Unlike a copy, the write takes
    /// the content properties of the plan.
    ///
    /// # Errors
    ///
    /// As [`Self::encode_copy`], except that the plan may carry content
    /// properties, or call
    /// [`layered::put_from_url_requirements`](crate::layered::put_from_url_requirements)
    /// first.
    pub fn encode_put_from_url<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalCopy<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        self.validate_copy(plan, Write::FromUrl)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, Some(plan.key), &[], RequestedRange::Whole, now)?;
        head.header("x-ms-blob-type", b"BlockBlob");
        self.push_source_authorization(&mut head);
        self.push_copy(&mut head, plan);
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of a Put Blob From URL and reports what Azure
    /// did.
    ///
    /// This is [`Self::accept_put_head`] for a write from a URL: a 412 is
    /// [`PutHeadOutcome::PreconditionFailed`] if the plan carried a condition
    /// on the target or on the source and Azure names a failed one. A source
    /// that does not exist is a [`PutHeadOutcome::ServiceFailure`] with
    /// [`ServiceErrorKind::NotFound`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 201 is [`ResponseFault::Status`].
    pub fn accept_put_from_url_head<'h>(
        &self,
        shape: CopyShape,
        head: ResponseHead<'h>,
    ) -> Result<PutHeadOutcome<'h>> {
        match head.status {
            201 => Ok(PutHeadOutcome::Created {
                meta: ObjectMeta {
                    last_modified: text_header(head.last_modified)?,
                    ..meta_of(head)
                },
            }),
            412 if carries_condition(shape)
                && named(&head) == Some(ServiceErrorKind::Precondition) =>
            {
                Ok(PutHeadOutcome::PreconditionFailed)
            }
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(PutHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(PutHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`PutHeadOutcome::NeedErrorBody`] of a Put Blob From URL
    /// with the response body.
    pub fn accept_put_from_url_error_body<'h>(
        &self,
        shape: CopyShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> PutHeadOutcome<'h> {
        let kind = body_kind(body);
        if names_failed_condition(failure.status, carries_condition(shape), kind) {
            return PutHeadOutcome::PreconditionFailed;
        }
        PutHeadOutcome::ServiceFailure(crate::common::failure(
            failure.status,
            kind,
            failure.request_id,
        ))
    }

    /// Writes the request head of a Put Block From URL into `buf`, which
    /// stages a block from bytes of another blob.
    ///
    /// Azure reads the source by URL, so the request sends the client's
    /// token in `x-ms-copy-source-authorization` too. Read the response with
    /// [`Self::accept_stage_block_head`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key or a block ID that
    /// [`Self::encode_stage_block`] refuses, a source that
    /// [`Self::encode_copy`] refuses, and a range that
    /// [`PhysicalStageBlockFromUrl::range`] does not allow.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::stage_block_from_url_requirements`](crate::layered::stage_block_from_url_requirements)
    /// first.
    pub fn encode_stage_block_from_url<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalStageBlockFromUrl<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key, self.namespace)?;
        super::blocks::validate_block_id(plan.id)?;
        validate_source(&plan.source, self.namespace)?;
        match plan.range {
            RequestedRange::Whole => {}
            RequestedRange::Bounded { start, end } if start >= end => {
                return Err(InvalidPlan::Range.into());
            }
            RequestedRange::Bounded { .. } => {}
            _ => return Err(InvalidPlan::UnsupportedRange.into()),
        }
        let query = [
            url::literal("comp", "block"),
            url::encoded("blockid", plan.id),
        ];
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &query,
            RequestedRange::Whole,
            now,
        )?;
        head.header("x-ms-copy-source", self.source_url(&plan.source));
        if plan.range != RequestedRange::Whole {
            head.header("x-ms-source-range", plan.range);
        }
        self.push_source_authorization(&mut head);
        push_source_condition(&mut head, &plan.source);
        head.header("content-length", b"0");
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Writes the request head of an Abort Copy Blob into `buf`, which stops
    /// the pending copy `copy_id` onto `key` and leaves the target empty.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key that [`Self::encode_get`]
    /// refuses, and [`InvalidPlan::Option`] for an empty copy ID.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::abort_copy_requirements`](crate::layered::abort_copy_requirements)
    /// first.
    pub fn encode_abort_copy<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        key: &str,
        copy_id: &str,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(key, self.namespace)?;
        if copy_id.is_empty() {
            return Err(InvalidPlan::Option.into());
        }
        let query = [
            url::literal("comp", "copy"),
            url::encoded("copyid", copy_id),
        ];
        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, Some(key), &query, RequestedRange::Whole, now)?;
        head.header("x-ms-copy-action", b"abort");
        head.header("content-length", b"0");
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of an Abort Copy Blob and reports what Azure
    /// did.
    ///
    /// A copy that is no longer pending is refused with 409
    /// `NoPendingCopyOperation`, which reaches you as a service failure.
    /// Finish a [`UpdateHeadOutcome::NeedErrorBody`] with
    /// [`Self::accept_update_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 204 is [`ResponseFault::Status`].
    pub fn accept_abort_copy_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<UpdateHeadOutcome<'h>> {
        super::tags::accept_update(head, &[204], ConditionKind::None)
    }

    // Checks a copy plan. A Copy Blob takes the source's content properties,
    // and a Put Blob From URL takes the plan's.
    fn validate_copy(&self, plan: &PhysicalCopy<'_>, write: Write) -> Result<()> {
        validate_key(plan.key, self.namespace)?;
        validate_source(&plan.source, self.namespace)?;
        match plan.metadata {
            Some([]) => return Err(InvalidPlan::Option.into()),
            Some(metadata) => validate_metadata(metadata)?,
            None => {}
        }
        validate_options(&plan.options, write, false, self)?;
        validate_tags(
            plan.tags.unwrap_or(&[]),
            azure_tag_char,
            Some((10, 128, 256)),
        )?;
        validate_condition(plan.condition, plan.condition_value)
    }

    // The headers that every copy carries beside its own: the source, the
    // conditions on it and on the target, and what the target stores.
    fn push_copy(&self, head: &mut HeadWriter<'_>, plan: &PhysicalCopy<'_>) {
        head.header("x-ms-copy-source", self.source_url(&plan.source));
        push_source_condition(head, &plan.source);
        push_stored(head, &plan.options);
        if let Some(tags) = plan.tags.filter(|tags| !tags.is_empty()) {
            head.header("x-ms-tags", tags);
        }
        push_metadata(head, plan.metadata.unwrap_or(&[]));
        push_condition(head, plan.condition, plan.condition_value);
        head.header("content-length", b"0");
    }

    // The URL of `source`, as `x-ms-copy-source` names it.
    fn source_url<'c>(&self, source: &'c CopySource<'c>) -> SourceUrl<'c>
    where
        'a: 'c,
    {
        SourceUrl {
            container: self.container,
            source,
        }
    }

    // A From URL operation reads the source as a client would, with the
    // client's token.
    fn push_source_authorization(&self, head: &mut HeadWriter<'_>) {
        head.header("x-ms-copy-source-authorization", Bearer(self.token));
    }
}

// The URL of a copy's source: the client's origin, the source's container or
// the client's own, the key encoded as a path, and the snapshot or version.
struct SourceUrl<'c> {
    // The client's container, whose origin the URL starts with, and which
    // holds the source unless the source names a container of its own.
    container: Container<'c>,
    source: &'c CopySource<'c>,
}

impl HeaderValue for SourceUrl<'_> {
    fn write_to(self, out: &mut dyn ByteSink) {
        let SourceUrl { container, source } = self;
        out.push(source.endpoint.unwrap_or(container.endpoint).as_bytes());
        out.push(b"/");
        out.push(source.container.unwrap_or(container.name).as_bytes());
        out.push(b"/");
        for part in url::encode_object_key(source.key) {
            out.push(part);
        }
        url::write_query_in_url(out, &[revision_parameter(source.revision)]);
    }
}

// Whether a copy carried a condition on the target or on the source.
fn carries_condition(shape: CopyShape) -> bool {
    shape.condition != ConditionKind::None || shape.source_condition != ConditionKind::None
}

// A source container goes into the URL as it is, so it may not hold what
// would change the URL's structure, as `Container::new` requires of the
// client's own.
fn validate_source(source: &CopySource<'_>, namespace: AzureNamespace) -> Result<()> {
    // Another account's origin goes into the URL as it is, and Azure reads
    // it as a client would, so it is an origin of TLS.
    if source
        .endpoint
        .is_some_and(|endpoint| !crate::http::valid_http_origin(endpoint, PlainHttp::Refused))
    {
        return Err(InvalidPlan::CopySource.into());
    }
    if let Some(container) = source.container
        && (container.is_empty()
            || container
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_control()))
    {
        return Err(InvalidPlan::CopySource.into());
    }
    validate_key(source.key, namespace)?;
    validate_revision(source.revision, true)?;
    validate_condition(source.condition, source.condition_value)
}

fn push_source_condition(head: &mut HeadWriter<'_>, source: &CopySource<'_>) {
    let name = match source.condition {
        ConditionKind::None => return,
        ConditionKind::IfMatch => "x-ms-source-if-match",
        ConditionKind::IfNoneMatch => "x-ms-source-if-none-match",
        ConditionKind::IfModifiedSince => "x-ms-source-if-modified-since",
        ConditionKind::IfUnmodifiedSince => "x-ms-source-if-unmodified-since",
    };
    let value = source.condition_value.expect("the plan was validated");
    head.header(name, value);
}
