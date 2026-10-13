//! The options, the items and the shapes, as the protocol crate reads them.
//!
//! A number that names no variant is refused rather than read as another one.
//! [`crate::entry`] read every pointer that a value holds, and hands the bytes
//! here beside it.

#![forbid(unsafe_code)]

use crate::step::{optional, text};
use crate::types::*;

use borink_object_storage_proto as proto;
use borink_object_storage_proto::{Error, InvalidPlan, azure};

// A number that names no value of the protocol crate's enum. It is refused as an
// invalid plan, and the plan is never read as the value that happens to be
// oldest.
pub(crate) const UNKNOWN: Error = Error::InvalidPlan(InvalidPlan::Unknown);

pub(crate) fn condition_kind(condition: u16) -> proto::Result<proto::ConditionKind> {
    proto::ConditionKind::from_discriminant(condition).ok_or(UNKNOWN)
}

// A checksum kind, with 0 for none.
pub(crate) fn checksum_kind(kind: u16) -> proto::Result<Option<proto::ChecksumKind>> {
    match kind {
        0 => Ok(None),
        kind => proto::ChecksumKind::from_discriminant(kind)
            .map(Some)
            .ok_or(UNKNOWN),
    }
}

pub(crate) fn range(range: &Range) -> proto::Result<proto::RequestedRange> {
    Ok(proto::RequestedRange::from_parts(
        proto::RangeForm::from_discriminant(range.form).ok_or(UNKNOWN)?,
        range.start,
        range.end,
    ))
}

// What a condition compares against. `e_tag` is the bytes that its `e_tag`
// addresses.
pub(crate) fn condition_value<'a>(
    value: &ConditionValue,
    e_tag: &'a [u8],
) -> proto::Result<Option<proto::ConditionValue<'a>>> {
    match value.form {
        0 => Ok(None),
        1 => Ok(Some(proto::ConditionValue::ETag(e_tag))),
        2 => Ok(Some(proto::ConditionValue::Time(value.seconds))),
        _ => Err(UNKNOWN),
    }
}

// The content properties and the access tier, from the six values that
// `ptr::write_options` read. An empty value is none, and one that is not
// text is a property that Azure would not store as given.
pub(crate) fn write_options(values: [&[u8]; 6]) -> proto::Result<proto::WriteOptions<'_>> {
    let [
        content_type,
        content_encoding,
        content_language,
        content_disposition,
        cache_control,
        storage_class,
    ] = values.map(|value| {
        optional(value)
            .map(|value| text(value, InvalidPlan::ContentProperty))
            .transpose()
    });
    Ok(proto::WriteOptions {
        properties: proto::ContentProperties {
            content_type: content_type?,
            content_encoding: content_encoding?,
            content_language: content_language?,
            content_disposition: content_disposition?,
            cache_control: cache_control?,
        },
        storage_class: storage_class?,
        ..proto::WriteOptions::new()
    })
}

pub(crate) fn get_options(options: &GetOptions) -> proto::Result<proto::GetOptions> {
    Ok(proto::GetOptions {
        kind: proto::GetKind::from_discriminant(options.kind).ok_or(UNKNOWN)?,
        ranging: proto::Ranging::from_discriminant(options.ranging).ok_or(UNKNOWN)?,
        condition: condition_kind(options.condition)?,
        revision: proto::RevisionKind::from_discriminant(options.revision).ok_or(UNKNOWN)?,
    })
}

// A read, from the bytes that its key, its entity tag and its revision
// address.
pub(crate) fn get_item<'a>(
    item: &GetItem,
    key: &'a [u8],
    e_tag: &'a [u8],
    revision: &'a [u8],
) -> proto::Result<proto::GetItem<'a>> {
    Ok(proto::GetItem {
        key: text(key, InvalidPlan::KeyNotUtf8)?,
        range: range(&item.range)?,
        condition_value: condition_value(&item.condition_value, e_tag)?,
        revision: optional(revision)
            .map(|revision| text(revision, InvalidPlan::Revision))
            .transpose()?,
    })
}

pub(crate) fn get_shape(shape: &GetShape) -> proto::Result<proto::GetShape> {
    Ok(proto::GetShape {
        kind: proto::GetKind::from_discriminant(shape.kind).ok_or(UNKNOWN)?,
        range: range(&shape.range)?,
        condition: condition_kind(shape.condition)?,
    })
}

pub(crate) fn put_options<'a>(
    options: &PutOptions,
    write: [&'a [u8]; 6],
) -> proto::Result<proto::PutOptions<'a>> {
    Ok(proto::PutOptions {
        condition: condition_kind(options.condition)?,
        metadata: &[],
        options: write_options(write)?,
        checksum: checksum_kind(options.checksum)?,
    })
}

// A write, from the bytes that its key, its entity tag and its checksum
// address. The content stays in the caller's program, so only its length
// reaches the request.
pub(crate) fn put_item<'a>(
    item: &PutItem,
    key: &'a [u8],
    e_tag: &'a [u8],
    checksum: &'a [u8],
) -> proto::Result<proto::PutItem<'a>> {
    Ok(proto::PutItem {
        key: text(key, InvalidPlan::KeyNotUtf8)?,
        content: proto::Payload::Streamed {
            len: item.content_len,
        },
        condition_value: condition_value(&item.condition_value, e_tag)?,
        checksum: checksum_value(checksum)?,
    })
}

pub(crate) fn put_shape(shape: &PutShape) -> proto::Result<proto::PutShape> {
    Ok(proto::PutShape {
        condition: condition_kind(shape.condition)?,
    })
}

pub(crate) fn stage_options(options: &StageOptions) -> proto::Result<proto::StageOptions> {
    Ok(proto::StageOptions {
        checksum: checksum_kind(options.checksum)?,
    })
}

pub(crate) fn stage_item<'a>(
    item: &StageBlockItem,
    key: &'a [u8],
    id: &'a [u8],
    checksum: &'a [u8],
) -> proto::Result<azure::StageBlockItem<'a>> {
    Ok(azure::StageBlockItem {
        key: text(key, InvalidPlan::KeyNotUtf8)?,
        id: text(id, InvalidPlan::PartId)?,
        content: proto::Payload::Streamed {
            len: item.content_len,
        },
        checksum: checksum_value(checksum)?,
    })
}

pub(crate) fn commit_options<'a>(
    options: &CommitOptions,
    write: [&'a [u8]; 6],
) -> proto::Result<azure::CommitOptions<'a>> {
    Ok(azure::CommitOptions {
        condition: condition_kind(options.condition)?,
        metadata: &[],
        options: write_options(write)?,
        content_md5: options.content_md5,
    })
}

// A commit, from the bytes that its key, its entity tag and its MD5 address,
// and its blocks as `ptr::blocks` reads them. Every block is checked once
// here: an ID that is not text, or a source that this crate does not define,
// is refused before the protocol crate reads any, so the reads after this find
// none.
pub(crate) fn commit_item<'a, B>(
    item: &CommitItem,
    key: &'a [u8],
    e_tag: &'a [u8],
    content_md5: &'a [u8],
    blocks: B,
) -> proto::Result<azure::CommitItem<'a, impl Iterator<Item = azure::BlockRef<'a>> + Clone>>
where
    B: Iterator<Item = (&'a [u8], u16)> + Clone,
{
    for (id, source) in blocks.clone() {
        text(id, InvalidPlan::PartId)?;
        azure::BlockSource::from_discriminant(source).ok_or(UNKNOWN)?;
    }
    Ok(azure::CommitItem {
        key: text(key, InvalidPlan::KeyNotUtf8)?,
        condition_value: condition_value(&item.condition_value, e_tag)?,
        blocks: blocks.map(|(id, source)| azure::BlockRef {
            id: core::str::from_utf8(id).unwrap_or_default(),
            source: azure::BlockSource::from_discriminant(source).unwrap_or_default(),
        }),
        content_md5: optional(content_md5)
            .map(|md5| text(md5, InvalidPlan::Checksum))
            .transpose()?,
    })
}

pub(crate) fn commit_shape(shape: &CommitShape) -> proto::Result<proto::CommitShape> {
    Ok(proto::CommitShape {
        condition: condition_kind(shape.condition)?,
    })
}

// The flags that `borink_list_include` names, and the protocol crate's value of
// each. Any other bit is refused.
const INCLUDES: [(ListInclude, proto::ListInclude); 3] = [
    (ListInclude::Metadata, proto::ListInclude::METADATA),
    (ListInclude::Snapshots, proto::ListInclude::SNAPSHOTS),
    (ListInclude::Versions, proto::ListInclude::VERSIONS),
];

pub(crate) fn list_options<'a>(
    options: &ListOptions,
    delimiter: &'a [u8],
) -> proto::Result<proto::ListOptions<'a>> {
    let mut include = proto::ListInclude::default();
    let mut known = 0;
    for (bit, flag) in INCLUDES {
        known |= bit as u32;
        if options.include & bit as u32 != 0 {
            include = include | flag;
        }
    }
    if options.include & !known != 0 {
        return Err(UNKNOWN);
    }
    Ok(proto::ListOptions {
        delimiter: optional(delimiter)
            .map(|delimiter| text(delimiter, InvalidPlan::Delimiter))
            .transpose()?,
        max_results: options
            .max_results
            .present
            .then_some(options.max_results.value),
        include,
    })
}

// A page, from the bytes that its prefix and its marker address. An empty
// marker asks for the first page.
pub(crate) fn list_item<'a>(
    prefix: &'a [u8],
    marker: &'a [u8],
) -> proto::Result<proto::ListItem<'a>> {
    Ok(proto::ListItem {
        prefix: text(prefix, InvalidPlan::Prefix)?,
        marker: optional(marker)
            .map(|marker| text(marker, InvalidPlan::Marker).map(proto::ListMarker::Text))
            .transpose()?,
        start_after: None,
    })
}

// A checksum that a request sends as the base64 text it crossed as, or none
// for an empty one.
fn checksum_value(text_of: &[u8]) -> proto::Result<Option<proto::ChecksumValue<'_>>> {
    optional(text_of)
        .map(|value| text(value, InvalidPlan::Checksum).map(proto::ChecksumValue::Base64))
        .transpose()
}
