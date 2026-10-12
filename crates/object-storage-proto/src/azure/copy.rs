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
use crate::azure::template::{
    Describe, Detail, Field, FieldList, Operation, Template, Value, encode as encode_template,
    prepare as prepare_template,
};
use crate::azure::{
    AzureNamespace, Bearer, Blobs, Container, Write, azure_tag_char, body_kind, failed, named,
    names_failed_condition, push_metadata, push_stored, revision_parameter, validate_key,
    validate_metadata, validate_options,
};
use crate::azure::{stored_fields, utf8_properties};
use crate::common::{
    condition_header, validate_properties, validate_revision_item, validate_source_range,
    validate_template_options,
};
use crate::common::{
    encoded, finish_with_body, meta_of, push_condition, text_header, trim_ascii,
    validate_condition, validate_revision, validate_tags,
};
use crate::http::PlainHttp;
use crate::request::{ByteSink, HeadWriter, HeaderValue};
use crate::template::{Body, Hole, ParamValue, Values, WireRequest};
use crate::url;
use crate::url::QueryValue;
use crate::{
    ConditionKind, CopyHeadOutcome, CopyShape, CopySource, Failure, HeaderSpan, InvalidPlan,
    LegacyRequest, Method, ObjectMeta, Payload, PhysicalCopy, PutHeadOutcome, RequestedRange,
    ResponseFault, ResponseHead, Result, ServiceErrorKind, Timestamps, UpdateHeadOutcome,
};
use crate::{CopyItem, CopyOptions, Ranging, RevisionKind, WriteOptions};

/// One Put Block From URL: a block staged from the bytes of another blob,
/// which Azure reads itself.
///
/// Commit the block as you commit one that [`Blobs::encode_stage_block`]
/// staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalStageBlockFromUrl<'a> {
    /// The blob that the block belongs to, under the rules of
    /// [Object keys](crate#object-keys).
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
    ///   [`encode_put`](crate::azure::encode_put) refuses, for the target or the source.
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
    ) -> Result<LegacyRequest<'r>> {
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
    ) -> Result<LegacyRequest<'r>> {
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
        accept_copy_head(shape, head)
    }

    /// Finishes a [`CopyHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`accept_get_error_body`](crate::azure::accept_get_error_body) for a copy, and reads the body
    /// the same way.
    pub fn accept_copy_error_body<'h>(
        &self,
        shape: CopyShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> CopyHeadOutcome<'h> {
        accept_copy_error_body(shape, failure, body)
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
    ) -> Result<LegacyRequest<'r>> {
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
    /// This is [`accept_put_head`](crate::azure::accept_put_head) for a write from a URL: a 412 is
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
        accept_put_from_url_head(shape, head)
    }

    /// Finishes a [`PutHeadOutcome::NeedErrorBody`] of a Put Blob From URL
    /// with the response body.
    pub fn accept_put_from_url_error_body<'h>(
        &self,
        shape: CopyShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> PutHeadOutcome<'h> {
        accept_put_from_url_error_body(shape, failure, body)
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
    ) -> Result<LegacyRequest<'r>> {
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
    /// Returns [`Error::InvalidPlan`] for a key that [`encode_get`](crate::azure::encode_get)
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
    ) -> Result<LegacyRequest<'r>> {
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
        accept_abort_copy_head(head)
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

/// One Put Block From URL: a copy to the block `id` of the blob
/// `copy.key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageBlockFromUrlItem<'i> {
    /// The source, the target blob, and the bytes of the source.
    pub copy: CopyItem<'i>,
    /// The block ID, under the rules of [`BlockRef::id`](crate::azure::BlockRef::id).
    pub id: &'i str,
}

/// One Abort Copy Blob, which stops the copy `copy_id` to `key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbortCopyItem<'i> {
    /// The key of the copy's target.
    pub key: &'i str,
    /// The ID that Azure gave the copy, never empty.
    pub copy_id: &'i str,
}

// The copies of the template-based functions, and what each sends beside
// what every copy does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyKind {
    Copy,
    CopyFromUrl,
    PutFromUrl,
    StageBlock,
}

// Checks the options of a copy of `kind` against the rules of Azure.
fn validate_copy_options(
    options: &CopyOptions<'_>,
    kind: CopyKind,
    namespace: AzureNamespace,
) -> Result<()> {
    validate_source_location(options.source_endpoint, options.source_container)?;
    let staged = kind == CopyKind::StageBlock;
    // A block is no object: it takes a source, and nothing that an object
    // stores.
    if staged
        && (options.condition != ConditionKind::None
            || options.metadata.is_some()
            || options.tags.is_some()
            || options.options != WriteOptions::default())
    {
        return Err(InvalidPlan::Option.into());
    }
    if !staged && options.source_ranging == Ranging::Ranged {
        return Err(InvalidPlan::Range.into());
    }
    match options.metadata {
        Some([]) => return Err(InvalidPlan::Option.into()),
        Some(metadata) => validate_metadata(metadata)?,
        None => {}
    }
    // A copy names its tags apart, so that none can differ from the
    // source's, and has nothing of its own to sum.
    validate_template_options(&options.options)?;
    if !options.options.tags.is_empty() {
        return Err(InvalidPlan::Option.into());
    }
    // A Copy Blob takes the source's content properties.
    if matches!(kind, CopyKind::Copy | CopyKind::CopyFromUrl)
        && !options.options.properties.is_empty()
    {
        return Err(InvalidPlan::ContentProperty.into());
    }
    validate_properties(&options.options, utf8_properties(namespace))?;
    validate_tags(
        options.tags.unwrap_or(&[]),
        azure_tag_char,
        Some((10, 128, 256)),
    )
}

// Checks where a template's sources are: another account's origin of TLS,
// and a container that does not change the URL's structure.
fn validate_source_location(endpoint: Option<&str>, container: Option<&str>) -> Result<()> {
    if endpoint
        .is_some_and(|endpoint| !crate::http::valid_http_origin(endpoint, PlainHttp::Refused))
    {
        return Err(InvalidPlan::CopySource.into());
    }
    crate::common::validate_source_container(container)
}

// The field that carries a condition on a copy's source.
const fn source_condition_header(condition: ConditionKind) -> Option<&'static str> {
    match condition {
        ConditionKind::None => None,
        ConditionKind::IfMatch => Some("x-ms-source-if-match"),
        ConditionKind::IfNoneMatch => Some("x-ms-source-if-none-match"),
        ConditionKind::IfModifiedSince => Some("x-ms-source-if-modified-since"),
        ConditionKind::IfUnmodifiedSince => Some("x-ms-source-if-unmodified-since"),
    }
}

// Writes the template of a copy of `kind` with `options`.
fn prepare_copy_kind(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &CopyOptions<'_>,
    kind: CopyKind,
    buf: &mut [u8],
) -> Result<Template> {
    validate_copy_options(options, kind, namespace)?;
    let mut fields = FieldList::new();
    match kind {
        CopyKind::Copy | CopyKind::StageBlock => {}
        CopyKind::CopyFromUrl => {
            fields.push(Field::new("x-ms-requires-sync", Value::Bytes(b"true")))
        }
        CopyKind::PutFromUrl => {
            fields.push(Field::new("x-ms-blob-type", Value::Bytes(b"BlockBlob")))
        }
    }
    fields.push(Field::new(
        "x-ms-copy-source",
        Value::CopySource {
            origin: options.source_endpoint.unwrap_or(container.endpoint),
            container: options.source_container.unwrap_or(container.name),
            revision: options.source_revision,
        },
    ));
    if options.source_ranging == Ranging::Ranged {
        fields.push(Field::new(
            "x-ms-source-range",
            Value::Hole(Hole::SourceRange),
        ));
    }
    // A From URL operation reads the source as a client would, with the
    // client's token.
    if kind != CopyKind::Copy {
        fields.push(Field::new(
            "x-ms-copy-source-authorization",
            Value::Bearer(token),
        ));
    }
    if let Some(name) = source_condition_header(options.source_condition) {
        fields.push(Field::new(name, Value::Hole(Hole::SourceCondition)));
    }
    stored_fields(&mut fields, &options.options);
    if let Some(tags) = options.tags.filter(|tags| !tags.is_empty()) {
        fields.push(Field::new("x-ms-tags", Value::Tags(tags)));
    }
    if let Some(name) = condition_header(options.condition) {
        fields.push(Field::new(name, Value::Hole(Hole::Condition)));
    }
    fields.push(Field::new("content-length", Value::Bytes(b"0")));
    let query = [
        (kind == CopyKind::StageBlock)
            .then_some(("comp", ParamValue::Fixed(QueryValue::Literal("block")))),
        (kind == CopyKind::StageBlock).then_some(("blockid", ParamValue::Hole(Hole::BlockId))),
    ];
    let operation = match kind {
        CopyKind::Copy => Operation::Copy,
        CopyKind::CopyFromUrl => Operation::CopyFromUrl,
        CopyKind::PutFromUrl => Operation::PutFromUrl,
        CopyKind::StageBlock => Operation::StageBlockFromUrl,
    };
    let describe = Describe {
        method: Method::Put,
        key: true,
        query: &query,
        fields: fields.as_slice(),
        metadata: options.metadata.unwrap_or(&[]),
        body: Body::None,
    };
    prepare_template(
        container,
        token,
        namespace,
        &describe,
        operation,
        Detail::Copy {
            condition: options.condition,
            source_revision: options.source_revision,
            source_condition: options.source_condition,
            source_ranging: options.source_ranging,
        },
        buf,
    )
}

// Writes the copy `item` of `operation`, to the block `block_id` when it
// stages one.
fn encode_copy_kind<'r>(
    template: &Template,
    bytes: &[u8],
    operation: Operation,
    item: &CopyItem<'_>,
    block_id: Option<&str>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    let Detail::Copy {
        condition,
        source_revision,
        source_condition,
        source_ranging,
    } = template.detail
    else {
        return Err(InvalidPlan::Template.into());
    };
    if template.operation != operation {
        return Err(InvalidPlan::Template.into());
    }
    validate_key(item.key, template.namespace)?;
    validate_key(item.source_key, template.namespace)?;
    if let Some(id) = block_id {
        super::blocks::validate_block_id(id)?;
    }
    validate_revision_item(source_revision, item.source_revision)?;
    validate_condition(condition, item.condition_value)?;
    validate_condition(source_condition, item.source_condition_value)?;
    validate_source_range(source_ranging, item.source_range)?;
    let values = Values {
        now: Some(now),
        key: Some(item.key),
        condition: item.condition_value,
        source_key: Some(item.source_key),
        source_revision: item.source_revision,
        source_condition: item.source_condition_value,
        source_range: Some(item.source_range),
        block_id,
        ..Values::default()
    };
    encode_template(template, bytes, &values, Payload::Slice(&[]), buf)
}

/// Writes the template of Copy Blobs with `options` into `buf`. A Copy Blob
/// may finish after it answers: its outcome says whether it is pending.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for options that Azure refuses:
/// [`InvalidPlan::CopySource`] for a source origin that is not one of TLS,
/// or a source container that is empty or holds `/`, `?` or `#`;
/// [`InvalidPlan::ContentProperty`] for a content property, which a copy
/// takes from its source; [`InvalidPlan::Option`] for a checksum, for tags
/// in [`CopyOptions::options`], and for an empty set of metadata; and the
/// refusals of [`prepare_put`](crate::azure::prepare_put) for metadata and
/// tags.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_copy(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &CopyOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    prepare_copy_kind(container, token, namespace, options, CopyKind::Copy, buf)
}

/// Writes the template of Copy Blob From URLs with `options` into `buf`.
/// Each answers once its copy is written, and reads its source with the
/// template's token.
///
/// # Errors
///
/// As [`prepare_copy`].
pub fn prepare_copy_from_url(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &CopyOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    prepare_copy_kind(
        container,
        token,
        namespace,
        options,
        CopyKind::CopyFromUrl,
        buf,
    )
}

/// Writes the template of Put Blob From URLs with `options` into `buf`.
/// Each writes a block blob from the bytes of its source, read with the
/// template's token, and stores the template's content properties.
///
/// # Errors
///
/// As [`prepare_copy`], which takes content properties here.
pub fn prepare_put_from_url(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &CopyOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    prepare_copy_kind(
        container,
        token,
        namespace,
        options,
        CopyKind::PutFromUrl,
        buf,
    )
}

/// Writes the template of Put Block From URLs with `options` into `buf`.
/// Each stages a block from the bytes of its source, read with the
/// template's token.
///
/// # Errors
///
/// As [`prepare_copy`]. A block stores nothing of its own, so options
/// beyond the source's are [`InvalidPlan::Option`].
pub fn prepare_stage_block_from_url(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &CopyOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    prepare_copy_kind(
        container,
        token,
        namespace,
        options,
        CopyKind::StageBlock,
        buf,
    )
}

/// Writes the Copy Blob `item` at `now` from `template` into `buf`, and
/// returns it.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key of the target or the source
/// that Azure refuses, see [Object keys](crate#object-keys), for a revision
/// or a condition value that the template does not take, and for a template
/// of another operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_copy<'r>(
    template: &Template,
    bytes: &[u8],
    item: &CopyItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    encode_copy_kind(template, bytes, Operation::Copy, item, None, now, buf)
}

/// Writes the Copy Blob From URL `item` at `now` from `template` into
/// `buf`, and returns it.
///
/// # Errors
///
/// As [`encode_copy`].
pub fn encode_copy_from_url<'r>(
    template: &Template,
    bytes: &[u8],
    item: &CopyItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    encode_copy_kind(
        template,
        bytes,
        Operation::CopyFromUrl,
        item,
        None,
        now,
        buf,
    )
}

/// Writes the Put Blob From URL `item` at `now` from `template` into
/// `buf`, and returns it.
///
/// # Errors
///
/// As [`encode_copy`].
pub fn encode_put_from_url<'r>(
    template: &Template,
    bytes: &[u8],
    item: &CopyItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    encode_copy_kind(template, bytes, Operation::PutFromUrl, item, None, now, buf)
}

/// Writes the Put Block From URL `item` at `now` from `template` into
/// `buf`, and returns it. Read the response with
/// [`accept_stage_block_head`](crate::azure::accept_stage_block_head).
///
/// # Errors
///
/// As [`encode_copy`], with a block ID that breaks the rules of
/// [`BlockRef::id`](crate::azure::BlockRef::id), and a range of the source
/// that is empty, or that is not bounded where the template names one:
/// [`InvalidPlan::Range`], or [`InvalidPlan::UnsupportedRange`] for a
/// suffix or an offset.
pub fn encode_stage_block_from_url<'r>(
    template: &Template,
    bytes: &[u8],
    item: &StageBlockFromUrlItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    encode_copy_kind(
        template,
        bytes,
        Operation::StageBlockFromUrl,
        &item.copy,
        Some(item.id),
        now,
        buf,
    )
}

/// Reads the response head of a Copy Blob or a Copy Blob From URL and
/// reports what Azure did.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 202 is [`ResponseFault::Status`], and a 202 whose copy status
/// is neither `success` nor `pending` with an ID is [`ResponseFault::Head`].
pub fn accept_copy_head<'h>(
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
                Some(b"pending") if head.copy_id.is_some() => Ok(CopyHeadOutcome::Pending { meta }),
                _ => Err(ResponseFault::Head.into()),
            }
        }
        412 if carries_condition(shape) && named(&head) == Some(ServiceErrorKind::Precondition) => {
            Ok(CopyHeadOutcome::PreconditionFailed)
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`CopyHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_copy_error_body<'h>(
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

/// Reads the response head of a Put Blob From URL and reports what Azure
/// did.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 201 is [`ResponseFault::Status`].
pub fn accept_put_from_url_head<'h>(
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
        412 if carries_condition(shape) && named(&head) == Some(ServiceErrorKind::Precondition) => {
            Ok(PutHeadOutcome::PreconditionFailed)
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`PutHeadOutcome::NeedErrorBody`] of a Put Blob From URL with
/// the response body.
pub fn accept_put_from_url_error_body<'h>(
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

/// Writes the template of Abort Copy Blobs into `buf`.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_abort_copy(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    buf: &mut [u8],
) -> Result<Template> {
    let query = [
        Some(("comp", ParamValue::Fixed(QueryValue::Literal("copy")))),
        Some(("copyid", ParamValue::Hole(Hole::CopyId))),
    ];
    let fields = [
        Field::new("x-ms-copy-action", Value::Bytes(b"abort")),
        Field::new("content-length", Value::Bytes(b"0")),
    ];
    let describe = Describe {
        method: Method::Put,
        key: true,
        query: &query,
        fields: &fields,
        metadata: &[],
        body: Body::None,
    };
    prepare_template(
        container,
        token,
        namespace,
        &describe,
        Operation::AbortCopy,
        Detail::Object {
            condition: ConditionKind::None,
            revision: RevisionKind::Current,
        },
        buf,
    )
}

/// Writes the Abort Copy Blob `item` at `now` from `template` into `buf`,
/// and returns it.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), [`InvalidPlan::Option`] for an empty
/// copy ID, and [`InvalidPlan::Template`] for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_abort_copy<'r>(
    template: &Template,
    bytes: &[u8],
    item: &AbortCopyItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    if template.operation != Operation::AbortCopy {
        return Err(InvalidPlan::Template.into());
    }
    validate_key(item.key, template.namespace)?;
    if item.copy_id.is_empty() {
        return Err(InvalidPlan::Option.into());
    }
    let values = Values {
        now: Some(now),
        key: Some(item.key),
        copy_id: Some(item.copy_id),
        ..Values::default()
    };
    encode_template(template, bytes, &values, Payload::Slice(&[]), buf)
}

/// Reads the response head of an Abort Copy Blob and reports what Azure did.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 204 is [`ResponseFault::Status`].
pub fn accept_abort_copy_head<'h>(head: ResponseHead<'h>) -> Result<UpdateHeadOutcome<'h>> {
    super::tags::accept_update(head, &[204], ConditionKind::None)
}
