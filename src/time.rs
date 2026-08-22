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
    pub fn from_unix(seconds: u64) -> Self {
        let seconds = seconds.min(MAX_UNIX_SECONDS);
        let days = (seconds / 86_400) as i64;
        let remainder = seconds % 86_400;
        let (hour, minute, second) = (remainder / 3600, (remainder % 3600) / 60, remainder % 60);

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

    pub fn unix(&self) -> u64 {
        self.unix
    }

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
