// Reading the values of a listing and the dates of a head.

const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

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
/// [`GetItem::condition_value`](crate::GetItem::condition_value).
pub fn quoted_etag<'a>(listed: &[u8], into: &'a mut [u8]) -> Option<&'a [u8]> {
    let quoted = listed.starts_with(b"\"") && listed.ends_with(b"\"") && listed.len() >= 2;
    if quoted || listed.starts_with(b"W/") {
        let into = into.get_mut(..listed.len())?;
        into.copy_from_slice(listed);
        return Some(into);
    }
    // A byte slice is at most isize::MAX bytes, leaving usize room for two quotes.
    let into = into.get_mut(..listed.len() + 2)?;
    into[0] = b'"';
    into[1..listed.len() + 1].copy_from_slice(listed);
    into[listed.len() + 1] = b'"';
    Some(into)
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
pub fn decode_property<'a>(value: &[u8], into: &'a mut [u8]) -> Option<&'a [u8]> {
    let into = into.get_mut(..value.len())?;
    into.copy_from_slice(value);
    let len = crate::xml::decode_text(into, false).ok()?;
    Some(&into[..len])
}

/// Reads an HTTP date as milliseconds since the Unix epoch.
///
/// Use this on [`ObjectMeta::last_modified`], and on
/// [`ListEntry::last_modified`] of an Azure listing. Returns [`None`] if
/// `value` is not an RFC 1123 date.
///
/// [`ObjectMeta::last_modified`]: crate::ObjectMeta::last_modified
/// [`ListEntry::last_modified`]: crate::ListEntry::last_modified
pub fn http_date_ms(value: &str) -> Option<u64> {
    // RFC 1123 `Www, DD Mon YYYY HH:MM:SS GMT`, the only form these services
    // send and the only one this crate writes.
    let value = value.as_bytes();
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

/// Reads an ISO 8601 date as milliseconds since the Unix epoch.
///
/// Use this on [`ListEntry::last_modified`] of an S3 listing, which is
/// written as `2026-08-22T12:01:01.000Z`. The fraction of a second is
/// optional and read to the millisecond. Returns [`None`] if `value` is not
/// such a date in UTC.
///
/// [`ListEntry::last_modified`]: crate::ListEntry::last_modified
pub fn iso8601_ms(value: &str) -> Option<u64> {
    let value = value.as_bytes();
    if value.len() < 20
        || value[4] != b'-'
        || value[7] != b'-'
        || value[10] != b'T'
        || value[13] != b':'
        || value[16] != b':'
        || value.last() != Some(&b'Z')
    {
        return None;
    }
    let year = number(&value[..4])? as i64;
    let month = number(&value[5..7])?;
    let day = number(&value[8..10])?;
    let hour = number(&value[11..13])?;
    let minute = number(&value[14..16])?;
    let second = number(&value[17..19])?;
    // The fraction, if any, between the seconds and the `Z`.
    let millis = match &value[19..value.len() - 1] {
        [] => 0,
        [b'.', digits @ ..] if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) => {
            // Three digits are milliseconds; fewer are padded and more cut.
            digits
                .iter()
                .chain(core::iter::repeat(&b'0'))
                .take(3)
                .fold(0, |value, digit| value * 10 + (digit - b'0') as u64)
        }
        _ => return None,
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let seconds = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add((hour * 3600 + minute * 60 + second) as i64)?;
    u64::try_from(seconds)
        .ok()?
        .checked_mul(1000)?
        .checked_add(millis)
}

fn number(bytes: &[u8]) -> Option<u64> {
    // The date readers pass only two- or four-byte fields, so the sum is <= 9999.
    bytes.iter().try_fold(0, |value, byte| {
        byte.checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .map(|digit| value * 10 + digit as u64)
    })
}

// Howard Hinnant's `days_from_civil`, the inverse of the conversion in `time`.
// Called only with a four-digit year, month 1..=12 and day 1..=31; all
// intermediates fit i64, including the adjusted year -1 for January of year 0.
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
    use super::{http_date_ms, iso8601_ms, quoted_etag};

    #[test]
    fn reads_an_azure_last_modified_header() {
        assert_eq!(
            http_date_ms("Fri, 24 May 2013 00:00:00 GMT"),
            Some(1_369_353_600_000)
        );
        assert_eq!(http_date_ms("not an HTTP date"), None);
        assert_eq!(http_date_ms("Fri, 24 Xxx 2013 00:00:00 GMT"), None);
    }

    #[test]
    fn reads_an_s3_listing_date() {
        assert_eq!(
            iso8601_ms("2013-05-24T00:00:00.000Z"),
            Some(1_369_353_600_000)
        );
        assert_eq!(
            iso8601_ms("2013-05-24T00:00:01.5Z"),
            Some(1_369_353_601_500)
        );
        assert_eq!(iso8601_ms("2013-05-24T00:00:01Z"), Some(1_369_353_601_000));
        for value in [
            "2013-05-24T00:00:00.000",
            "2013-05-24 00:00:00Z",
            "2013-05-24T00:00:00.Z",
            "2013-13-24T00:00:00Z",
            "Fri, 24 May 2013 00:00:00 GMT",
        ] {
            assert_eq!(iso8601_ms(value), None, "{value}");
        }
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
