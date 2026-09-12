//! Checksums of the content of a write.
//!
//! Azure compares a checksum in the request head against the bytes it
//! receives, and refuses the write if the two differ. To send one:
//!
//! 1. Compute the MD5 or the CRC-64/NVME of the content.
//! 2. Write its base64. [`Digest::base64`] does this for a [`Digest`].
//! 3. Put the text in [`WriteOptions::checksum`](crate::WriteOptions::checksum)
//!    as [`TransactionalChecksum::Md5`](crate::TransactionalChecksum::Md5) or
//!    [`TransactionalChecksum::Crc64`](crate::TransactionalChecksum::Crc64).
//!
//! To have the encoder compute it instead:
//!
//! 1. Register a [`ChecksumProvider`] of that kind with
//!    [`Blobs::with_checksum`](crate::Blobs::with_checksum).
//! 2. Put [`TransactionalChecksum::Compute`](crate::TransactionalChecksum::Compute)
//!    of that kind in `WriteOptions::checksum`.
//!
//! The encoder then sums the content of the request and writes the header.
//! It can only sum content that it holds: a
//! [`Payload::Slice`](crate::Payload::Slice), or the block list of a commit.
//! For a streamed payload, compute the checksum yourself before you encode
//! and pass the text.
//!
//! # Providers
//!
//! This crate computes no checksum. A provider is three function pointers,
//! `start`, `update` and `finish`, which keep their state in a
//! [`ChecksumState`]. The `borink-crypto` crate has a CRC-64/NVME, an adapter
//! for RustCrypto's `md-5`, and a trait that turns any implementation into a
//! provider. Register one of those, or build your own with
//! [`ChecksumProvider::new`].
//!
//! Neither checksum is cryptography. Both detect corruption in transit and
//! nothing else.

use core::mem::MaybeUninit;

use crate::{InvalidPlan, Result};

/// The checksums that a write can send.
///
/// The kind decides the request header that carries the checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum ChecksumKind {
    /// MD5, sixteen bytes, sent as `Content-MD5`.
    Md5 = 1,
    /// CRC-64/NVME, eight bytes, sent as `x-ms-content-crc64`.
    Crc64 = 2,
}

impl ChecksumKind {
    /// Returns the kind with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Md5,
            2 => Self::Crc64,
            _ => return None,
        })
    }

    /// Returns the number of bytes in a digest of this kind.
    pub const fn digest_len(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Crc64 => 8,
        }
    }

    // The request header that carries a checksum of this kind.
    pub(crate) const fn header(self) -> &'static str {
        match self {
            Self::Md5 => "content-md5",
            Self::Crc64 => "x-ms-content-crc64",
        }
    }

    // Where a provider of this kind sits in a client's table.
    pub(crate) const fn slot(self) -> usize {
        match self {
            Self::Md5 => 0,
            Self::Crc64 => 1,
        }
    }

    // The length of the base64 of a digest of this kind, as the characters
    // from the alphabet and the `=` padding after them. Sixteen bytes are 22
    // and 2; eight bytes are 11 and 1.
    const fn base64_shape(self) -> (usize, usize) {
        let len = self.digest_len();
        let (groups, rest) = (len / 3, len % 3);
        if rest == 0 {
            (groups * 4, 0)
        } else {
            (groups * 4 + rest + 1, 3 - rest)
        }
    }

    // Checks that `text` is the base64 of a digest of this kind: the right
    // number of characters from the alphabet, then the right number of `=`.
    // Azure refuses any other text with 400 `InvalidHeaderValue`.
    pub(crate) fn check_base64(self, text: &str) -> Result<()> {
        let (chars, padding) = self.base64_shape();
        let bytes = text.as_bytes();
        let alphabet = |byte: &u8| byte.is_ascii_alphanumeric() || *byte == b'+' || *byte == b'/';
        if bytes.len() != chars + padding
            || !bytes[..chars].iter().all(alphabet)
            || !bytes[chars..].iter().all(|byte| *byte == b'=')
        {
            return Err(InvalidPlan::Checksum.into());
        }
        Ok(())
    }
}

/// The number of kinds. A client holds one provider slot per kind.
pub(crate) const KINDS: usize = 2;

/// The length of the longest text that [`Digest::base64`] writes, which is
/// the 24 characters of an MD5.
pub const BASE64_LEN: usize = 24;

/// The bytes of a finished checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digest {
    bytes: [u8; 16],
    kind: ChecksumKind,
}

impl Digest {
    /// Creates the digest of an MD5 from its sixteen bytes.
    pub const fn md5(bytes: [u8; 16]) -> Self {
        Self {
            bytes,
            kind: ChecksumKind::Md5,
        }
    }

    /// Creates the digest of a CRC64 from its value.
    ///
    /// The digest holds the eight little-endian bytes of `value`, which is
    /// the order that Azure reads.
    pub const fn crc64(value: u64) -> Self {
        let le = value.to_le_bytes();
        let mut bytes = [0; 16];
        let mut index = 0;
        while index < 8 {
            bytes[index] = le[index];
            index += 1;
        }
        Self {
            bytes,
            kind: ChecksumKind::Crc64,
        }
    }

    /// Returns which checksum this is.
    pub const fn kind(&self) -> ChecksumKind {
        self.kind
    }

    /// Returns the bytes of the digest: sixteen for an MD5, eight for a CRC64.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.kind.digest_len()]
    }

    /// Writes the base64 of the digest into `into` and returns it as text.
    ///
    /// The text is 24 characters for an MD5 and 12 for a CRC64. Pass it as
    /// [`TransactionalChecksum::Md5`](crate::TransactionalChecksum::Md5) or
    /// [`TransactionalChecksum::Crc64`](crate::TransactionalChecksum::Crc64).
    pub fn base64<'a>(&self, into: &'a mut [u8; BASE64_LEN]) -> &'a str {
        let (chars, padding) = self.kind.base64_shape();
        crate::layered::base64_into(self.as_bytes(), &mut into[..chars + padding])
    }
}

/// The bytes in which a provider keeps a checksum while it is computed.
///
/// The encoder creates one with [`Self::uninit`], passes it to the
/// provider's `start`, then to each `update`, then to `finish`, and drops
/// it. Only the provider reads or writes the bytes. The encoder does not
/// interpret them.
///
/// The slot is [`Self::LEN`] bytes long and aligned to sixteen bytes. It is
/// on the encoder's stack: nothing here is allocated.
#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub struct ChecksumState {
    bytes: [MaybeUninit<u8>; ChecksumState::LEN],
}

impl ChecksumState {
    /// The number of bytes in the slot.
    ///
    /// A provider's state must fit in it. An MD5 as RustCrypto keeps it takes
    /// 88 bytes, and a CRC-64 takes eight. `borink-crypto` refuses at compile
    /// time to build a provider whose state is larger than this.
    pub const LEN: usize = 128;

    /// Creates a slot whose bytes are not written yet.
    ///
    /// A provider's `start` writes them.
    pub const fn uninit() -> Self {
        Self {
            bytes: [MaybeUninit::uninit(); Self::LEN],
        }
    }

    /// Returns a pointer to the first of the [`Self::LEN`] bytes.
    pub fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr().cast()
    }

    /// Returns a mutable pointer to the first of the [`Self::LEN`] bytes.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr().cast()
    }
}

impl core::fmt::Debug for ChecksumState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChecksumState").finish_non_exhaustive()
    }
}

/// An implementation of one checksum, which you register with
/// [`Blobs::with_checksum`](crate::Blobs::with_checksum).
///
/// A provider holds the kind it computes and three function pointers. For
/// each write that computes a checksum, the encoder calls `start` once,
/// `update` once for each piece of the content in order, and `finish` once.
/// The state lives for that one encoding call. The provider keeps nothing
/// between requests.
///
/// A provider holds no borrows, so it can be a `const`. The providers of
/// `borink-crypto` are.
#[derive(Clone, Copy)]
pub struct ChecksumProvider {
    kind: ChecksumKind,
    start: fn() -> ChecksumState,
    update: fn(&mut ChecksumState, &[u8]),
    finish: fn(ChecksumState) -> Digest,
}

impl ChecksumProvider {
    /// Creates a provider from the three calls that compute one checksum.
    ///
    /// `finish` must return a [`Digest`] of `kind`. The encoder writes the
    /// header that `kind` names and the base64 of what `finish` returned.
    pub const fn new(
        kind: ChecksumKind,
        start: fn() -> ChecksumState,
        update: fn(&mut ChecksumState, &[u8]),
        finish: fn(ChecksumState) -> Digest,
    ) -> Self {
        Self {
            kind,
            start,
            update,
            finish,
        }
    }

    /// Returns which checksum this computes.
    pub const fn kind(&self) -> ChecksumKind {
        self.kind
    }

    // Starts a checksum of some content.
    pub(crate) fn start(&self) -> Sum {
        Sum {
            provider: *self,
            state: (self.start)(),
        }
    }
}

impl core::fmt::Debug for ChecksumProvider {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChecksumProvider")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

// A checksum in progress: the provider and the state it keeps between
// pieces of the content.
pub(crate) struct Sum {
    provider: ChecksumProvider,
    state: ChecksumState,
}

impl Sum {
    // Adds the next piece of the content.
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        (self.provider.update)(&mut self.state, bytes);
    }

    // Returns the checksum of every piece added so far.
    pub(crate) fn finish(self) -> Digest {
        let digest = (self.provider.finish)(self.state);
        debug_assert_eq!(
            digest.kind(),
            self.provider.kind,
            "a provider returned a digest of another kind"
        );
        digest
    }
}
