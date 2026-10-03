use ::sha1::{Digest as _, Sha1 as Hasher};
use borink_object_storage_proto::checksum::{ChecksumKind, Digest};

use crate::Checksum;

/// The SHA-1 of the content, which S3 checks as `x-amz-checksum-sha1`.
///
/// This type adapts the hasher of RustCrypto's `sha1` crate to
/// [`Checksum`]. This crate computes no SHA-1 of its own.
#[derive(Debug, Clone, Default)]
pub struct Sha1RustCrypto(Hasher);

impl Checksum for Sha1RustCrypto {
    const KIND: ChecksumKind = ChecksumKind::Sha1;

    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(self) -> Digest {
        Digest::sha1(self.0.finalize().into())
    }
}
