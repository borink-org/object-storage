//! Helpers built on the public API.
//!
//! Each function here uses only the public types, so you can write your own
//! version if you need different behaviour.

use crate::{
    Blobs, Error, Payload, PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut, Result,
    Timestamps,
};

const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

/// Returns the number of bytes that [`Blobs::encode_get`] needs for this plan.
///
/// Call this to size a buffer before you encode; the answer is exact.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if `get` cannot become an Azure request,
/// unchanged from [`Blobs::encode_get`].
pub fn get_requirements(
    blobs: &Blobs<'_>,
    get: &PhysicalGet<'_>,
    now: &Timestamps,
) -> Result<usize> {
    required(blobs.encode_get(&mut [], get, now).map(drop))
}

/// Returns the number of bytes that [`Blobs::encode_put`] needs for this plan.
///
/// Call this to size a buffer before you encode. The answer covers the request
/// head only, and never the content. Only the length of `content` reaches the
/// head, so a [`Payload::Streamed`] sizes a buffer without the bytes.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if `put` cannot become an Azure request,
/// unchanged from [`Blobs::encode_put`].
pub fn put_requirements(
    blobs: &Blobs<'_>,
    put: &PhysicalPut<'_>,
    content: Payload<'_>,
    now: &Timestamps,
) -> Result<usize> {
    // The head states how long the content is, so the requirement depends on
    // the length of `content`. Its bytes are never read.
    required(blobs.encode_put(&mut [], put, content, now).map(drop))
}

/// Returns the number of bytes that [`Blobs::encode_delete`] needs for this
/// plan.
///
/// Call this to size a buffer before you encode; the answer is exact.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if `delete` cannot become an Azure request,
/// unchanged from [`Blobs::encode_delete`].
pub fn delete_requirements(
    blobs: &Blobs<'_>,
    delete: &PhysicalDelete<'_>,
    now: &Timestamps,
) -> Result<usize> {
    required(blobs.encode_delete(&mut [], delete, now).map(drop))
}

/// Returns the number of bytes that [`Blobs::encode_list`] needs for this
/// plan.
///
/// Call this to size a buffer before you encode; the answer is exact.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if `list` cannot become an Azure request,
/// unchanged from [`Blobs::encode_list`].
pub fn list_requirements(
    blobs: &Blobs<'_>,
    list: &PhysicalList<'_>,
    now: &Timestamps,
) -> Result<usize> {
    required(blobs.encode_list(&mut [], list, now).map(drop))
}

/// Writes an entity tag from a listing in the quoted form that HTTP defines.
///
/// A listing writes an entity tag without the quotes that the `ETag` header
/// carries. Azure conditions a request on either form; this writes the quoted
/// one.
///
/// Copies `listed` into `into`, adding the quotes unless it already carries
/// them or is a weak tag, and returns what it wrote. Returns [`None`] if
/// `into` is too small; it needs at most two bytes more than `listed`.
///
/// Use it on [`ListEntry::e_tag`](crate::ListEntry::e_tag) to turn an entry of
/// a listing into a
/// [`PhysicalGet::condition_value`](crate::PhysicalGet::condition_value).
pub fn quoted_etag<'a>(listed: &[u8], into: &'a mut [u8]) -> Option<&'a [u8]> {
    let quoted = listed.starts_with(b"\"") && listed.ends_with(b"\"") && listed.len() >= 2;
    if quoted || listed.starts_with(b"W/") {
        let into = into.get_mut(..listed.len())?;
        into.copy_from_slice(listed);
        return Some(into);
    }
    let into = into.get_mut(..listed.len() + 2)?;
    into[0] = b'"';
    into[1..listed.len() + 1].copy_from_slice(listed);
    into[listed.len() + 1] = b'"';
    Some(into)
}

fn required(result: Result<()>) -> Result<usize> {
    match result {
        Ok(()) => Ok(0),
        Err(Error::Capacity(error)) => Ok(error.required),
        Err(error) => Err(error),
    }
}

/// Writes the text of a listed value with its references resolved.
///
/// Use this on a value that
/// [`ListEntry::property`](crate::ListEntry::property) returned, which holds
/// the bytes that the service wrote. XML writes an `&` as `&amp;`, and a
/// character that the document cannot carry as `&#233;`. This writes what
/// those stand for.
///
/// Copies `value` into `into` and returns what it wrote, which is never longer
/// than `value`. Returns [`None`] if `into` is shorter than `value`, and for a
/// reference that no listing declares.
///
/// This undoes XML references and nothing else. It does not percent-decode.
/// Azure escapes the metadata it returns for XML but never percent-encodes
/// it, so `already%80escaped` is the text itself and not an escape. Only a
/// name that the service marked as encoded is percent-decoded, and reading
/// the page did that already.
///
/// The fields of a [`ListEntry`](crate::ListEntry) do not need this. Reading
/// the page decoded them in place.
pub fn decode_into<'a>(value: &[u8], into: &'a mut [u8]) -> Option<&'a [u8]> {
    let into = into.get_mut(..value.len())?;
    into.copy_from_slice(value);
    let len = crate::xml::decode_text(into, false).ok()?;
    Some(&into[..len])
}

/// Reads an HTTP date as milliseconds since the Unix epoch.
///
/// Use this on [`ObjectMeta::last_modified`], which holds the bytes that Azure
/// sent. Returns [`None`] if `value` is not an RFC 1123 date.
///
/// [`ObjectMeta::last_modified`]: crate::ObjectMeta::last_modified
pub fn http_date_ms(value: &[u8]) -> Option<u64> {
    // RFC 1123 `Www, DD Mon YYYY HH:MM:SS GMT`, the only form these services
    // send and the only one this crate writes.
    if value.len() != 29
        || value[3] != b','
        || value[4] != b' '
        || value[7] != b' '
        || value[11] != b' '
        || value[16] != b' '
        || value[19] != b':'
        || value[22] != b':'
        || &value[25..] != b" GMT"
    {
        return None;
    }
    let day = number(&value[5..7])?;
    let month = MONTHS
        .iter()
        .position(|name| name.as_slice() == &value[8..11])? as u64
        + 1;
    let year = number(&value[12..16])? as i64;
    let hour = number(&value[17..19])?;
    let minute = number(&value[20..22])?;
    let second = number(&value[23..25])?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let seconds = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add((hour * 3600 + minute * 60 + second) as i64)?;
    u64::try_from(seconds).ok()?.checked_mul(1000)
}

fn number(bytes: &[u8]) -> Option<u64> {
    bytes.iter().try_fold(0, |value, byte| {
        byte.checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .map(|digit| value * 10 + digit as u64)
    })
}

// Howard Hinnant's `days_from_civil`, the inverse of the conversion in `time`.
fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::{http_date_ms, quoted_etag};

    #[test]
    fn reads_an_azure_last_modified_header() {
        assert_eq!(
            http_date_ms(b"Fri, 24 May 2013 00:00:00 GMT"),
            Some(1_369_353_600_000)
        );
        assert_eq!(http_date_ms(b"not an HTTP date"), None);
        assert_eq!(http_date_ms(b"Fri, 24 Xxx 2013 00:00:00 GMT"), None);
    }

    #[test]
    fn quotes_a_listed_etag_once() {
        let mut into = [0; 32];
        assert_eq!(
            quoted_etag(b"0x8DF0046E8E555AF", &mut into),
            Some(b"\"0x8DF0046E8E555AF\"".as_slice())
        );
        assert_eq!(
            quoted_etag(b"\"already\"", &mut into),
            Some(b"\"already\"".as_slice())
        );
        assert_eq!(
            quoted_etag(b"W/\"weak\"", &mut into),
            Some(b"W/\"weak\"".as_slice())
        );
        assert_eq!(quoted_etag(b"0x8DF", &mut [0; 6]), None);
    }
}
