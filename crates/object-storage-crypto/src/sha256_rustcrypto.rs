use hmac::{KeyInit, Mac};
use sha2::Digest as _;

use crate::Sha256;

/// SHA-256 and HMAC-SHA256 from RustCrypto's `sha2` and `hmac` crates.
///
/// `sha2` uses the SHA extensions of x86_64 and aarch64 when the CPU has
/// them, and portable code elsewhere. It needs no `std`.
#[derive(Debug, Clone, Default)]
pub struct Sha256RustCrypto(sha2::Sha256);

impl Sha256 for Sha256RustCrypto {
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }

    fn hmac(key: &[u8], message: &[u8]) -> [u8; 32] {
        // HMAC takes a key of any length, so this never fails.
        let Ok(mut mac) = <hmac::Hmac<sha2::Sha256> as KeyInit>::new_from_slice(key) else {
            unreachable!("HMAC accepts a key of any length");
        };
        mac.update(message);
        mac.finalize().into_bytes().into()
    }
}
