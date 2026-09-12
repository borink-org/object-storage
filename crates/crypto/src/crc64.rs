use borink_object_storage_proto::checksum::{ChecksumKind, Digest};

use crate::Checksum;

/// The CRC-64/NVME of the content, which Azure checks as
/// `x-ms-content-crc64` and S3 as `x-amz-checksum-crc64nvme`.
///
/// This implementation reads one byte at a time through a 256-entry table.
#[derive(Debug, Clone)]
pub struct Crc64(u64);

// The reflection of the polynomial `0xad93_d235_94c9_3659`, so that the loop
// shifts right. The catalogue's check value, the CRC of `123456789`, is
// `0xae8b_1486_0a79_9888`; `crc64_matches_the_catalogue` asserts it.
const POLYNOMIAL: u64 = 0x9a6c_9329_ac4b_c9b5;

const TABLE: [u64; 256] = build_table();

const fn build_table() -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u64;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ POLYNOMIAL
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

impl Default for Crc64 {
    fn default() -> Self {
        Self(u64::MAX)
    }
}

impl Crc64 {
    /// Returns the CRC-64/NVME of `bytes`.
    ///
    /// This is the value that [`Checksum::finish`] returns after `bytes` was
    /// added, whether at once or piece by piece.
    pub fn of(bytes: &[u8]) -> u64 {
        let mut crc = Self::default();
        crc.update(bytes);
        crc.0 ^ u64::MAX
    }
}

impl Checksum for Crc64 {
    const KIND: ChecksumKind = ChecksumKind::Crc64;

    fn update(&mut self, bytes: &[u8]) {
        let mut state = self.0;
        for &byte in bytes {
            state = TABLE[((state ^ u64::from(byte)) & 0xff) as usize] ^ (state >> 8);
        }
        self.0 = state;
    }

    fn finish(self) -> Digest {
        Digest::crc64(self.0 ^ u64::MAX)
    }
}
