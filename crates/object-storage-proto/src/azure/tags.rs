// The Azure requests that change what Azure stores about a blob without
// writing its content: Set Blob Tags and Get Blob Tags, Set Blob Tier, Set
// Blob Metadata and Set Blob Properties. A write stores each of these with
// the blob as well, through its plan and its `WriteOptions`.

// Only the links in the doc comments use this, so it is imported for rustdoc
// alone: a normal build would report it unused.
#[cfg(doc)]
use crate::Error;
use crate::azure::template::{Field, Operation, Template, Value};
use crate::azure::{
    AzureNamespace, Blobs, Container, ObjectRequest, azure_tag_char, body_kind, encode_object,
    failed, named, prepare_object, push_metadata, push_stored, revision_parameter, stored_fields,
    utf8_properties, validate_azure_checksum, validate_key, validate_metadata,
};
use crate::common::{
    decimal_header, encoded, encoded_with_body, finish_with_body, meta_of, push_checksum,
    push_condition, text_header, valid_header, validate_condition, validate_properties,
    validate_revision, validate_tags, write_tag_set,
};
use crate::request::{HeadWriter, U64Decimal, Writer};
use crate::template::{Body, Hole, WireRequest};
use crate::url;
use crate::{
    Condition, ConditionKind, ConditionValue, ContentProperties, Failure, HeaderSpan, InvalidPlan,
    LegacyRequest, MetadataPair, Method, ObjectItem, ObjectMeta, Payload, PhysicalSetTags,
    RequestedRange, ResponseFault, ResponseHead, Result, Revision, RevisionKind, ServiceErrorKind,
    Tag, TagsHeadOutcome, Timestamps, TransactionalChecksum, UpdateHeadOutcome, WriteOptions,
};

/// A Set Blob Metadata: the metadata pairs that an object then holds, and no
/// others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalSetMetadata<'a> {
    /// The object key, under the rules of
    /// [Object keys](crate#object-keys).
    pub key: &'a str,
    /// The metadata pairs, under the rules of
    /// [`PutOptions::metadata`](crate::PutOptions::metadata). None removes
    /// every pair.
    pub metadata: &'a [MetadataPair<'a>],
    /// The condition on the object, as a write carries it.
    pub condition: ConditionKind,
    /// What `condition` compares against: see [`ConditionValue`].
    pub condition_value: Option<ConditionValue<'a>>,
}

impl<'a> PhysicalSetMetadata<'a> {
    /// Creates a plan that gives `key` these metadata pairs, with no
    /// condition.
    pub const fn new(key: &'a str, metadata: &'a [MetadataPair<'a>]) -> Self {
        Self {
            key,
            metadata,
            condition: ConditionKind::None,
            condition_value: None,
        }
    }

    /// Returns this plan with `condition`, which sets [`Self::condition`]
    /// and [`Self::condition_value`] together.
    pub const fn with_condition(mut self, condition: Condition<'a>) -> Self {
        let (kind, value) = condition.split();
        self.condition = kind;
        self.condition_value = value;
        self
    }
}

/// A Set Blob Properties: the content properties that an object then holds,
/// and no others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalSetProperties<'a> {
    /// The object key, under the rules of
    /// [Object keys](crate#object-keys).
    pub key: &'a str,
    /// The content properties, under the rules of [`ContentProperties`].
    /// Azure clears every one that this leaves out.
    pub properties: ContentProperties<'a>,
    /// The condition on the object, as a write carries it.
    pub condition: ConditionKind,
    /// What `condition` compares against: see [`ConditionValue`].
    pub condition_value: Option<ConditionValue<'a>>,
}

impl<'a> PhysicalSetProperties<'a> {
    /// Creates a plan that gives `key` these content properties, with no
    /// condition.
    pub const fn new(key: &'a str, properties: ContentProperties<'a>) -> Self {
        Self {
            key,
            properties,
            condition: ConditionKind::None,
            condition_value: None,
        }
    }

    /// Returns this plan with `condition`, which sets [`Self::condition`]
    /// and [`Self::condition_value`] together.
    pub const fn with_condition(mut self, condition: Condition<'a>) -> Self {
        let (kind, value) = condition.split();
        self.condition = kind;
        self.condition_value = value;
        self
    }
}

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
    /// Returns [`Error::InvalidPlan`] for a key that [`encode_get`](crate::azure::encode_get)
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
    ) -> Result<LegacyRequest<'r>> {
        validate_key(key, self.namespace)?;
        if !valid_header(tier.as_bytes()) || tier.trim_ascii() != tier {
            return Err(InvalidPlan::ContentProperty.into());
        }
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(key),
            &[url::literal("comp", "tier")],
            RequestedRange::Whole,
            now,
        )?;
        head.header("x-ms-access-tier", tier.as_bytes());
        head.header("content-length", b"0");
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
        accept_set_tier_head(head)
    }

    /// Writes the request head of a Set Blob Metadata into `buf`. The object
    /// then holds these metadata pairs and no others: a plan with none
    /// removes them all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key, a metadata pair or a
    /// condition that [`encode_put`](crate::azure::encode_put) refuses.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::set_metadata_requirements`](crate::layered::set_metadata_requirements)
    /// first.
    pub fn encode_set_metadata<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalSetMetadata<'_>,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
        validate_key(plan.key, self.namespace)?;
        validate_metadata(plan.metadata)?;
        validate_condition(plan.condition, plan.condition_value)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &[url::literal("comp", "metadata")],
            RequestedRange::Whole,
            now,
        )?;
        push_metadata(&mut head, plan.metadata);
        push_condition(&mut head, plan.condition, plan.condition_value);
        head.header("content-length", b"0");
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of a Set Blob Metadata and reports what Azure
    /// did. A success carries the object's new entity tag.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`].
    pub fn accept_set_metadata_head<'h>(
        &self,
        condition: ConditionKind,
        head: ResponseHead<'h>,
    ) -> Result<UpdateHeadOutcome<'h>> {
        accept_set_metadata_head(condition, head)
    }

    /// Writes the request head of a Set Blob Properties into `buf`. The
    /// object then holds these content properties and no others: Azure clears
    /// every one the plan leaves out, its stored `Content-MD5` as well.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key, a content property or a
    /// condition that [`encode_put`](crate::azure::encode_put) refuses.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::set_properties_requirements`](crate::layered::set_properties_requirements)
    /// first.
    pub fn encode_set_properties<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalSetProperties<'_>,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
        validate_key(plan.key, self.namespace)?;
        let options = WriteOptions {
            properties: plan.properties,
            ..WriteOptions::new()
        };
        validate_properties(&options, utf8_properties(self.namespace))?;
        validate_condition(plan.condition, plan.condition_value)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &[url::literal("comp", "properties")],
            RequestedRange::Whole,
            now,
        )?;
        push_stored(&mut head, &options);
        push_condition(&mut head, plan.condition, plan.condition_value);
        head.header("content-length", b"0");
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of a Set Blob Properties and reports what
    /// Azure did. A success carries the object's new entity tag.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`].
    pub fn accept_set_properties_head<'h>(
        &self,
        condition: ConditionKind,
        head: ResponseHead<'h>,
    ) -> Result<UpdateHeadOutcome<'h>> {
        accept_set_properties_head(condition, head)
    }

    /// Finishes an [`UpdateHeadOutcome::NeedErrorBody`] of a Set Blob Tier,
    /// a Set Blob Tags, a Set Blob Metadata, a Set Blob Properties or an
    /// Abort Copy Blob with the response body.
    pub fn accept_update_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> UpdateHeadOutcome<'h> {
        accept_update_error_body(failure, body)
    }

    /// Writes a Set Blob Tags into `buf`: the head, then the XML body after
    /// it. The object then holds these tags and no others.
    ///
    /// The body is written into `buf` after the head, so
    /// [`LegacyRequest::body_span`] names it and [`LegacyRequest::payload`]
    /// borrows it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key that [`encode_get`](crate::azure::encode_get)
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
    ) -> Result<LegacyRequest<'r>> {
        validate_key(plan.key, self.namespace)?;
        validate_revision(plan.revision, true)?;
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
            &[
                url::literal("comp", "tags"),
                revision_parameter(plan.revision),
            ],
            RequestedRange::Whole,
            now,
        )?;
        head.header("content-type", b"application/xml");
        head.header("content-length", U64Decimal::new(length as u64).as_bytes());
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
        accept_set_tags_head(head)
    }

    /// Writes the request head of a Get Blob Tags into `buf`, which reads the
    /// tags of `key`, or of the snapshot or version that `revision` names.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key or a revision that
    /// [`encode_get`](crate::azure::encode_get) refuses.
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
        revision: Option<Revision<'_>>,
        now: &Timestamps,
    ) -> Result<LegacyRequest<'r>> {
        validate_key(key, self.namespace)?;
        validate_revision(revision, true)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(key),
            &[url::literal("comp", "tags"), revision_parameter(revision)],
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
        accept_get_tags_head(head)
    }

    /// Finishes a [`TagsHeadOutcome::NeedErrorBody`] with the response body.
    pub fn accept_get_tags_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> TagsHeadOutcome<'h> {
        accept_get_tags_error_body(failure, body)
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
        fill_tags(body, into)
    }
}

fn write_tags_document(out: &mut dyn crate::request::ByteSink, tags: &[Tag<'_>]) {
    out.push(TAGS_OPEN);
    write_tag_set(out, tags);
    out.push(TAGS_CLOSE);
}

// Reads the head of a request that changes the object and returns no body.
// Azure names the error of a failure in its head. A 412 is the plan's failed
// `condition` only if it carried one and Azure names a failed condition.
pub(super) fn accept_update<'h>(
    head: ResponseHead<'h>,
    success: &[u16],
    condition: ConditionKind,
) -> Result<UpdateHeadOutcome<'h>> {
    match head.status {
        status if success.contains(&status) => Ok(UpdateHeadOutcome::Updated {
            meta: ObjectMeta {
                last_modified: text_header(head.last_modified)?,
                ..meta_of(head)
            },
        }),
        412 if condition != ConditionKind::None
            && named(&head) == Some(ServiceErrorKind::Precondition) =>
        {
            Ok(UpdateHeadOutcome::PreconditionFailed)
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
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
/// and [`InvalidPlan::ContentProperty`] for a tier that is not one either.
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
    if !valid_header(tier.as_bytes()) || tier.trim_ascii() != tier {
        return Err(InvalidPlan::ContentProperty.into());
    }
    let mut request = ObjectRequest {
        comp: Some("tier"),
        ..ObjectRequest::new(Operation::SetTier, Method::Put)
    };
    request.fields.push(Field::new(
        "x-ms-access-tier",
        Value::Bytes(tier.as_bytes()),
    ));
    request
        .fields
        .push(Field::new("content-length", Value::Bytes(b"0")));
    prepare_object(container, token, namespace, request, buf)
}

/// Writes the Set Blob Tier of `item` at `now` from `template` into `buf`,
/// and returns it.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), for a condition value or a revision,
/// which the template does not carry, and for a template of another
/// operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_set_tier<'r>(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    encode_object(template, bytes, Operation::SetTier, item, now, buf)
}

/// Reads the response head of a Set Blob Tier and reports what Azure did.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 and 202 is [`ResponseFault::Status`].
pub fn accept_set_tier_head<'h>(head: ResponseHead<'h>) -> Result<UpdateHeadOutcome<'h>> {
    accept_update(head, &[200, 202], ConditionKind::None)
}

/// Writes the template of Set Blob Metadatas with `options` into `buf`.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a metadata pair that
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
    let mut request = ObjectRequest {
        comp: Some("metadata"),
        condition: options.condition,
        metadata: options.metadata,
        ..ObjectRequest::new(Operation::SetMetadata, Method::Put)
    };
    request
        .fields
        .push(Field::new("content-length", Value::Bytes(b"0")));
    prepare_object(container, token, namespace, request, buf)
}

/// Writes the Set Blob Metadata of `item` at `now` from `template` into
/// `buf`, and returns it.
///
/// # Errors
///
/// As [`encode_set_tier`], with the condition value that the template's
/// condition needs.
pub fn encode_set_metadata<'r>(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
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
    accept_update(head, &[200], condition)
}

/// Writes the template of Set Blob Properties with `options` into `buf`.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] for a content property that
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
    let mut request = ObjectRequest {
        comp: Some("properties"),
        condition: options.condition,
        ..ObjectRequest::new(Operation::SetProperties, Method::Put)
    };
    stored_fields(&mut request.fields, &stored);
    request
        .fields
        .push(Field::new("content-length", Value::Bytes(b"0")));
    prepare_object(container, token, namespace, request, buf)
}

/// Writes the Set Blob Properties of `item` at `now` from `template` into
/// `buf`, and returns it.
///
/// # Errors
///
/// As [`encode_set_metadata`].
pub fn encode_set_properties<'r>(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
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
    accept_update(head, &[200], condition)
}

/// Finishes an [`UpdateHeadOutcome::NeedErrorBody`] of a Set Blob Tier, a
/// Set Blob Tags, a Set Blob Metadata, a Set Blob Properties or an Abort
/// Copy Blob with the response body.
pub fn accept_update_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> UpdateHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Writes the template of Set Blob Tags that give objects `tags` and no
/// others into `buf`. Each request names a snapshot or a version of the
/// kind `revision`, or the object as it is now.
///
/// The template holds the body, which each request sends.
///
/// # Errors
///
/// Returns [`InvalidPlan::Tag`] for a tag that breaks the rules of [`Tag`].
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
    let write = |out: &mut dyn crate::request::ByteSink| write_tags_document(out, tags);
    let mut request = ObjectRequest {
        comp: Some("tags"),
        revision,
        body: Body::Fixed(&write),
        ..ObjectRequest::new(Operation::SetTags, Method::Put)
    };
    request
        .fields
        .push(Field::new("content-type", Value::Bytes(b"application/xml")));
    request
        .fields
        .push(Field::new("content-length", Value::Hole(Hole::BodyLength)));
    prepare_object(container, token, namespace, request, buf)
}

/// Writes the Set Blob Tags of `item` at `now` from `template` into `buf`,
/// with its body after the head, and returns it.
///
/// # Errors
///
/// As [`encode_set_tier`], with the revision that the template names.
pub fn encode_set_tags<'r>(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
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
    accept_update(head, &[204], ConditionKind::None)
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
    let request = ObjectRequest {
        comp: Some("tags"),
        revision,
        ..ObjectRequest::new(Operation::GetTags, Method::Get)
    };
    prepare_object(container, token, namespace, request, buf)
}

/// Writes the Get Blob Tags of `item` at `now` from `template` into `buf`,
/// and returns it.
///
/// # Errors
///
/// As [`encode_set_tags`].
pub fn encode_get_tags<'r>(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
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
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`TagsHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_get_tags_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> TagsHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
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
