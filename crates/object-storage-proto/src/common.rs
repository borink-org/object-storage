// What both providers share when they write a request head and read a
// response head.

use crate::checksum::ChecksumKind;
use crate::request::{ByteSink, HeaderValue, U64Decimal};
use crate::{
    BodyWindow, ConditionKind, ConditionValue, Error, Failure, FailureClass, GetHeadOutcome,
    GetKind, GetShape, InvalidRequest, ObjectMeta, RequestedRange, ResponseFault, ResponseHead,
    Result, Revision, ServiceErrorKind, Tag, Timestamps, WriteOptions,
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

// The one record that every failing head becomes, whichever operation asked.
pub(crate) fn failure<'h>(
    status: u16,
    kind: Option<ServiceErrorKind>,
    request_id: Option<&'h [u8]>,
) -> Failure<'h> {
    Failure {
        status,
        class: failure_class(status, kind),
        kind,
        request_id,
    }
}

// The two outcomes that `finish_with_body` produces, for one operation's
// outcome type. An outcome whose request names no object, such as a
// listing, has no `NotFound`: its 404 can only mean a missing container,
// so `not_found` returns the failure.
pub(crate) trait FailureOutcome<'h> {
    fn need_error_body(failure: Failure<'h>) -> Self;
    fn not_found(failure: Failure<'h>) -> Self;
    fn service_failure(failure: Failure<'h>) -> Self;
}

// Finishes a failure with the error `kind` that its head or body named. A
// 404 is not found, unless it names a missing container. That is a failure
// of the service, as any other status is: a caller must never read a
// missing container as a missing object.
pub(crate) fn finish_with_body<'h, O: FailureOutcome<'h>>(
    head_failure: Failure<'h>,
    kind: Option<ServiceErrorKind>,
) -> O {
    let failure = failure(head_failure.status, kind, head_failure.request_id);
    if failure.status == 404 && kind != Some(ServiceErrorKind::NoSuchContainer) {
        O::not_found(failure)
    } else {
        O::service_failure(failure)
    }
}

macro_rules! failure_outcome {
    ($($outcome:ident),*) => {$(
        impl<'h> FailureOutcome<'h> for crate::$outcome<'h> {
            fn need_error_body(failure: Failure<'h>) -> Self {
                Self::NeedErrorBody(failure)
            }

            fn not_found(failure: Failure<'h>) -> Self {
                Self::NotFound { kind: failure.kind }
            }

            fn service_failure(failure: Failure<'h>) -> Self {
                Self::ServiceFailure(failure)
            }
        }
    )*};
}

// The outcomes whose request names no object, which report every 404 as a
// failure of the service.
macro_rules! container_failure_outcome {
    ($($outcome:ty),*) => {$(
        impl<'h> FailureOutcome<'h> for $outcome {
            fn need_error_body(failure: Failure<'h>) -> Self {
                Self::NeedErrorBody(failure)
            }

            fn not_found(failure: Failure<'h>) -> Self {
                Self::ServiceFailure(failure)
            }

            fn service_failure(failure: Failure<'h>) -> Self {
                Self::ServiceFailure(failure)
            }
        }
    )*};
}
pub(crate) use container_failure_outcome;

failure_outcome!(
    CopyHeadOutcome,
    RestoreHeadOutcome,
    GetHeadOutcome,
    DeleteHeadOutcome,
    StageHeadOutcome,
    CommitHeadOutcome,
    ListPartsHeadOutcome,
    UpdateHeadOutcome,
    TagsHeadOutcome
);

container_failure_outcome!(
    crate::PutHeadOutcome<'h>,
    crate::ListHeadOutcome<'h>,
    crate::DeleteManyHeadOutcome<'h>
);

// The outcome of a 404 whose head names its error, as Azure's does.
pub(crate) fn missing<'h, O: FailureOutcome<'h>>(
    head: &ResponseHead<'h>,
    kind: Option<ServiceErrorKind>,
) -> O {
    finish_with_body(failure(404, None, head.request_id), kind)
}

// The metadata that a response head states, without its size and with
// `Last-Modified` unread: `text_header` reads it.
pub(crate) fn meta_of(head: ResponseHead<'_>) -> ObjectMeta<'_> {
    ObjectMeta {
        size: None,
        e_tag: head.e_tag,
        last_modified: None,
        version: head.version,
        content_encoding: head.content_encoding,
        content_type: head.content_type,
        content_md5: head.content_md5,
        content_language: head.content_language,
        content_disposition: head.content_disposition,
        cache_control: head.cache_control,
        storage_class: head.storage_class,
        copy_id: head.copy_id,
        copy_status: head.copy_status,
        restore_status: head.restore_status,
    }
}

pub(crate) fn accept_success<'h>(
    shape: GetShape,
    head: ResponseHead<'h>,
) -> Result<GetHeadOutcome<'h>> {
    let content_length = decimal_header(head.content_length)?;
    let last_modified = text_header(head.last_modified)?;
    let meta = |size| ObjectMeta {
        size,
        last_modified,
        ..meta_of(head)
    };
    if head.status == 200 {
        // An unranged read reads from byte zero, and the service states the whole
        // object length, so `Content-Length` is both the window and the size.
        return Ok(match shape.kind {
            GetKind::Head => GetHeadOutcome::Complete {
                meta: meta(content_length),
            },
            GetKind::Bytes => GetHeadOutcome::Body {
                meta: meta(content_length),
                body: BodyWindow {
                    object_offset: 0,
                    expected_len: content_length,
                    object_size: content_length,
                },
            },
        });
    }
    let value = head
        .content_range
        .ok_or(Error::Response(ResponseFault::Head))?;
    let ContentRange::Satisfied { start, end, total } =
        parse_content_range(value).ok_or(Error::Response(ResponseFault::Head))?
    else {
        return Err(ResponseFault::Head.into());
    };
    // parse_content_range establishes start <= end < u64::MAX.
    let served = end - start + 1;
    if content_length.is_some_and(|length| length != served) {
        return Err(ResponseFault::Head.into());
    }
    // Both services serve the whole satisfiable range, so a short serve is a
    // mismatch: silently accepting it would hand consumers a partial read.
    let requested_start = match shape.range {
        RequestedRange::Bounded { start, .. } | RequestedRange::Offset(start) => start,
        // Where a suffix starts depends on the size, so the head must state it.
        RequestedRange::Suffix(suffix) => match total {
            Some(total) => total.saturating_sub(suffix),
            None => return Err(ResponseFault::Range.into()),
        },
        RequestedRange::Whole => {
            // Public shapes need not have passed through an encoder.
            return Err(ResponseFault::Range.into());
        }
    };
    if start != requested_start {
        return Err(ResponseFault::Range.into());
    }
    if let Some(total) = total {
        let satisfiable = match shape.range {
            RequestedRange::Bounded { end, .. } => end.min(total),
            _ => total,
        };
        // The parser's end < total bound also makes the exclusive end fit.
        if end + 1 != satisfiable {
            return Err(ResponseFault::Range.into());
        }
    }
    Ok(GetHeadOutcome::Body {
        meta: meta(total),
        body: BodyWindow {
            object_offset: start,
            expected_len: Some(served),
            object_size: total,
        },
    })
}

pub(crate) enum ContentRange {
    Satisfied {
        start: u64,
        end: u64,
        total: Option<u64>,
    },
    Unsatisfied {
        total: Option<u64>,
    },
}

// Satisfied ranges establish S <= E < u64::MAX, and E < T when T is known.
// This keeps inclusive lengths and exclusive ends representable even for /*.
pub(crate) fn parse_content_range(value: &[u8]) -> Option<ContentRange> {
    let rest = trim_ascii(value).strip_prefix(b"bytes ")?;
    // `start-end/total` in digits, as both services write it, in one pass.
    // Anything else takes the general reading below, which agrees with this
    // one wherever this one reads a value.
    if let Some(range) = digits_range(rest) {
        return Some(range);
    }
    let slash = rest.iter().rposition(|byte| *byte == b'/')?;
    let (spec, total) = (trim_ascii(&rest[..slash]), trim_ascii(&rest[slash + 1..]));
    let total = match total {
        b"*" => None,
        digits => Some(decimal(digits)?),
    };
    if spec == b"*" {
        return Some(ContentRange::Unsatisfied { total });
    }
    let dash = spec.iter().position(|byte| *byte == b'-')?;
    let start = decimal(&spec[..dash])?;
    let end = decimal(&spec[dash + 1..])?;
    if start > end || end >= total.unwrap_or(u64::MAX) {
        return None;
    }
    Some(ContentRange::Satisfied { start, end, total })
}

// `start-end/total`, digits only, as a satisfied range.
fn digits_range(text: &[u8]) -> Option<ContentRange> {
    let (start, text) = leading_decimal(text)?;
    let (end, text) = leading_decimal(text.strip_prefix(b"-")?)?;
    let (total, text) = leading_decimal(text.strip_prefix(b"/")?)?;
    (text.is_empty() && start <= end && end < total).then_some(ContentRange::Satisfied {
        start,
        end,
        total: Some(total),
    })
}

// The number that `text` starts with, of 1 to 19 digits, which cannot
// overflow, and the rest of `text`. One loop reads the digits and their
// value; a 20th digit is refused, so the wrapping arithmetic never wraps in a
// value that is returned.
fn leading_decimal(text: &[u8]) -> Option<(u64, &[u8])> {
    let (mut value, mut len) = (0u64, 0);
    while let Some(&byte) = text.get(len) {
        let digit = byte.wrapping_sub(b'0');
        if digit > 9 || len == 19 {
            break;
        }
        value = value.wrapping_mul(10).wrapping_add(u64::from(digit));
        len += 1;
    }
    let refused = len == 0 || text.get(len).is_some_and(u8::is_ascii_digit);
    (!refused).then_some((value, &text[len..]))
}

// Reads a header value that carries text. Both services write
// `Last-Modified` in ASCII, so a value that is not UTF-8 is a fault in the
// head.
pub(crate) fn text_header(value: Option<&[u8]>) -> Result<Option<&str>> {
    value
        .map(|value| core::str::from_utf8(value).map_err(|_| Error::Response(ResponseFault::Head)))
        .transpose()
}

pub(crate) fn decimal_header(value: Option<&[u8]>) -> Result<Option<u64>> {
    match value {
        None => Ok(None),
        Some(value) => decimal(trim_ascii(value))
            .map(Some)
            .ok_or(Error::Response(ResponseFault::Head)),
    }
}

pub(crate) fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    // Up to 19 digits cannot overflow, so they need no checked arithmetic.
    if bytes.len() <= 19 {
        return bytes.iter().try_fold(0u64, |value, byte| {
            let digit = byte.checked_sub(b'0').filter(|digit| *digit <= 9)?;
            Some(value * 10 + u64::from(digit))
        });
    }
    bytes.iter().try_fold(0u64, |value, byte| {
        let digit = byte.checked_sub(b'0').filter(|digit| *digit <= 9)?;
        value.checked_mul(10)?.checked_add(digit as u64)
    })
}

pub(crate) fn trim_ascii(value: &[u8]) -> &[u8] {
    value.trim_ascii()
}

pub(crate) fn failure_class(status: u16, kind: Option<ServiceErrorKind>) -> FailureClass {
    match kind {
        Some(ServiceErrorKind::Unauthorized) => FailureClass::Auth,
        Some(ServiceErrorKind::Throttled) => FailureClass::Throttled,
        Some(ServiceErrorKind::Service | ServiceErrorKind::Timeout) => FailureClass::Server,
        _ => match status {
            300..=399 => FailureClass::Redirect,
            401 | 403 => FailureClass::Auth,
            408 | 429 => FailureClass::Throttled,
            500..=599 => FailureClass::Server,
            _ => FailureClass::Other,
        },
    }
}

// A range is written as `Range` writes it: `bytes=` and its first and last
// byte.
impl HeaderValue for RequestedRange {
    fn write_to(self, out: &mut dyn ByteSink) {
        write_range(out, self);
    }
}

pub(crate) fn write_range(out: &mut dyn ByteSink, range: RequestedRange) {
    out.push(b"bytes=");
    match range {
        RequestedRange::Bounded { start, end } => {
            out.push(U64Decimal::new(start).as_bytes());
            out.push(b"-");
            // Validation requires start < end, so end is nonzero.
            out.push(U64Decimal::new(end - 1).as_bytes());
        }
        RequestedRange::Offset(first) => {
            out.push(U64Decimal::new(first).as_bytes());
            out.push(b"-");
        }
        RequestedRange::Suffix(last) => {
            out.push(b"-");
            out.push(U64Decimal::new(last).as_bytes());
        }
        RequestedRange::Whole => unreachable!("the item was validated"),
    }
}

// The written head, or the exact number of bytes that it needed.
pub(crate) fn capacity_error(capacity: crate::CapacityError) -> Error {
    // A slice holds at most isize::MAX bytes.
    if capacity.required > isize::MAX as usize {
        InvalidRequest::RequestTooLarge.into()
    } else {
        Error::Capacity(capacity)
    }
}

#[cfg(test)]
#[test]
fn a_request_larger_than_a_slice_is_not_a_recoverable_capacity_error() {
    let fits = crate::CapacityError {
        required: isize::MAX as usize,
    };
    assert_eq!(capacity_error(fits), Error::Capacity(fits));
    let capacity = crate::CapacityError {
        required: isize::MAX as usize + 1,
    };
    assert_eq!(
        capacity_error(capacity),
        InvalidRequest::RequestTooLarge.into()
    );
}

pub(crate) fn condition_header(kind: ConditionKind) -> Option<&'static str> {
    match kind {
        ConditionKind::None => None,
        ConditionKind::IfMatch => Some("if-match"),
        ConditionKind::IfNoneMatch => Some("if-none-match"),
        ConditionKind::IfModifiedSince => Some("if-modified-since"),
        ConditionKind::IfUnmodifiedSince => Some("if-unmodified-since"),
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
            if valid_header(tag) =>
        {
            Ok(())
        }
        (
            ConditionKind::IfModifiedSince | ConditionKind::IfUnmodifiedSince,
            Some(ConditionValue::Time(seconds)),
        ) if seconds <= crate::time::MAX_UNIX_SECONDS => Ok(()),
        _ => Err(InvalidRequest::Condition.into()),
    }
}

// An entity tag is written as it is, and an instant as an HTTP date.
impl HeaderValue for ConditionValue<'_> {
    fn write_to(self, out: &mut dyn ByteSink) {
        match self {
            Self::ETag(tag) => out.push(tag),
            Self::Time(seconds) => out.push(Timestamps::from_unix(seconds).rfc1123_bytes()),
        }
    }
}

pub(crate) fn valid_header(value: &[u8]) -> bool {
    !value.is_empty() && value.is_ascii() && !value.iter().any(u8::is_ascii_control)
}

// Writes the body that the encoder generates, such as the list of parts of
// a commit, after the head, and finishes the request with it. `write` must
// write the bytes that the head states the length of.
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
            valid_header(bytes)
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

// Tags are written as `write_tags` writes them.
impl HeaderValue for &[Tag<'_>] {
    fn write_to(self, out: &mut dyn ByteSink) {
        write_tags(out, self);
    }
}

// Writes tags as both services take them in a header: `key=value` pairs
// joined by `&`, each part percent-encoded.
pub(crate) fn write_tags(out: &mut dyn ByteSink, tags: &[Tag<'_>]) {
    for (index, tag) in tags.iter().enumerate() {
        if index != 0 {
            out.push(b"&");
        }
        for part in crate::url::encode_query_value(tag.key.as_bytes()) {
            out.push(part);
        }
        out.push(b"=");
        for part in crate::url::encode_query_value(tag.value.as_bytes()) {
            out.push(part);
        }
    }
}

// Writes tags as both services take them in a body: a `TagSet` that holds a
// `Tag` of a `Key` and a `Value` for each.
pub(crate) fn write_tag_set(out: &mut dyn ByteSink, tags: &[Tag<'_>]) {
    out.push(b"<TagSet>");
    for tag in tags {
        out.push(b"<Tag><Key>");
        crate::encoding::write_xml_text(out, tag.key.as_bytes());
        out.push(b"</Key><Value>");
        crate::encoding::write_xml_text(out, tag.value.as_bytes());
        out.push(b"</Value></Tag>");
    }
    out.push(b"</TagSet>");
}

// Checks what every read of a template does alike: a HEAD reads no range,
// and a service that keeps no snapshots names none.
// Checks that a template names a snapshot only for a service that keeps
// them: S3 keeps none.
pub(crate) fn validate_revision_kind(
    kind: crate::RevisionKind,
    keeps_snapshots: bool,
) -> Result<()> {
    if kind == crate::RevisionKind::Snapshot && !keeps_snapshots {
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

pub(crate) fn validate_get_options(
    options: &crate::GetOptions,
    keeps_snapshots: bool,
) -> Result<()> {
    if options.kind == GetKind::Head && options.ranging == crate::Ranging::Ranged {
        return Err(InvalidRequest::RangedHead.into());
    }
    validate_revision_kind(options.revision, keeps_snapshots)
}

// Checks one read against its template's options: a range exactly when the
// template sends one, and not empty; a condition value and a revision
// exactly when the template carries them.
pub(crate) fn validate_get_item(
    options: &crate::GetOptions,
    item: &crate::GetItem<'_>,
) -> Result<()> {
    let ranged = item.range != RequestedRange::Whole;
    if ranged != (options.ranging == crate::Ranging::Ranged) {
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
pub(crate) fn validate_revision_item(
    kind: crate::RevisionKind,
    revision: Option<&str>,
) -> Result<()> {
    match (kind, revision) {
        (crate::RevisionKind::Current, None) => Ok(()),
        (crate::RevisionKind::Current, Some(_)) | (_, None) => Err(InvalidRequest::Revision.into()),
        (_, Some("")) => Err(InvalidRequest::Revision.into()),
        (_, Some(_)) => Ok(()),
    }
}

// Checks one write's checksum against its template: present exactly when
// the template sends one, of the template's kind, and base64 text of a
// digest of that kind.
pub(crate) fn validate_checksum_item(
    kind: Option<ChecksumKind>,
    value: Option<crate::ChecksumValue<'_>>,
) -> Result<()> {
    match (kind, value) {
        (None, None) => Ok(()),
        (Some(kind), Some(crate::ChecksumValue::Base64(text))) => kind.check_base64(text),
        (Some(kind), Some(crate::ChecksumValue::Digest(digest))) if digest.kind() == kind => Ok(()),
        _ => Err(InvalidRequest::Checksum.into()),
    }
}

// The base64 text of a write's checksum, written into `into` from a digest.
// S3 reads a CRC64 in big-endian order, Azure in little-endian order.
pub(crate) fn checksum_text<'a>(
    value: Option<crate::ChecksumValue<'a>>,
    into: &'a mut [u8; crate::checksum::BASE64_LEN],
    crc64_big_endian: bool,
) -> Option<&'a str> {
    match value? {
        crate::ChecksumValue::Base64(text) => Some(text),
        crate::ChecksumValue::Digest(digest) => Some(digest.base64_for(into, crc64_big_endian)),
    }
}

// Checks the bytes of a source that a staged copy names: a bounded range,
// not empty, exactly when its template names one.
pub(crate) fn validate_source_range(ranging: crate::Ranging, range: RequestedRange) -> Result<()> {
    match (ranging, range) {
        (crate::Ranging::Whole, RequestedRange::Whole) => Ok(()),
        (crate::Ranging::Ranged, RequestedRange::Bounded { start, end }) if start < end => Ok(()),
        (crate::Ranging::Ranged, RequestedRange::Suffix(_) | RequestedRange::Offset(_)) => {
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
