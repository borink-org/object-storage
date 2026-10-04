use sha2::block_api::compress256;

use crate::Sha256;

/// SHA-256 over the compression function of RustCrypto's `sha2` crate.
///
/// `sha2` uses the SHA extensions of x86_64 and aarch64 when the CPU has
/// them, and portable code elsewhere. It needs no `std`.
///
/// This type keeps the state, the partial block and the length itself, so
/// that it is [`Copy`], as a provider's SHA-256 must be.
#[derive(Debug, Clone, Copy)]
pub struct Sha256RustCrypto {
    state: [u32; 8],
    // The bytes of the block not yet compressed, `block[..filled]`.
    block: [u8; 64],
    filled: usize,
    // The bytes added so far.
    len: u64,
}

// The initial hash value of FIPS 180-4, section 5.3.3.
const INITIAL: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

impl Default for Sha256RustCrypto {
    fn default() -> Self {
        Self {
            state: INITIAL,
            block: [0; 64],
            filled: 0,
            len: 0,
        }
    }
}

impl Sha256RustCrypto {
    fn add(&mut self, mut bytes: &[u8]) {
        self.len += bytes.len() as u64;
        if self.filled > 0 {
            let take = bytes.len().min(64 - self.filled);
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled < 64 {
                return;
            }
            compress256(&mut self.state, core::slice::from_ref(&self.block));
            self.filled = 0;
        }
        let (blocks, rest) = bytes.as_chunks::<64>();
        if !blocks.is_empty() {
            compress256(&mut self.state, blocks);
        }
        self.block[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }

    // Pads the message as FIPS 180-4, section 5.1.1 says, and returns the
    // state as bytes.
    fn digest(mut self) -> [u8; 32] {
        let bits = self.len.wrapping_mul(8);
        self.block[self.filled] = 0x80;
        self.block[self.filled + 1..].fill(0);
        if self.filled >= 56 {
            compress256(&mut self.state, core::slice::from_ref(&self.block));
            self.block = [0; 64];
        }
        self.block[56..].copy_from_slice(&bits.to_be_bytes());
        compress256(&mut self.state, core::slice::from_ref(&self.block));
        let mut out = [0; 32];
        for (bytes, word) in out.chunks_exact_mut(4).zip(self.state) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

impl Sha256 for Sha256RustCrypto {
    fn update(&mut self, bytes: &[u8]) {
        self.add(bytes);
    }

    fn finish(self) -> [u8; 32] {
        self.digest()
    }
}

// The same SHA-256 as the checksum of a write's content, which S3 checks as
// `x-amz-checksum-sha256`.
impl crate::Checksum for Sha256RustCrypto {
    const KIND: borink_object_storage_proto::checksum::ChecksumKind =
        borink_object_storage_proto::checksum::ChecksumKind::Sha256;

    fn update(&mut self, bytes: &[u8]) {
        self.add(bytes);
    }

    fn finish(self) -> borink_object_storage_proto::checksum::Digest {
        borink_object_storage_proto::checksum::Digest::sha256(self.digest())
    }
}
