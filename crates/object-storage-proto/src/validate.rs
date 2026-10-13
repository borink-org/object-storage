// Checks of a request's options and of each item against its template, which
// both services make alike. Each refuses what it checks with the
// `InvalidRequest` that names it.

use crate::checksum::ChecksumKind;
use crate::time::MAX_UNIX_SECONDS;
use crate::{
    ChecksumValue, ConditionKind, ConditionValue, GetItem, GetKind, GetOptions, InvalidRequest,
    Ranging, RequestedRange, Result, Revision, RevisionKind, Tag, WriteOptions,
};

// Checks the snapshot or version that a request names: an identifier that is
// not empty, and a snapshot only where the service keeps snapshots.
pub(crate) fn validate_revision(
    revision: Option<Revision<'_>>,
    keeps_snapshots: bool,
) -> Result<()> {
    match revision {
        None => Ok(()),
        Some(Revision::Snapshot(_)) if !keeps_snapshots => Err(InvalidRequest::Revision.into()),
        Some(Revision::Snapshot(id) | Revision::Version(id)) if id.is_empty() => {
            Err(InvalidRequest::Revision.into())
        }
        Some(_) => Ok(()),
    }
}

// The kind and the value must agree in both directions: a kind without a value
// cannot be encoded, and a value without a kind would be dropped. An entity
// tag goes with the first two kinds and an instant with the dates, which must
// be one that an HTTP date can write.
pub(crate) fn validate_condition(
    condition: ConditionKind,
    value: Option<ConditionValue<'_>>,
) -> Result<()> {
    match (condition, value) {
        (ConditionKind::None, None) => Ok(()),
        (ConditionKind::IfMatch | ConditionKind::IfNoneMatch, Some(ConditionValue::ETag(tag)))
            if is_header_value(tag) =>
        {
            Ok(())
        }
        (
            ConditionKind::IfModifiedSince | ConditionKind::IfUnmodifiedSince,
            Some(ConditionValue::Time(seconds)),
        ) if seconds <= MAX_UNIX_SECONDS => Ok(()),
        _ => Err(InvalidRequest::Condition.into()),
    }
}

// Whether `value` can be sent as one header value as it is: ASCII, not
// empty, and without control characters.
pub(crate) fn is_header_value(value: &[u8]) -> bool {
    !value.is_empty() && value.is_ascii() && !value.iter().any(u8::is_ascii_control)
}

// Checks the content properties and the storage class of a write: each is
// one header value that the service stores as given, so no space at either
// end, which HTTP drops. A value is ASCII, except that the properties that
// `utf8` names, by lowercase header name, may hold UTF-8, which the service
// returns as it got it.
pub(crate) fn validate_properties(options: &WriteOptions<'_>, utf8: &[&str]) -> Result<()> {
    let stored_as_given = |value: &str, utf8: bool| {
        let bytes = value.as_bytes();
        let valid = if utf8 {
            !bytes.is_empty() && !bytes.iter().any(u8::is_ascii_control)
        } else {
            is_header_value(bytes)
        };
        valid && value.trim_ascii() == value
    };
    if options
        .properties
        .iter()
        .all(|(name, value)| stored_as_given(value, utf8.contains(&name)))
        && options
            .storage_class
            .is_none_or(|value| stored_as_given(value, false))
    {
        Ok(())
    } else {
        Err(InvalidRequest::ContentProperty.into())
    }
}

// Checks the tags of a write against the rules that `allowed` states for one
// character of a key or a value, and refuses two tags with the same key.
// `limits` is the most tags, and the most characters in a key and in a
// value, if the service has limits.
pub(crate) fn validate_tags(
    tags: &[Tag<'_>],
    allowed: impl Fn(char) -> bool,
    limits: Option<(usize, usize, usize)>,
) -> Result<()> {
    let within = |text: &str, most: usize| text.chars().count() <= most;
    for (index, tag) in tags.iter().enumerate() {
        let shaped = match limits {
            Some((_, key_len, value_len)) => {
                within(tag.key, key_len)
                    && within(tag.value, value_len)
                    && tag.key.chars().chain(tag.value.chars()).all(&allowed)
            }
            None => true,
        };
        if tag.key.is_empty() || !shaped || tags[..index].iter().any(|t| t.key == tag.key) {
            return Err(InvalidRequest::Tag.into());
        }
    }
    if limits.is_some_and(|(count, _, _)| tags.len() > count) {
        return Err(InvalidRequest::Tag.into());
    }
    Ok(())
}

// Checks that a template names a snapshot only for a service that keeps
// them: S3 keeps none.
pub(crate) fn validate_revision_kind(kind: RevisionKind, keeps_snapshots: bool) -> Result<()> {
    if kind == RevisionKind::Snapshot && !keeps_snapshots {
        return Err(InvalidRequest::Revision.into());
    }
    Ok(())
}

// Checks the text at which a listing groups its keys: never empty, and `/`
// alone where the service groups at nothing else, as on a hierarchical
// Azure account and an S3 directory bucket.
pub(crate) fn validate_delimiter(delimiter: Option<&str>, slash_only: bool) -> Result<()> {
    match delimiter {
        Some(text) if text.is_empty() || (slash_only && text != "/") => {
            Err(InvalidRequest::Delimiter.into())
        }
        _ => Ok(()),
    }
}

// Checks the options of a read: a HEAD reads no range, and a service that
// keeps no snapshots names none.
pub(crate) fn validate_get_options(options: &GetOptions, keeps_snapshots: bool) -> Result<()> {
    if options.kind == GetKind::Head && options.ranging == Ranging::Ranged {
        return Err(InvalidRequest::RangedHead.into());
    }
    validate_revision_kind(options.revision, keeps_snapshots)
}

// Checks one read against its template's options: a range exactly when the
// template sends one, and not empty; a condition value and a revision
// exactly when the template carries them.
pub(crate) fn validate_get_item(options: &GetOptions, item: &GetItem<'_>) -> Result<()> {
    let ranged = item.range != RequestedRange::Whole;
    if ranged != (options.ranging == Ranging::Ranged) {
        return Err(InvalidRequest::Range.into());
    }
    match item.range {
        RequestedRange::Bounded { start, end } if start >= end => {
            return Err(InvalidRequest::Range.into());
        }
        RequestedRange::Suffix(0) => return Err(InvalidRequest::Range.into()),
        _ => {}
    }
    validate_revision_item(options.revision, item.revision)?;
    validate_condition(options.condition, item.condition_value)
}

// Checks that a request names a snapshot or a version exactly when its
// template does, and that it is not empty.
pub(crate) fn validate_revision_item(kind: RevisionKind, revision: Option<&str>) -> Result<()> {
    match (kind, revision) {
        (RevisionKind::Current, None) => Ok(()),
        (RevisionKind::Current, Some(_)) | (_, None) => Err(InvalidRequest::Revision.into()),
        (_, Some("")) => Err(InvalidRequest::Revision.into()),
        (_, Some(_)) => Ok(()),
    }
}

// Checks one write's checksum against its template: present exactly when
// the template sends one, of the template's kind, and base64 text of a
// digest of that kind.
pub(crate) fn validate_checksum_item(
    kind: Option<ChecksumKind>,
    value: Option<ChecksumValue<'_>>,
) -> Result<()> {
    match (kind, value) {
        (None, None) => Ok(()),
        (Some(kind), Some(ChecksumValue::Base64(text))) => kind.check_base64(text),
        (Some(kind), Some(ChecksumValue::Digest(digest))) if digest.kind() == kind => Ok(()),
        _ => Err(InvalidRequest::Checksum.into()),
    }
}

// Checks the bytes of a source that a staged copy names: a bounded range,
// not empty, exactly when its template names one.
pub(crate) fn validate_source_range(ranging: Ranging, range: RequestedRange) -> Result<()> {
    match (ranging, range) {
        (Ranging::Whole, RequestedRange::Whole) => Ok(()),
        (Ranging::Ranged, RequestedRange::Bounded { start, end }) if start < end => Ok(()),
        (Ranging::Ranged, RequestedRange::Suffix(_) | RequestedRange::Offset(_)) => {
            Err(InvalidRequest::UnsupportedRange.into())
        }
        _ => Err(InvalidRequest::Range.into()),
    }
}

// Checks the container or bucket that holds a copy's sources: it goes into
// a URL or a header as it is, so it may not be empty or hold what would
// change their structure.
pub(crate) fn validate_source_container(container: Option<&str>) -> Result<()> {
    if let Some(container) = container
        && (container.is_empty()
            || container
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_control()))
    {
        return Err(InvalidRequest::CopySource.into());
    }
    Ok(())
}
