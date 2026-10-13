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
    CopyFrom, Describe, Detail, Field, FieldList, Holes, Operation, Template, Value,
    encode as encode_template, prepare as prepare_template,
};
use crate::azure::{
    AzureNamespace, Container, azure_tag_char, body_kind, failed, named, names_existing_object,
    names_failed_condition, validate_key, validate_metadata,
};
use crate::azure::{stored_fields, utf8_properties};
use crate::common::{
    condition_header, validate_properties, validate_revision_item, validate_source_range,
};
use crate::common::{
    finish_with_body, meta_of, text_header, trim_ascii, validate_condition, validate_tags,
};
use crate::http::PlainHttp;
use crate::template::{Body, Hole, ParamValue, Values, WireRequest};
use crate::url::QueryValue;
use crate::{
    ConditionKind, CopyHeadOutcome, CopyShape, Failure, InvalidRequest, Method, ObjectMeta,
    Payload, PutHeadOutcome, ResponseFault, ResponseHead, Result, ServiceErrorKind, Timestamps,
    UpdateHeadOutcome,
};
use crate::{CopyItem, CopyOptions, MetadataPair, Ranging, RevisionKind, WriteOptions};

// Whether a copy carried a condition on the target or on the source.
fn carries_condition(shape: CopyShape) -> bool {
    shape.condition != ConditionKind::None || shape.source_condition != ConditionKind::None
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

impl CopyKind {
    // The operation of a template of copies of this kind.
    const fn operation(self) -> Operation {
        match self {
            Self::Copy => Operation::Copy,
            Self::CopyFromUrl => Operation::CopyFromUrl,
            Self::PutFromUrl => Operation::PutFromUrl,
            Self::StageBlock => Operation::StageBlockFromUrl,
        }
    }
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
        return Err(InvalidRequest::Option.into());
    }
    if !staged && options.source_ranging == Ranging::Ranged {
        return Err(InvalidRequest::Range.into());
    }
    match options.metadata {
        Some([]) => return Err(InvalidRequest::Option.into()),
        Some(metadata) => validate_metadata(metadata)?,
        None => {}
    }
    // A copy names its tags apart, so that none can differ from the
    // source's, and has nothing of its own to sum.
    if !options.options.tags.is_empty() {
        return Err(InvalidRequest::Option.into());
    }
    // A Copy Blob takes the source's content properties.
    if matches!(kind, CopyKind::Copy | CopyKind::CopyFromUrl)
        && !options.options.properties.is_empty()
    {
        return Err(InvalidRequest::ContentProperty.into());
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
        return Err(InvalidRequest::CopySource.into());
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
    // A From URL operation reads the source as a client would, with the
    // client's token.
    if kind != CopyKind::Copy {
        fields.push(Field::new(
            "x-ms-copy-source-authorization",
            Value::Bearer(token),
        ));
    }
    stored_fields(&mut fields, &options.options);
    if let Some(tags) = options.tags.filter(|tags| !tags.is_empty()) {
        fields.push(Field::new("x-ms-tags", Value::Tags(tags)));
    }
    fields.push(Field::new("content-length", Value::Bytes(b"0")));
    let detail = Detail::Copy {
        condition: options.condition,
        source_revision: options.source_revision,
        source_condition: options.source_condition,
        source_ranging: options.source_ranging,
    };
    let text = CopyText {
        fields: fields.as_slice(),
        metadata: options.metadata.unwrap_or(&[]),
        origin: options.source_endpoint.unwrap_or(container.endpoint),
        container: options.source_container.unwrap_or(container.name),
    };
    prepare_template(
        container,
        token,
        namespace,
        &describe_copy(kind, detail, Some(&text)),
        kind.operation(),
        detail,
        buf,
        |_| {},
    )
}

// What every copy of a template sends alike, beside its structure: the
// fixed fields and the metadata, and where its sources are.
struct CopyText<'p> {
    fields: &'p [Field<'p>],
    metadata: &'p [MetadataPair<'p>],
    origin: &'p str,
    container: &'p str,
}

// The request of a copy of `kind` for a template of `detail`. `text` is
// the template's, and none for a request.
fn describe_copy<'p>(
    kind: CopyKind,
    detail: Detail,
    text: Option<&CopyText<'p>>,
) -> Describe<'p, 2> {
    let Detail::Copy {
        condition,
        source_revision,
        source_condition,
        source_ranging,
    } = detail
    else {
        unreachable!("a copy's template has the detail of a copy");
    };
    let staged = kind == CopyKind::StageBlock;
    Describe {
        method: Method::Put,
        key: true,
        query: [
            staged.then_some(("comp", ParamValue::Fixed(QueryValue::Literal("block")))),
            staged.then_some(("blockid", ParamValue::Hole(Hole::BlockId))),
        ],
        holes: Holes {
            copy_source: Some(CopyFrom {
                origin: text.map_or("", |text| text.origin),
                container: text.map_or("", |text| text.container),
                revision: source_revision,
            }),
            source_range: source_ranging == Ranging::Ranged,
            source_condition: source_condition_header(source_condition),
            condition: condition_header(condition),
            ..Holes::default()
        },
        fields: text.map_or(&[], |text| text.fields),
        metadata: text.map_or(&[], |text| text.metadata),
        body: Body::None,
    }
}

// Writes the copy `item` of `operation`, to the block `block_id` when it
// stages one.
fn encode_copy_kind<'r>(
    template: &Template,
    bytes: &[u8],
    kind: CopyKind,
    item: &CopyItem<'_>,
    block_id: Option<&str>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    let detail @ Detail::Copy {
        condition,
        source_revision,
        source_condition,
        source_ranging,
    } = template.detail
    else {
        return Err(InvalidRequest::Template.into());
    };
    if template.operation != kind.operation() {
        return Err(InvalidRequest::Template.into());
    }
    let key = template.request_key(bytes, item.key)?;
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
        key,
        condition: item.condition_value,
        source_key: Some(item.source_key),
        source_revision: item.source_revision,
        source_condition: item.source_condition_value,
        source_range: Some(item.source_range),
        block_id,
        ..Values::default()
    };
    encode_template(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_copy(kind, detail, None),
    )
}

/// Writes the template of Copy Blobs with `options` into `buf`. A Copy Blob
/// may finish after it answers: its outcome says whether it is pending.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for options that Azure refuses:
/// [`InvalidRequest::CopySource`] for a source origin that is not one of TLS,
/// or a source container that is empty or holds `/`, `?` or `#`;
/// [`InvalidRequest::ContentProperty`] for a content property, which a copy
/// takes from its source; [`InvalidRequest::Option`] for a checksum, for tags
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
/// beyond the source's are [`InvalidRequest::Option`].
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
/// Returns [`Error::InvalidRequest`] for a key of the target or the source
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
    encode_copy_kind(template, bytes, CopyKind::Copy, item, None, now, buf)
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
    encode_copy_kind(template, bytes, CopyKind::CopyFromUrl, item, None, now, buf)
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
    encode_copy_kind(template, bytes, CopyKind::PutFromUrl, item, None, now, buf)
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
/// [`InvalidRequest::Range`], or [`InvalidRequest::UnsupportedRange`] for a
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
        CopyKind::StageBlock,
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
        409 if names_existing_object(409, shape.condition, named(&head)) => {
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
    if names_failed_condition(failure.status, carries_condition(shape), kind)
        || names_existing_object(failure.status, shape.condition, kind)
    {
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
        409 if names_existing_object(409, shape.condition, named(&head)) => {
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
    if names_failed_condition(failure.status, carries_condition(shape), kind)
        || names_existing_object(failure.status, shape.condition, kind)
    {
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
    let fields = [
        Field::new("x-ms-copy-action", Value::Bytes(b"abort")),
        Field::new("content-length", Value::Bytes(b"0")),
    ];
    prepare_template(
        container,
        token,
        namespace,
        &describe_abort_copy(&fields),
        Operation::AbortCopy,
        Detail::Object {
            condition: ConditionKind::None,
            revision: RevisionKind::Current,
            delete: crate::DeleteKind::Object,
        },
        buf,
        |_| {},
    )
}

// The request of an Abort Copy Blob, which sends `fields`.
fn describe_abort_copy<'p>(fields: &'p [Field<'p>]) -> Describe<'p, 2> {
    Describe {
        method: Method::Put,
        key: true,
        query: [
            Some(("comp", ParamValue::Fixed(QueryValue::Literal("copy")))),
            Some(("copyid", ParamValue::Hole(Hole::CopyId))),
        ],
        holes: Holes::default(),
        fields,
        metadata: &[],
        body: Body::None,
    }
}

/// Writes the Abort Copy Blob `item` at `now` from `template` into `buf`,
/// and returns it.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), [`InvalidRequest::Option`] for an empty
/// copy ID, and [`InvalidRequest::Template`] for a template of another
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
        return Err(InvalidRequest::Template.into());
    }
    let key = template.request_key(bytes, item.key)?;
    if item.copy_id.is_empty() {
        return Err(InvalidRequest::Option.into());
    }
    let values = Values {
        now: Some(now),
        key,
        copy_id: Some(item.copy_id),
        ..Values::default()
    };
    encode_template(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_abort_copy(&[]),
    )
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
