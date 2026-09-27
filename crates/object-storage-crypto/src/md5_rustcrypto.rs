use ::md5::{Digest as _, Md5 as Hasher};
use borink_object_storage_proto::checksum::{ChecksumKind, Digest};

use crate::Checksum;

/// The MD5 of the content, which Azure and S3 check as `Content-MD5`.
///
/// This type adapts the hasher of RustCrypto's `md-5` crate to
/// [`Checksum`]. This crate computes no MD5 of its own.
#[derive(Debug, Clone, Default)]
pub struct Md5RustCrypto(Hasher);

impl Checksum for Md5RustCrypto {
    const KIND: ChecksumKind = ChecksumKind::Md5;

    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(self) -> Digest {
        Digest::md5(self.0.finalize().into())
    }
}
