use borink_object_storage_proto::checksum::{ChecksumKind, Digest};

use crate::Checksum;

// Both CRCs are reflected, start from all ones and end inverted, and differ
// only in their polynomial. Each reads one byte at a time through a
// 256-entry table built from the reflection of its polynomial, so that the
// loop shifts right.
const fn build_table(polynomial: u32) -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ polynomial
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

fn advance(table: &[u32; 256], mut state: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        state = table[((state ^ u32::from(byte)) & 0xff) as usize] ^ (state >> 8);
    }
    state
}

/// The CRC-32 of the content, as zlib and ISO-HDLC compute it, which S3
/// checks as `x-amz-checksum-crc32`.
#[cfg(feature = "crc32")]
#[derive(Debug, Clone)]
pub struct Crc32(u32);

// The reflection of `0x04c1_1db7`. The catalogue's check value, the CRC of
// `123456789`, is `0xcbf4_3926`.
#[cfg(feature = "crc32")]
const CRC32_TABLE: [u32; 256] = build_table(0xedb8_8320);

#[cfg(feature = "crc32")]
impl Default for Crc32 {
    fn default() -> Self {
        Self(u32::MAX)
    }
}

#[cfg(feature = "crc32")]
impl Crc32 {
    /// Returns the CRC-32 of `bytes`.
    pub fn of(bytes: &[u8]) -> u32 {
        advance(&CRC32_TABLE, u32::MAX, bytes) ^ u32::MAX
    }
}

#[cfg(feature = "crc32")]
impl Checksum for Crc32 {
    const KIND: ChecksumKind = ChecksumKind::Crc32;

    fn update(&mut self, bytes: &[u8]) {
        self.0 = advance(&CRC32_TABLE, self.0, bytes);
    }

    fn finish(self) -> Digest {
        Digest::crc32(self.0 ^ u32::MAX)
    }
}

/// The CRC-32C of the content, with the Castagnoli polynomial, which S3
/// checks as `x-amz-checksum-crc32c`.
#[cfg(feature = "crc32c")]
#[derive(Debug, Clone)]
pub struct Crc32c(u32);

// The reflection of `0x1edc_6f41`. The catalogue's check value is
// `0xe306_9283`.
#[cfg(feature = "crc32c")]
const CRC32C_TABLE: [u32; 256] = build_table(0x82f6_3b78);

#[cfg(feature = "crc32c")]
impl Default for Crc32c {
    fn default() -> Self {
        Self(u32::MAX)
    }
}

#[cfg(feature = "crc32c")]
impl Crc32c {
    /// Returns the CRC-32C of `bytes`.
    pub fn of(bytes: &[u8]) -> u32 {
        advance(&CRC32C_TABLE, u32::MAX, bytes) ^ u32::MAX
    }
}

#[cfg(feature = "crc32c")]
impl Checksum for Crc32c {
    const KIND: ChecksumKind = ChecksumKind::Crc32c;

    fn update(&mut self, bytes: &[u8]) {
        self.0 = advance(&CRC32C_TABLE, self.0, bytes);
    }

    fn finish(self) -> Digest {
        Digest::crc32c(self.0 ^ u32::MAX)
    }
}
