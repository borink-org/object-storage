//! Checksum and SHA-256 implementations for
//! [`borink-object-storage-proto`].
//!
//! # Checksums
//!
//! The protocol crate computes no checksum. A template names the kind of
//! checksum that its requests send, and each request takes the value in its
//! item, such as
//! [`PutItem::checksum`](borink_object_storage_proto::PutItem::checksum).
//! Compute the value with a [`Checksum`]: create it, add the content, and
//! pass the digest.
//!
//! ```
//! # #[cfg(feature = "crc64")] {
//! use borink_object_storage_crypto::{Checksum, Crc64};
//! use borink_object_storage_proto::ChecksumValue;
//!
//! let mut crc = Crc64::default();
//! crc.update(b"the content of the write");
//! let checksum = ChecksumValue::Digest(crc.finish());
//! # }
//! ```
//!
//! Each implementation is behind a feature, and none is on by default:
//!
//! - `crc64` builds [`Crc64`], this crate's own CRC-64/NVME.
//! - `md5-rustcrypto` builds [`Md5RustCrypto`], an adapter for RustCrypto's
//!   `md-5`.
//! - `crc32` and `crc32c` build [`Crc32`] and [`Crc32c`], this crate's own.
//!   Only S3 takes them.
//! - `sha1-rustcrypto` builds [`Sha1RustCrypto`], an adapter for RustCrypto's
//!   `sha1`. Only S3 takes it.
//! - The SHA-256 implementations below are checksums as well. Only S3 takes
//!   them.
//!
//! A checksum here detects corruption in transit and nothing else, a SHA-1
//! or a SHA-256 included.
//!
//! # SHA-256
//!
//! The [`sigv4`] module signs S3 requests with any [`Sha256`]. This crate
//! has two, each behind a feature of its name:
//!
//! - [`Sha256RustCrypto`] (`sha256-rustcrypto`), over RustCrypto's `sha2`.
//!   It needs no `std`, runs on every target, and uses the CPU's SHA
//!   extensions where it finds them.
//! - [`Sha256Minimal`] (`sha256-minimal`), over `hmac-sha256`. That crate
//!   has no dependencies and needs no `std`. It is portable code, and uses
//!   no SHA extensions.
//!
//! Pass [`wipe`] to
//! [`Credentials::new`](borink_object_storage_proto::sigv4::Credentials::new).
//! [`sigv4::signing_key`] calls it on the copies of the secret access key
//! that it makes. The `zeroize` feature routes it through the `zeroize`
//! crate. A [`sigv4::SigningKey`] and a [`sigv4::Stamp`] hold SHA-256
//! states derived from the secret, and nothing wipes them.
//!
//! [`borink-object-storage-proto`]: borink_object_storage_proto

#![no_std]

use borink_object_storage_proto::checksum::{ChecksumKind, Digest};

#[cfg(any(feature = "crc32", feature = "crc32c"))]
mod crc32;
#[cfg(feature = "crc64")]
mod crc64;
#[cfg(feature = "md5-rustcrypto")]
mod md5_rustcrypto;
#[cfg(feature = "sha1-rustcrypto")]
mod sha1_rustcrypto;
#[cfg(feature = "sha256-minimal")]
mod sha256_minimal;
#[cfg(feature = "sha256-rustcrypto")]
mod sha256_rustcrypto;
pub mod sigv4;

#[cfg(feature = "crc32")]
pub use crc32::Crc32;
#[cfg(feature = "crc32c")]
pub use crc32::Crc32c;
#[cfg(feature = "crc64")]
pub use crc64::Crc64;
#[cfg(feature = "md5-rustcrypto")]
pub use md5_rustcrypto::Md5RustCrypto;
#[cfg(feature = "sha1-rustcrypto")]
pub use sha1_rustcrypto::Sha1RustCrypto;
#[cfg(feature = "sha256-minimal")]
pub use sha256_minimal::Sha256Minimal;
#[cfg(feature = "sha256-rustcrypto")]
pub use sha256_rustcrypto::Sha256RustCrypto;

/// A checksum computed one piece of the content at a time.
///
/// Create the value with [`Default`], call [`Self::update`] with each piece
/// of the content in order, and call [`Self::finish`] once.
pub trait Checksum: Default {
    /// The kind of checksum this computes, which decides the header that
    /// carries it.
    const KIND: ChecksumKind;

    /// Adds `bytes` to the content seen so far.
    fn update(&mut self, bytes: &[u8]);

    /// Returns the checksum of everything added so far, as a digest of
    /// [`Self::KIND`].
    fn finish(self) -> Digest;
}

/// SHA-256 computed one piece of the input at a time.
///
/// Implement this for a SHA-256 of your own, such as a library you already
/// link. [`sigv4`] creates the value with [`Default`], calls
/// [`Self::update`] with each piece of the input in order, and calls
/// [`Self::finish`] once. It builds HMAC-SHA256 from these calls itself.
///
/// The value is [`Copy`]: a [`sigv4::SigningKey`] keeps the state of a hash
/// after the padding block of the key, and each signature starts from a copy
/// of it.
pub trait Sha256: Default + Copy {
    /// Adds `bytes` to the input seen so far.
    fn update(&mut self, bytes: &[u8]);

    /// Returns the SHA-256 of everything added so far.
    fn finish(self) -> [u8; 32];
}

/// Sets every byte of `bytes` to zero, in writes that the compiler keeps.
///
/// Pass it to
/// [`Credentials::new`](borink_object_storage_proto::sigv4::Credentials::new),
/// which calls it on its copy of the secret access key.
///
/// With the `zeroize` feature, this calls the `zeroize` crate. Without it,
/// it writes each byte with [`core::ptr::write_volatile`] and then fences,
/// as `zeroize` does.
pub fn wipe(bytes: &mut [u8]) {
    #[cfg(feature = "zeroize")]
    zeroize::Zeroize::zeroize(bytes);
    #[cfg(not(feature = "zeroize"))]
    {
        for byte in bytes.iter_mut() {
            // SAFETY: `byte` is a unique borrow of one initialized byte.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        // Keeps later accesses from moving before the writes.
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}
