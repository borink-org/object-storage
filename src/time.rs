/// A host-supplied instant preformatted for Azure request headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamps {
    rfc1123: [u8; 29],
    unix: u64,
}

const WEEKDAYS: [&[u8; 3]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];
const MAX_UNIX_SECONDS: u64 = 253_402_300_799;

impl Timestamps {
    /// Formats non-negative Unix seconds as an RFC 1123 timestamp.
    ///
    /// Values after `9999-12-31 23:59:59 UTC` saturate because the HTTP date
    /// format has a four-digit year.
    pub fn from_unix(seconds: u64) -> Self {
        let seconds = seconds.min(MAX_UNIX_SECONDS);
        let days = (seconds / 86_400) as i64;
        let remainder = seconds % 86_400;
        let (hour, minute, second) = (remainder / 3600, (remainder % 3600) / 60, remainder % 60);

        // Howard Hinnant's `civil_from_days`, translated to Rust:
        // https://howardhinnant.github.io/date_algorithms.html#civil_from_days
        // It maps days since 1970-01-01 to a proleptic Gregorian date. March
        // is treated as month zero so leap day is at the end of each year.
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
        write_digits(&mut rfc1123[5..7], day as u64);
        rfc1123[8..11].copy_from_slice(MONTHS[(month - 1) as usize]);
        write_digits(&mut rfc1123[12..16], year as u64);
        write_digits(&mut rfc1123[17..19], hour);
        write_digits(&mut rfc1123[20..22], minute);
        write_digits(&mut rfc1123[23..25], second);
        Self {
            rfc1123,
            unix: seconds,
        }
    }

    /// Returns the represented Unix timestamp in seconds.
    pub fn unix(&self) -> u64 {
        self.unix
    }

    /// Returns `Www, DD Mon YYYY HH:MM:SS GMT` for `x-ms-date`.
    pub fn rfc1123(&self) -> &str {
        core::str::from_utf8(&self.rfc1123).expect("HTTP dates are ASCII")
    }
}

fn write_digits(output: &mut [u8], mut value: u64) {
    for byte in output.iter_mut().rev() {
        *byte = b'0' + (value % 10) as u8;
        value /= 10;
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_UNIX_SECONDS, MONTHS, Timestamps, WEEKDAYS};

    #[test]
    fn civil_conversion_matches_a_simple_calendar_for_every_day() {
        // Deliberately use a slow day-by-day calendar, not the formula under
        // test, as the reference implementation.
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

            if (year, month, day) == (9999, 12, 31) {
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
}
