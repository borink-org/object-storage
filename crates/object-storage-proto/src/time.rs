/// An instant that you supply, formatted for request headers.
///
/// This crate performs no I/O and cannot read the clock, so pass one of these
/// to [`azure::encode_get`](crate::azure::encode_get) and every other
/// encoding method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamps {
    rfc1123: [u8; 29],
    iso8601: [u8; 16],
    unix: u64,
}

const WEEKDAYS: [&[u8; 3]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];
pub(crate) const MAX_UNIX_SECONDS: u64 = 253_402_300_799;

impl Timestamps {
    /// Creates a timestamp from seconds since the Unix epoch.
    ///
    /// The HTTP date format has a four-digit year, so a value after
    /// `9999-12-31 23:59:59 UTC` becomes that instant.
    pub fn from_unix(seconds: u64) -> Self {
        let seconds = seconds.min(MAX_UNIX_SECONDS);
        let days = (seconds / 86_400) as i64;
        let remainder = seconds % 86_400;
        let (hour, minute, second) = (remainder / 3600, (remainder % 3600) / 60, remainder % 60);

        // Howard Hinnant's `civil_from_days`, translated to Rust:
        // https://howardhinnant.github.io/date_algorithms.html#civil_from_days
        // It maps days since 1970-01-01 to a proleptic Gregorian date. March
        // is treated as month zero so leap day is at the end of each year.
        // The clamp above bounds days to 2,932,896; these i64 intermediates
        // stay below four million and produce month 1..=12, day 1..=31.
        let shifted_days = days + 719_468;
        let era = shifted_days.div_euclid(146_097);
        let day_of_era = shifted_days.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        if month <= 2 {
            year += 1;
        }

        let mut rfc1123 = *b"Xxx, 00 Xxx 0000 00:00:00 GMT";
        // 1970-01-01 was Thursday, index 4 in `WEEKDAYS`.
        rfc1123[..3].copy_from_slice(WEEKDAYS[((days + 4) % 7) as usize]);
        write_fixed_digits(&mut rfc1123[5..7], day as u64);
        rfc1123[8..11].copy_from_slice(MONTHS[(month - 1) as usize]);
        write_fixed_digits(&mut rfc1123[12..16], year as u64);
        write_fixed_digits(&mut rfc1123[17..19], hour);
        write_fixed_digits(&mut rfc1123[20..22], minute);
        write_fixed_digits(&mut rfc1123[23..25], second);
        let mut iso8601 = *b"00000000T000000Z";
        write_fixed_digits(&mut iso8601[..4], year as u64);
        write_fixed_digits(&mut iso8601[4..6], month as u64);
        write_fixed_digits(&mut iso8601[6..8], day as u64);
        write_fixed_digits(&mut iso8601[9..11], hour);
        write_fixed_digits(&mut iso8601[11..13], minute);
        write_fixed_digits(&mut iso8601[13..15], second);
        Self {
            rfc1123,
            iso8601,
            unix: seconds,
        }
    }

    /// Returns the time in seconds since the Unix epoch.
    pub fn unix(&self) -> u64 {
        self.unix
    }

    /// Returns the time as `Www, DD Mon YYYY HH:MM:SS GMT`, for `x-ms-date`.
    pub fn rfc1123(&self) -> &str {
        core::str::from_utf8(&self.rfc1123).expect("HTTP dates are ASCII")
    }

    // The bytes of `Self::rfc1123`, for a writer that needs no `str`.
    pub(crate) fn rfc1123_bytes(&self) -> &[u8] {
        &self.rfc1123
    }

    /// Returns the time as `YYYYMMDDTHHMMSSZ`, for `x-amz-date`.
    pub fn iso8601(&self) -> &str {
        core::str::from_utf8(&self.iso8601).expect("ISO 8601 basic dates are ASCII")
    }

    // The bytes of `Self::iso8601`, for a writer that needs no `str`.
    pub(crate) fn iso8601_bytes(&self) -> &[u8] {
        &self.iso8601
    }

    // The `YYYYMMDD` that a SigV4 credential scope names.
    pub(crate) fn date(&self) -> &[u8] {
        &self.iso8601[..8]
    }

    /// The length of a timestamp stored with [`Self::to_bytes`].
    pub const STORED_LEN: usize = 53;

    /// Returns the timestamp as bytes, which [`Self::from_bytes`] reads
    /// back: the seconds, then both dates as text. They hold no pointer.
    pub fn to_bytes(&self) -> [u8; Self::STORED_LEN] {
        let mut stored = [0; Self::STORED_LEN];
        let unix = self.unix.to_le_bytes();
        let values = unix.iter().chain(&self.iso8601).chain(&self.rfc1123);
        for (slot, value) in stored.iter_mut().zip(values) {
            *slot = *value;
        }
        stored
    }

    /// Reads a timestamp that [`Self::to_bytes`] stored, or [`None`] for
    /// bytes whose dates are not ASCII text.
    ///
    /// It checks nothing else: bytes that [`Self::to_bytes`] did not write
    /// give a timestamp whose dates are those bytes, which a request then
    /// carries.
    pub fn from_bytes(stored: &[u8; Self::STORED_LEN]) -> Option<Self> {
        let (unix, dates) = stored.split_first_chunk::<8>()?;
        let (iso8601, rfc1123) = dates.split_first_chunk::<16>()?;
        let rfc1123: [u8; 29] = rfc1123.try_into().ok()?;
        (iso8601.is_ascii() && rfc1123.is_ascii()).then_some(Self {
            rfc1123,
            iso8601: *iso8601,
            unix: u64::from_le_bytes(*unix),
        })
    }
}

// Date fields have a width fixed by RFC 1123, so leading zeroes are retained.
fn write_fixed_digits(output: &mut [u8], mut value: u64) {
    for byte in output.iter_mut().rev() {
        *byte = b'0' + (value % 10) as u8;
        value /= 10;
    }
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
    let day = parse_digits(&value[5..7])?;
    let month = MONTHS
        .iter()
        .position(|name| name.as_slice() == &value[8..11])? as u64
        + 1;
    let year = parse_digits(&value[12..16])? as i64;
    let hour = parse_digits(&value[17..19])?;
    let minute = parse_digits(&value[20..22])?;
    let second = parse_digits(&value[23..25])?;
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
    let year = parse_digits(&value[..4])? as i64;
    let month = parse_digits(&value[5..7])?;
    let day = parse_digits(&value[8..10])?;
    let hour = parse_digits(&value[11..13])?;
    let minute = parse_digits(&value[14..16])?;
    let second = parse_digits(&value[17..19])?;
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

fn parse_digits(bytes: &[u8]) -> Option<u64> {
    // The date readers pass only two- or four-byte fields, so the sum is <= 9999.
    bytes.iter().try_fold(0, |value, byte| {
        byte.checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .map(|digit| value * 10 + digit as u64)
    })
}

// Howard Hinnant's `days_from_civil`, the inverse of `civil_from_days` in `Timestamps::from_unix`.
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
    extern crate std;

    use super::{MAX_UNIX_SECONDS, MONTHS, Timestamps, WEEKDAYS, http_date_ms, iso8601_ms};

    #[test]
    fn a_stored_timestamp_reads_back_as_itself_and_needs_text() {
        let now = Timestamps::from_unix(1_787_400_000);
        let stored = now.to_bytes();
        assert_eq!(Timestamps::from_bytes(&stored), Some(now));
        for at in [8, 23, 24, 52] {
            let mut changed = stored;
            changed[at] = 0xC3;
            assert_eq!(Timestamps::from_bytes(&changed), None, "{at}");
        }
        // The seconds are a number, whatever their bytes.
        let mut later = stored;
        later[0] ^= 1;
        assert_eq!(
            Timestamps::from_bytes(&later).map(|time| time.unix()),
            Some(1_787_400_001)
        );
    }

    #[test]
    fn civil_conversion_matches_a_simple_calendar_for_every_day() {
        // Deliberately use a slow day-by-day calendar, not the formula under
        // test, as the reference implementation.
        //
        // The calendar repeats every 400 years, so every day up to the end of
        // 2400 covers each leap rule once and crosses the era boundary at
        // 2000 that the formula computes around. Sweeping to 9999 instead
        // took most of the crate's test time and found nothing more.
        let (mut year, mut month, mut day) = (1970u32, 1usize, 1u32);
        let (mut weekday, mut unix) = (4usize, 0u64);

        loop {
            let actual = Timestamps::from_unix(unix);
            let text = actual.rfc1123().as_bytes();
            assert_eq!(&text[..3], WEEKDAYS[weekday], "{year}-{month}-{day}");
            assert_eq!(decimal(&text[5..7]), day, "{year}-{month}-{day}");
            assert_eq!(&text[8..11], MONTHS[month - 1], "{year}-{month}-{day}");
            assert_eq!(decimal(&text[12..16]), year, "{year}-{month}-{day}");
            assert_eq!(&text[17..], b"00:00:00 GMT", "{year}-{month}-{day}");
            let iso = actual.iso8601();
            assert_eq!(iso, std::format!("{year:04}{month:02}{day:02}T000000Z"));
            assert_eq!(actual.date(), &iso.as_bytes()[..8]);

            if (year, month, day) == (2400, 12, 31) {
                break;
            }
            unix += 86_400;
            weekday = (weekday + 1) % 7;
            day += 1;
            if day > days_in_month(year, month) {
                day = 1;
                month += 1;
                if month == 13 {
                    month = 1;
                    year += 1;
                }
            }
        }
    }

    #[test]
    fn formats_the_basic_form_that_sigv4_signs() {
        // The instant of the examples in the AWS Signature Version 4
        // documentation for S3.
        assert_eq!(
            Timestamps::from_unix(1_369_353_600).iso8601(),
            "20130524T000000Z"
        );
        assert_eq!(
            Timestamps::from_unix(1_787_400_061).iso8601(),
            "20260822T120101Z"
        );
    }

    #[test]
    fn formats_seconds_within_a_day() {
        assert_eq!(
            Timestamps::from_unix(1).rfc1123(),
            "Thu, 01 Jan 1970 00:00:01 GMT"
        );
        assert_eq!(
            Timestamps::from_unix(86_399).rfc1123(),
            "Thu, 01 Jan 1970 23:59:59 GMT"
        );
    }

    #[test]
    fn the_last_representable_day_formats_as_itself() {
        assert_eq!(
            Timestamps::from_unix(MAX_UNIX_SECONDS).rfc1123(),
            "Fri, 31 Dec 9999 23:59:59 GMT"
        );
    }

    #[test]
    fn saturates_at_the_four_digit_year_limit() {
        assert_eq!(
            Timestamps::from_unix(u64::MAX),
            Timestamps::from_unix(MAX_UNIX_SECONDS)
        );
    }

    fn decimal(bytes: &[u8]) -> u32 {
        bytes
            .iter()
            .fold(0, |value, byte| value * 10 + u32::from(byte - b'0'))
    }

    fn days_in_month(year: u32, month: usize) -> u32 {
        match month {
            4 | 6 | 9 | 11 => 30,
            2 if year.is_multiple_of(4)
                && (!year.is_multiple_of(100) || year.is_multiple_of(400)) =>
            {
                29
            }
            2 => 28,
            _ => 31,
        }
    }

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
}
