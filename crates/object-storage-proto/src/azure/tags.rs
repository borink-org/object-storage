// The Azure requests that change what Azure stores about a blob without
// writing its content: Set Blob Tags and Get Blob Tags, Set Blob Tier, Set
// Blob Metadata and Set Blob Properties. A write stores each of these with
// the blob as well, through its options and their `WriteOptions`.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::azure::template::{Field, FieldList, FieldValue, Operation, Template};
use crate::azure::{
    AzureNamespace, Container, ObjectText, azure_tag_char, encode_object, kind_in_body,
    kind_in_head, prepare_object, push_stored_fields, read_failure_head, utf8_properties,
    validate_metadata,
};
use crate::head::{read_decimal_header, read_text_header};
use crate::outcome::finish_with_body;
use crate::request::{ByteSink, write_tag_set};
use crate::template::WireRequest;
use crate::validate::{is_header_value, validate_properties, validate_tags};
use crate::{
    ConditionKind, ContentProperties, Failure, InvalidRequest, MetadataPair, ObjectItem,
    ObjectMeta, ResponseFault, ResponseHead, Result, RevisionKind, ServiceErrorKind, Tag,
    TagsHeadOutcome, Timestamps, UpdateHeadOutcome, WriteOptions,
};

// The document that Set Blob Tags sends and Get Blob Tags answers with.
const TAGS_OPEN: &[u8] = b"<?xml version=\"1.0\" encoding=\"utf-8\"?><Tags>";
const TAGS_CLOSE: &[u8] = b"</Tags>";

fn write_tags_document(out: &mut dyn ByteSink, tags: &[Tag<'_>]) {
    out.push(TAGS_OPEN);
    write_tag_set(out, tags);
    out.push(TAGS_CLOSE);
}

// Reads the head of a request that changes the object and returns no body.
// Azure names the error of a failure in its head. A 412 is the request's failed
// `condition` only if it carried one and Azure names a failed condition.
pub(super) fn accept_update_head<'h>(
    head: ResponseHead<'h>,
    success: &[u16],
    condition: ConditionKind,
) -> Result<UpdateHeadOutcome<'h>> {
    match head.status {
        status if success.contains(&status) => Ok(UpdateHeadOutcome::Updated {
            meta: ObjectMeta {
                last_modified: read_text_header(head.last_modified)?,
                ..head.object_meta()
            },
        }),
        412 if condition != ConditionKind::None
            && kind_in_head(&head) == Some(ServiceErrorKind::Precondition) =>
        {
            Ok(UpdateHeadOutcome::PreconditionFailed)
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

/// What every Set Blob Metadata of a template does alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetMetadataOptions<'o> {
    /// The metadata pairs that each object then holds, and no others, under
    /// the rules of [`PutOptions::metadata`](crate::PutOptions::metadata).
    /// None removes every pair.
    pub metadata: &'o [MetadataPair<'o>],
    /// The condition on the object, as a write carries it.
    pub condition: ConditionKind,
}

/// What every Set Blob Properties of a template does alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetPropertiesOptions<'o> {
    /// The content properties that each object then holds, and no others,
    /// under the rules of [`ContentProperties`]. Azure clears every one that
    /// this leaves out, its stored `Content-MD5` as well.
    pub properties: ContentProperties<'o>,
    /// The condition on the object, as a write carries it.
    pub condition: ConditionKind,
}

/// Writes the template of Set Blob Tiers that move objects of `container`
/// to the access tier `tier`, such as `Cool`, into `buf`.
///
/// The template sends any one header value as the tier, and Azure refuses a
/// tier it does not have with 400 `InvalidHeaderValue`. Write each request
/// with [`encode_set_tier`].
///
/// # Errors
///
/// Returns [`Error::InvalidToken`] for a token that is not one header value,
/// and [`InvalidRequest::ContentProperty`] for a tier that is not one either.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_set_tier(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    tier: &str,
    buf: &mut [u8],
) -> Result<Template> {
    if !is_header_value(tier.as_bytes()) || tier.trim_ascii() != tier {
        return Err(InvalidRequest::ContentProperty.into());
    }
    let fields = [
        Field::new("x-ms-access-tier", FieldValue::Bytes(tier.as_bytes())),
        Field::new("content-length", FieldValue::Bytes(b"0")),
    ];
    prepare_object(
        container,
        token,
        namespace,
        Operation::SetTier,
        ConditionKind::None,
        RevisionKind::Current,
        &ObjectText {
            fields: &fields,
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the Set Blob Tier of `item` at `now` from `template` into `buf`,
/// and returns where it is.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), for a condition value or a revision,
/// which the template does not carry, and for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_set_tier(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<WireRequest> {
    encode_object(template, bytes, Operation::SetTier, item, now, buf)
}

/// Reads the response head of a Set Blob Tier and reports what Azure did.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 and 202 is [`ResponseFault::Status`].
pub fn accept_set_tier_head<'h>(head: ResponseHead<'h>) -> Result<UpdateHeadOutcome<'h>> {
    accept_update_head(head, &[200, 202], ConditionKind::None)
}

/// Writes the template of Set Blob Metadatas with `options` into `buf`.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a metadata pair that
/// [`prepare_put`](crate::azure::prepare_put) refuses.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_set_metadata(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &SetMetadataOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    validate_metadata(options.metadata)?;
    prepare_object(
        container,
        token,
        namespace,
        Operation::SetMetadata,
        options.condition,
        RevisionKind::Current,
        &ObjectText {
            fields: &[Field::new("content-length", FieldValue::Bytes(b"0"))],
            metadata: options.metadata,
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the Set Blob Metadata of `item` at `now` from `template` into
/// `buf`, and returns where it is.
///
/// # Errors
///
/// As [`encode_set_tier`], with the condition value that the template's
/// condition needs.
pub fn encode_set_metadata(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<WireRequest> {
    encode_object(template, bytes, Operation::SetMetadata, item, now, buf)
}

/// Reads the response head of a Set Blob Metadata and reports what Azure
/// did. A success carries the object's new entity tag.
///
/// Pass the condition of the template. A 412 is
/// [`UpdateHeadOutcome::PreconditionFailed`] only if it carries one and
/// Azure names a failed condition.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_set_metadata_head<'h>(
    condition: ConditionKind,
    head: ResponseHead<'h>,
) -> Result<UpdateHeadOutcome<'h>> {
    accept_update_head(head, &[200], condition)
}

/// Writes the template of Set Blob Properties with `options` into `buf`.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a content property that
/// [`prepare_put`](crate::azure::prepare_put) refuses.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_set_properties(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &SetPropertiesOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    let stored = WriteOptions {
        properties: options.properties,
        ..WriteOptions::new()
    };
    validate_properties(&stored, utf8_properties(namespace))?;
    let mut fields = FieldList::new();
    push_stored_fields(&mut fields, &stored);
    fields.push(Field::new("content-length", FieldValue::Bytes(b"0")));
    prepare_object(
        container,
        token,
        namespace,
        Operation::SetProperties,
        options.condition,
        RevisionKind::Current,
        &ObjectText {
            fields: fields.as_slice(),
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the Set Blob Properties of `item` at `now` from `template` into
/// `buf`, and returns where it is.
///
/// # Errors
///
/// As [`encode_set_metadata`].
pub fn encode_set_properties(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<WireRequest> {
    encode_object(template, bytes, Operation::SetProperties, item, now, buf)
}

/// Reads the response head of a Set Blob Properties and reports what Azure
/// did, as [`accept_set_metadata_head`] does.
///
/// # Errors
///
/// As [`accept_set_metadata_head`].
pub fn accept_set_properties_head<'h>(
    condition: ConditionKind,
    head: ResponseHead<'h>,
) -> Result<UpdateHeadOutcome<'h>> {
    accept_update_head(head, &[200], condition)
}

/// Finishes an [`UpdateHeadOutcome::NeedErrorBody`] of a Set Blob Tier, a
/// Set Blob Tags, a Set Blob Metadata, a Set Blob Properties or an Abort
/// Copy Blob with the response body.
pub fn accept_update_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> UpdateHeadOutcome<'h> {
    finish_with_body(failure, kind_in_body(body))
}

/// Writes the template of Set Blob Tags that give objects `tags` and no
/// others into `buf`. Each request names a snapshot or a version of the
/// kind `revision`, or the object as it is now.
///
/// The template holds the body, which each request sends.
///
/// # Errors
///
/// Returns [`InvalidRequest::Tag`] for a tag that breaks the rules of [`Tag`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_set_tags(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    tags: &[Tag<'_>],
    revision: RevisionKind,
    buf: &mut [u8],
) -> Result<Template> {
    validate_tags(tags, azure_tag_char, Some((10, 128, 256)))?;
    let write = |out: &mut dyn ByteSink| write_tags_document(out, tags);
    prepare_object(
        container,
        token,
        namespace,
        Operation::SetTags,
        ConditionKind::None,
        revision,
        &ObjectText {
            fields: &[Field::new(
                "content-type",
                FieldValue::Bytes(b"application/xml"),
            )],
            body: Some(&write),
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the Set Blob Tags of `item` at `now` from `template` into `buf`,
/// with its body after the head, and returns where it is.
///
/// # Errors
///
/// As [`encode_set_tier`], with the revision that the template names.
pub fn encode_set_tags(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<WireRequest> {
    encode_object(template, bytes, Operation::SetTags, item, now, buf)
}

/// Reads the response head of a Set Blob Tags and reports what Azure did.
/// A failure is [`UpdateHeadOutcome::NeedErrorBody`] when the head names no
/// error: pass the body to [`accept_update_error_body`].
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 204 is [`ResponseFault::Status`].
pub fn accept_set_tags_head<'h>(head: ResponseHead<'h>) -> Result<UpdateHeadOutcome<'h>> {
    accept_update_head(head, &[204], ConditionKind::None)
}

/// Writes the template of Get Blob Tags into `buf`. Each request names a
/// snapshot or a version of the kind `revision`, or the object as it is now.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_get_tags(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    revision: RevisionKind,
    buf: &mut [u8],
) -> Result<Template> {
    prepare_object(
        container,
        token,
        namespace,
        Operation::GetTags,
        ConditionKind::None,
        revision,
        &ObjectText::default(),
        buf,
    )
}

/// Writes the Get Blob Tags of `item` at `now` from `template` into `buf`,
/// and returns where it is.
///
/// # Errors
///
/// As [`encode_set_tags`].
pub fn encode_get_tags(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<WireRequest> {
    encode_object(template, bytes, Operation::GetTags, item, now, buf)
}

/// Reads the response head of a Get Blob Tags and reports what to do next.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_get_tags_head<'h>(head: ResponseHead<'h>) -> Result<TagsHeadOutcome<'h>> {
    match head.status {
        200 => Ok(TagsHeadOutcome::Tags {
            expected_len: read_decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

/// Finishes a [`TagsHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_get_tags_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> TagsHeadOutcome<'h> {
    finish_with_body(failure, kind_in_body(body))
}

/// Reads the tags out of the response body of a Get Blob Tags into `into`,
/// and returns how many it read.
///
/// The body is decoded in place, and the tags borrow it. An array of ten
/// tags holds any object's.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if the object holds more tags than the array,
/// with `required` set to the number it holds.
///
/// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is not
/// a `Tags` document.
pub fn fill_tags<'b>(body: &'b mut [u8], into: &mut [Tag<'b>]) -> Result<usize> {
    crate::xml::tags::fill_tags(body, b"Tags", into)
}
