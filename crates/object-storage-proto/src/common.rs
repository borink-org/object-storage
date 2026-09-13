// What both providers share when they write a request head and read a
// response head.

use crate::request::{HeadWriter, U64Decimal, Writer};
use crate::{
    BodyWindow, ConditionKind, Error, Failure, FailureClass, GetHeadOutcome, GetKind, GetShape,
    HeaderSpan, InvalidPlan, Method, ObjectMeta, Payload, RequestedRange, ResponseFault,
    ResponseHead, Result, ServiceErrorKind, WireRequest,
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

pub(crate) fn accept_success<'h>(
    shape: GetShape,
    head: ResponseHead<'h>,
) -> Result<GetHeadOutcome<'h>> {
    let content_length = decimal_header(head.content_length)?;
    let last_modified = text_header(head.last_modified)?;
    let meta = |size| ObjectMeta {
        size,
        e_tag: head.e_tag,
        last_modified,
        version: head.version,
        content_encoding: head.content_encoding,
        content_type: head.content_type,
    };
    if head.status == 200 {
        // An unranged plan reads from byte zero, and Azure states the whole
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
    // Azure serves the whole satisfiable range, so a short serve is a
    // mismatch: silently accepting it would hand consumers a partial read.
    let requested_start = match shape.range {
        RequestedRange::Bounded { start, .. } | RequestedRange::Offset(start) => start,
        RequestedRange::Whole | RequestedRange::Suffix(_) => {
            // Public shapes need not have passed through encode_get.
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

// Reads a header value that carries text. Azure writes `Last-Modified` in
// ASCII, so a value that is not UTF-8 is a fault in the head.
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

pub(crate) fn write_range(out: &mut Writer<'_>, range: RequestedRange) {
    out.push(b"bytes=");
    match range {
        RequestedRange::Bounded { start, end } => {
            out.push(U64Decimal::new(start).as_bytes());
            out.push(b"-");
            // validate_get requires start < end, so end is nonzero.
            out.push(U64Decimal::new(end - 1).as_bytes());
        }
        RequestedRange::Offset(first) => {
            out.push(U64Decimal::new(first).as_bytes());
            out.push(b"-");
        }
        RequestedRange::Whole | RequestedRange::Suffix(_) => {
            unreachable!("the plan was validated")
        }
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
    }
}

// The kind and the value must agree in both directions: a kind without a value
// cannot be encoded, and a value without a kind would be dropped.
pub(crate) fn validate_condition(condition: ConditionKind, value: Option<&[u8]>) -> Result<()> {
    match (condition, value) {
        (ConditionKind::None, None) => Ok(()),
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
