//! Helpers built on the public API.
//!
//! Each function here uses only the public types, so you can write your own
//! version if you need different behaviour.

use crate::{Blobs, Error, PhysicalGet, Result, Timestamps};

const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

/// Returns the number of bytes that [`Blobs::encode_get`] needs for this plan.
///
/// Call this to size a buffer before you encode. This function encodes into an
/// empty buffer and reads the capacity error, so the answer is exact.
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
    match blobs.encode_get(&mut [], get, now) {
        Ok(_) => Ok(0),
        Err(Error::Capacity(error)) => Ok(error.required),
        Err(error) => Err(error),
    }
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
    use super::http_date_ms;

    #[test]
    fn reads_an_azure_last_modified_header() {
        assert_eq!(
            http_date_ms(b"Fri, 24 May 2013 00:00:00 GMT"),
            Some(1_369_353_600_000)
        );
        assert_eq!(http_date_ms(b"not an HTTP date"), None);
        assert_eq!(http_date_ms(b"Fri, 24 Xxx 2013 00:00:00 GMT"), None);
    }
}
