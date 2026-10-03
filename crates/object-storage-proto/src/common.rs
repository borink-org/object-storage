// What both providers share when they write a request head and read a
// response head.

use crate::checksum::{ChecksumKind, ChecksumProvider, KINDS, Sum};
use crate::request::{ByteSink, HeadWriter, U64Decimal, Writer};
use crate::{
    BodyWindow, ConditionKind, Error, Failure, FailureClass, GetHeadOutcome, GetKind, GetShape,
    HeaderSpan, InvalidPlan, Method, ObjectMeta, Payload, RequestedRange, ResponseFault,
    ResponseHead, Result, ServiceErrorKind, Tag, TransactionalChecksum, WireRequest, WriteOptions,
};

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
// outcome type.
pub(crate) trait FailureOutcome<'h> {
    fn not_found(kind: Option<ServiceErrorKind>) -> Self;
    fn service_failure(failure: Failure<'h>) -> Self;
}

// Finishes a failure whose head named no error, with the error `kind` that
// its body named. A 404 is not found, and any other status is a failure of
// the service.
pub(crate) fn finish_with_body<'h, O: FailureOutcome<'h>>(
    head_failure: Failure<'h>,
    kind: Option<ServiceErrorKind>,
) -> O {
    match head_failure.status {
        404 => O::not_found(kind),
        status => O::service_failure(failure(status, kind, head_failure.request_id)),
    }
}

macro_rules! failure_outcome {
    ($($outcome:ident),*) => {$(
        impl<'h> FailureOutcome<'h> for crate::$outcome<'h> {
            fn not_found(kind: Option<ServiceErrorKind>) -> Self {
                Self::NotFound { kind }
            }

            fn service_failure(failure: Failure<'h>) -> Self {
                Self::ServiceFailure(failure)
            }
        }
    )*};
}

failure_outcome!(
    GetHeadOutcome,
    PutHeadOutcome,
    DeleteHeadOutcome,
    ListHeadOutcome,
    StageHeadOutcome,
    CommitHeadOutcome,
    ListPartsHeadOutcome,
    UpdateHeadOutcome,
    TagsHeadOutcome,
    DeleteManyHeadOutcome
);

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
        // An unranged plan reads from byte zero, and the service states the whole
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

// The condition is the last header of every request that carries one.
pub(crate) fn push_condition(
    head: &mut HeadWriter<'_>,
    condition: ConditionKind,
    value: Option<&[u8]>,
) {
    if let Some(name) = condition_header(condition) {
        let value = value.expect("the plan was validated");
        head.header(name, |out| out.push(value));
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
        RequestedRange::Whole => unreachable!("the plan was validated"),
    }
}

// The written head, or the exact number of bytes that it needed.
pub(crate) fn capacity_error(capacity: crate::CapacityError) -> Error {
    // A slice's byte size must fit isize, including descriptor arrays on 32-bit.
    // HeaderSpan is nonzero-sized; divide before comparing to avoid overflow.
    let max_headers = isize::MAX as usize / core::mem::size_of::<HeaderSpan>();
    if capacity.required > isize::MAX as usize || capacity.required_headers > max_headers {
        InvalidPlan::RequestTooLarge.into()
    } else {
        Error::Capacity(capacity)
    }
}

#[cfg(test)]
#[test]
fn a_request_larger_than_a_slice_is_not_a_recoverable_capacity_error() {
    let capacity = crate::CapacityError {
        required: isize::MAX as usize + 1,
        ..crate::CapacityError::default()
    };
    assert_eq!(
        capacity_error(capacity),
        InvalidPlan::RequestTooLarge.into()
    );
    let max_headers = isize::MAX as usize / core::mem::size_of::<HeaderSpan>();
    let capacity = crate::CapacityError {
        required_headers: max_headers,
        ..crate::CapacityError::default()
    };
    assert_eq!(capacity_error(capacity), Error::Capacity(capacity));
    let capacity = crate::CapacityError {
        required_headers: max_headers + 1,
        ..capacity
    };
    assert_eq!(
        capacity_error(capacity),
        InvalidPlan::RequestTooLarge.into()
    );
}

pub(crate) fn encoded<'r>(
    head: HeadWriter<'r>,
    method: Method,
    payload: Payload<'r>,
) -> Result<WireRequest<'r>> {
    let capacity = head.capacity();
    head.finish(method, payload)
        .ok_or_else(|| capacity_error(capacity))
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
// cannot be encoded, and a value without a kind would be dropped. A date must
// be one that the services read, as `Timestamps::rfc1123` writes it.
pub(crate) fn validate_condition(condition: ConditionKind, value: Option<&[u8]>) -> Result<()> {
    match (condition, value) {
        (ConditionKind::None, None) => Ok(()),
        (kind, Some(value)) if kind.is_date() => {
            match core::str::from_utf8(value)
                .ok()
                .and_then(crate::layered::http_date_ms)
            {
                Some(_) => Ok(()),
                None => Err(InvalidPlan::Condition.into()),
            }
        }
        (ConditionKind::IfMatch | ConditionKind::IfNoneMatch, Some(value))
            if valid_header(value) =>
        {
            Ok(())
        }
        _ => Err(InvalidPlan::Condition.into()),
    }
}

pub(crate) fn valid_header(value: &[u8]) -> bool {
    !value.is_empty() && value.is_ascii() && !value.iter().any(u8::is_ascii_control)
}

// Checks the checksum of a write. Text must be the base64 of a digest of its
// kind. A computed checksum needs the content, which the encoder holds only
// if `has_bytes`, and a provider of its kind among `checksums`. A provider
// that refuses a kind refuses it before this.
pub(crate) fn validate_checksum(
    checksum: Option<TransactionalChecksum<'_>>,
    has_bytes: bool,
    checksums: &[Option<ChecksumProvider>; KINDS],
) -> Result<()> {
    match checksum {
        Some(TransactionalChecksum::Md5(text)) => ChecksumKind::Md5.check_base64(text),
        Some(TransactionalChecksum::Crc64(text)) => ChecksumKind::Crc64.check_base64(text),
        Some(TransactionalChecksum::Compute(kind))
            if !has_bytes || checksums[kind.slot()].is_none() =>
        {
            Err(InvalidPlan::Option.into())
        }
        Some(TransactionalChecksum::Compute(_)) | None => Ok(()),
        // The checksums that only S3 takes, which its client checks.
        Some(_) => Err(InvalidPlan::Option.into()),
    }
}

// Writes the checksum header of a write, if the plan carries a checksum.
// Text that the plan gave is written as it is; `validate_checksum` checked
// it. A computed checksum is summed here by the provider of its kind, which
// `validate_checksum` checked is registered. `content` feeds the content to
// the sum one piece at a time: a put or a stage has one piece, and a commit
// writes its list of parts piece by piece.
pub(crate) fn push_checksum(
    head: &mut HeadWriter<'_>,
    checksum: Option<TransactionalChecksum<'_>>,
    checksums: &[Option<ChecksumProvider>; KINDS],
    content: impl FnOnce(&mut Sum),
) {
    match checksum {
        Some(TransactionalChecksum::Md5(text)) => {
            head.header(ChecksumKind::Md5.header(), |out| out.push(text.as_bytes()));
        }
        Some(TransactionalChecksum::Crc64(text)) => {
            head.header(ChecksumKind::Crc64.header(), |out| {
                out.push(text.as_bytes())
            });
        }
        Some(TransactionalChecksum::Compute(kind)) => {
            // Validation refused the plan if this is `None`.
            if let Some(provider) = &checksums[kind.slot()] {
                let mut sum = provider.start();
                content(&mut sum);
                let mut into = [0; crate::checksum::BASE64_LEN];
                let text = sum.finish().base64(&mut into);
                head.header(kind.header(), |out| out.push(text.as_bytes()));
            }
        }
        // `validate_checksum` refused any other kind.
        _ => {}
    }
}

// Writes the body that the encoder generates, such as the list of parts of
// a commit, after the head, and finishes the request with it. `write` must
// write the bytes that the head states the length of.
pub(crate) fn encoded_with_body<'r>(
    mut head: HeadWriter<'r>,
    method: Method,
    write: impl FnOnce(&mut Writer<'r>),
) -> Result<WireRequest<'r>> {
    let body = head.body(write);
    let capacity = head.capacity();
    head.finish_with_body(method, Payload::Slice(&[]), Some(body))
        .ok_or_else(|| capacity_error(capacity))
}

// Checks the content properties and the storage class of a write: each is
// one header value that the service stores as given, so no space at either
// end, which HTTP drops.
pub(crate) fn validate_properties(options: &WriteOptions<'_>) -> Result<()> {
    let stored_as_given =
        |value: &str| valid_header(value.as_bytes()) && value.trim_ascii() == value;
    if options
        .properties
        .iter()
        .map(|(_, value)| value)
        .chain(options.storage_class)
        .all(stored_as_given)
    {
        Ok(())
    } else {
        Err(InvalidPlan::ContentProperty.into())
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
            return Err(InvalidPlan::Tag.into());
        }
    }
    if limits.is_some_and(|(count, _, _)| tags.len() > count) {
        return Err(InvalidPlan::Tag.into());
    }
    Ok(())
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
