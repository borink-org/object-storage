//! Checksums of the content of a write.
//!
//! The service compares a checksum in the request head against the bytes it
//! receives, and refuses the write if the two differ. Azure takes an MD5 or
//! a CRC-64/NVME. S3 takes those, a CRC-32, a CRC-32C, a SHA-1 or a SHA-256:
//! see [`ChecksumKind`]. To send one with a request from a template:
//!
//! 1. Name its kind in the template's options, such as
//!    [`PutOptions::checksum`](crate::PutOptions::checksum).
//! 2. Compute the checksum of each request's content, and pass it in the
//!    request's item, such as [`PutItem::checksum`](crate::PutItem::checksum):
//!    as its base64 text, or as the [`Digest`] itself, which the request
//!    writes in the byte order of its service.
//! 3. For a body that this crate writes, compute the checksum over
//!    [`s3::Unsigned::body`](crate::s3::Unsigned::body) and write it in with
//!    [`s3::set_body_checksum`](crate::s3::set_body_checksum).
//!
//! This crate computes no checksum. The `borink-object-storage-crypto` crate
//! has an implementation of each kind.
//!
//! A checksum here detects corruption in transit and nothing else, a SHA-1 or
//! a SHA-256 included.

use crate::encoding::{base64_into, base64_len};
use crate::{ChecksumValue, InvalidRequest, Result};

/// The checksums that a write can send.
///
/// The kind decides the request header that carries the checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum ChecksumKind {
    /// MD5, sixteen bytes, sent as `Content-MD5`.
    Md5 = 1,
    /// CRC-64/NVME, eight bytes, sent as `x-ms-content-crc64` to Azure and
    /// as `x-amz-checksum-crc64nvme` to S3.
    Crc64 = 2,
    /// CRC-32, as in ISO-HDLC and zlib, four bytes, sent as
    /// `x-amz-checksum-crc32`. S3 only.
    Crc32 = 3,
    /// CRC-32C, the Castagnoli polynomial, four bytes, sent as
    /// `x-amz-checksum-crc32c`. S3 only.
    Crc32c = 4,
    /// SHA-1, twenty bytes, sent as `x-amz-checksum-sha1`. S3 only.
    Sha1 = 5,
    /// SHA-256, 32 bytes, sent as `x-amz-checksum-sha256`. S3 only.
    Sha256 = 6,
}

impl ChecksumKind {
    /// Returns the kind with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Md5,
            2 => Self::Crc64,
            3 => Self::Crc32,
            4 => Self::Crc32c,
            5 => Self::Sha1,
            6 => Self::Sha256,
            _ => return None,
        })
    }

    /// Returns the number of bytes in a digest of this kind.
    pub const fn digest_len(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Crc64 => 8,
            Self::Crc32 | Self::Crc32c => 4,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }

    // The request header that carries a checksum of this kind to Azure, or
    // an MD5 to S3.
    pub(crate) const fn header(self) -> &'static str {
        match self {
            Self::Md5 => "content-md5",
            Self::Crc64 => "x-ms-content-crc64",
            Self::Crc32 => "x-amz-checksum-crc32",
            Self::Crc32c => "x-amz-checksum-crc32c",
            Self::Sha1 => "x-amz-checksum-sha1",
            Self::Sha256 => "x-amz-checksum-sha256",
        }
    }

    // Whether Azure takes a checksum of this kind.
    pub(crate) const fn azure_takes(self) -> bool {
        matches!(self, Self::Md5 | Self::Crc64)
    }

    const fn base64_shape(self) -> (usize, usize) {
        base64_shape(self.digest_len())
    }

    // Checks that `text` is the padded base64 of a digest of this kind.
    // Azure refuses any other text with 400 `InvalidHeaderValue`.
    pub(crate) fn check_base64(self, text: &str) -> Result<()> {
        if base64_len(text.as_bytes()) != Some(self.digest_len()) {
            return Err(InvalidRequest::Checksum.into());
        }
        Ok(())
    }
}

// The length of the base64 of `len` bytes, as the characters from the
// alphabet and the `=` padding after them. Sixteen bytes are 22 and 2;
// eight bytes are 11 and 1.
const fn base64_shape(len: usize) -> (usize, usize) {
    let (groups, rest) = (len / 3, len % 3);
    if rest == 0 {
        (groups * 4, 0)
    } else {
        (groups * 4 + rest + 1, 3 - rest)
    }
}

/// The length of the longest text that [`Digest::base64`] writes, which is
/// the 44 characters of a SHA-256.
pub const BASE64_LEN: usize = 44;

/// The bytes of a finished checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digest {
    bytes: [u8; 32],
    kind: ChecksumKind,
}

impl Digest {
    // A digest of `kind` whose bytes are `digest`, as long as the kind's.
    const fn of(kind: ChecksumKind, digest: &[u8]) -> Self {
        let mut bytes = [0; 32];
        let mut index = 0;
        while index < digest.len() {
            bytes[index] = digest[index];
            index += 1;
        }
        Self { bytes, kind }
    }

    /// Creates the digest of an MD5 from its sixteen bytes.
    pub const fn md5(bytes: [u8; 16]) -> Self {
        Self::of(ChecksumKind::Md5, &bytes)
    }

    /// Creates the digest of a CRC64 from its value.
    ///
    /// The digest holds the eight little-endian bytes of `value`, which is
    /// the order that Azure reads. An S3 client sends them in big-endian
    /// order.
    pub const fn crc64(value: u64) -> Self {
        Self::of(ChecksumKind::Crc64, &value.to_le_bytes())
    }

    /// Creates the digest of a CRC-32 from its value, held as its four
    /// big-endian bytes, the order that S3 reads.
    pub const fn crc32(value: u32) -> Self {
        Self::of(ChecksumKind::Crc32, &value.to_be_bytes())
    }

    /// Creates the digest of a CRC-32C from its value, held as its four
    /// big-endian bytes, the order that S3 reads.
    pub const fn crc32c(value: u32) -> Self {
        Self::of(ChecksumKind::Crc32c, &value.to_be_bytes())
    }

    /// Creates the digest of a SHA-1 from its twenty bytes.
    pub const fn sha1(bytes: [u8; 20]) -> Self {
        Self::of(ChecksumKind::Sha1, &bytes)
    }

    /// Creates the digest of a SHA-256 from its 32 bytes.
    pub const fn sha256(bytes: [u8; 32]) -> Self {
        Self::of(ChecksumKind::Sha256, &bytes)
    }

    /// Returns which checksum this is.
    pub const fn kind(&self) -> ChecksumKind {
        self.kind
    }

    /// Returns the bytes of the digest, as many as
    /// [`ChecksumKind::digest_len`] states for its kind.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.kind.digest_len()]
    }

    // Writes the base64 of the digest in the byte order a service reads.
    pub(crate) fn base64_for<'a>(
        &self,
        into: &'a mut [u8; BASE64_LEN],
        crc64_big_endian: bool,
    ) -> &'a str {
        let mut digest = *self;
        if crc64_big_endian && self.kind == ChecksumKind::Crc64 {
            digest.bytes[..8].reverse();
        }
        digest.base64(into)
    }

    /// Writes the base64 of the digest into `into` and returns it as text.
    ///
    /// The text is 24 characters for an MD5 and 12 for a CRC64, for example.
    /// A CRC64 is written in the little-endian order that Azure reads.
    pub fn base64<'a>(&self, into: &'a mut [u8; BASE64_LEN]) -> &'a str {
        let (chars, padding) = self.kind.base64_shape();
        base64_into(self.as_bytes(), &mut into[..chars + padding])
    }
}

// The base64 text of a write's checksum, written into `into` from a digest.
// S3 reads a CRC64 in big-endian order, Azure in little-endian order.
pub(crate) fn checksum_base64<'a>(
    value: Option<ChecksumValue<'a>>,
    into: &'a mut [u8; BASE64_LEN],
    crc64_big_endian: bool,
) -> Option<&'a str> {
    match value? {
        ChecksumValue::Base64(text) => Some(text),
        ChecksumValue::Digest(digest) => Some(digest.base64_for(into, crc64_big_endian)),
    }
}
