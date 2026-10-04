use crate::Sha256;

/// SHA-256 from the `hmac-sha256` crate.
///
/// `hmac-sha256` has no dependencies and needs no `std`. It is portable
/// code, and uses no SHA extensions of the CPU.
#[derive(Clone, Copy)]
pub struct Sha256Minimal(hmac_sha256::Hash);

impl Default for Sha256Minimal {
    fn default() -> Self {
        Self(hmac_sha256::Hash::new())
    }
}

impl core::fmt::Debug for Sha256Minimal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sha256Minimal").finish_non_exhaustive()
    }
}

impl Sha256 for Sha256Minimal {
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize()
    }
}

// The same SHA-256 as the checksum of a write's content, which S3 checks as
// `x-amz-checksum-sha256`.
impl crate::Checksum for Sha256Minimal {
    const KIND: borink_object_storage_proto::checksum::ChecksumKind =
        borink_object_storage_proto::checksum::ChecksumKind::Sha256;

    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(self) -> borink_object_storage_proto::checksum::Digest {
        borink_object_storage_proto::checksum::Digest::sha256(self.0.finalize())
    }
}
