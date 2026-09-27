//! Checksum and SHA-256 implementations for
//! [`borink-object-storage-proto`], and the traits that turn one into a
//! provider that crate can call.
//!
//! # Checksums
//!
//! The protocol crate computes no checksum. To have it compute one for a
//! write, register a provider with
//! [`Blobs::with_checksum`](borink_object_storage_proto::Blobs::with_checksum),
//! then ask for
//! [`TransactionalChecksum::Compute`](borink_object_storage_proto::TransactionalChecksum::Compute)
//! in the plan of the write.
//!
//! ```
//! # #[cfg(feature = "crc64")] {
//! use borink_object_storage_proto::{Blobs, Container};
//!
//! let container = Container::new("https://account.blob.core.windows.net", "objects")?;
//! let blobs = Blobs::new(container, "access-token")?
//!     .with_checksum(borink_object_storage_crypto::CRC64);
//! # }
//! # Ok::<(), borink_object_storage_proto::Error>(())
//! ```
//!
//! [`CRC64`] and [`MD5_RUSTCRYPTO`] are the checksum providers of this
//! crate. Each is behind a feature, and neither is on by default, so you
//! link the one you register and nothing else. The `crc64` feature builds
//! [`Crc64`], this crate's own CRC-64/NVME. The `md5-rustcrypto` feature
//! builds [`Md5RustCrypto`], an adapter for RustCrypto's `md-5`.
//!
//! To register an implementation of your own, implement [`Checksum`] for it
//! and pass the type to [`provider`].
//!
//! Neither checksum is cryptography. Both detect corruption in transit and
//! nothing else.
//!
//! # SHA-256
//!
//! An S3 client signs each request with SHA-256 and HMAC-SHA256. Pass a
//! provider to
//! [`s3::Objects::new`](borink_object_storage_proto::s3::Objects::new):
//!
//! ```
//! # #[cfg(feature = "sha256-rustcrypto")] {
//! use borink_object_storage_proto::s3::{Bucket, Service, Objects};
//! use borink_object_storage_proto::sigv4::Credentials;
//!
//! let bucket = Bucket::new(
//!     "https://s3.eu-west-1.amazonaws.com", "objects", "eu-west-1", Service::Aws,
//! )?;
//! let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret")?;
//! let objects = Objects::new(
//!     bucket, credentials, borink_object_storage_crypto::SHA256_RUSTCRYPTO,
//! );
//! # }
//! # Ok::<(), borink_object_storage_proto::Error>(())
//! ```
//!
//! This crate has two providers, each behind a feature of its name:
//!
//! - [`SHA256_RUSTCRYPTO`], over RustCrypto's `sha2` and `hmac`. It needs no
//!   `std`, runs on every target, and uses the CPU's SHA extensions where it
//!   finds them.
//! - [`SHA256_MINIMAL`], over `hmac-sha256`. That crate has no dependencies
//!   and needs no `std`. It is portable code, and uses no SHA extensions.
//!
//! To register an implementation of your own, implement [`Sha256`] for it
//! and pass the type to [`sha256_provider`].
//!
//! [`borink-object-storage-proto`]: borink_object_storage_proto

#![no_std]

use borink_object_storage_proto::checksum::{
    ChecksumKind, ChecksumProvider, ChecksumState, Digest,
};
use borink_object_storage_proto::sigv4::{Sha256Provider, Sha256State};

#[cfg(feature = "crc64")]
mod crc64;
#[cfg(feature = "md5-rustcrypto")]
mod md5_rustcrypto;
#[cfg(feature = "sha256-minimal")]
mod sha256_minimal;
#[cfg(feature = "sha256-rustcrypto")]
mod sha256_rustcrypto;

#[cfg(feature = "crc64")]
pub use crc64::Crc64;
#[cfg(feature = "md5-rustcrypto")]
pub use md5_rustcrypto::Md5RustCrypto;
#[cfg(feature = "sha256-minimal")]
pub use sha256_minimal::Sha256Minimal;
#[cfg(feature = "sha256-rustcrypto")]
pub use sha256_rustcrypto::Sha256RustCrypto;

/// [`Crc64`] as a provider.
#[cfg(feature = "crc64")]
pub const CRC64: ChecksumProvider = provider::<Crc64>();

/// [`Md5RustCrypto`] as a provider.
#[cfg(feature = "md5-rustcrypto")]
pub const MD5_RUSTCRYPTO: ChecksumProvider = provider::<Md5RustCrypto>();

/// [`Sha256RustCrypto`] as a provider.
#[cfg(feature = "sha256-rustcrypto")]
pub const SHA256_RUSTCRYPTO: Sha256Provider = sha256_provider::<Sha256RustCrypto>();

/// [`Sha256Minimal`] as a provider.
#[cfg(feature = "sha256-minimal")]
pub const SHA256_MINIMAL: Sha256Provider = sha256_provider::<Sha256Minimal>();

/// A checksum computed one piece of the content at a time.
///
/// Implement this for a checksum of your own, such as a hardware CRC or a
/// library you already link, and pass the type to [`provider`]. The
/// encoder creates the value with [`Default`], calls [`Self::update`] with
/// each piece of the content in order, and calls [`Self::finish`] once.
///
/// # Size
///
/// The value lives in a [`ChecksumState`] on the encoder's stack. It must be
/// no larger than [`ChecksumState::LEN`] bytes and aligned to no more than
/// sixteen. [`provider`] fails to compile for a type that breaks either
/// limit.
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

/// Returns a provider that computes `C`.
///
/// Register it with
/// [`Blobs::with_checksum`](borink_object_storage_proto::Blobs::with_checksum).
/// This is a `const fn`, so the result can be a `const`, as [`CRC64`] and
/// [`MD5_RUSTCRYPTO`] are.
///
/// # Compile-time check
///
/// A `C` larger than [`ChecksumState::LEN`] bytes, or aligned to more than
/// sixteen, fails to compile:
///
/// ```compile_fail
/// use borink_object_storage_crypto::{Checksum, provider};
/// use borink_object_storage_proto::checksum::{ChecksumKind, Digest};
///
/// #[derive(Default)]
/// struct Wide([u64; 32]);
///
/// impl Checksum for Wide {
///     const KIND: ChecksumKind = ChecksumKind::Crc64;
///     fn update(&mut self, _: &[u8]) {}
///     fn finish(self) -> Digest {
///         Digest::crc64(0)
///     }
/// }
///
/// let too_wide = provider::<Wide>();
/// ```
pub const fn provider<C: Checksum>() -> ChecksumProvider {
    ChecksumProvider::new(C::KIND, start::<C>, update::<C>, finish::<C>)
}

// A `C` lives in the state slot for the length of one encoding call. Each
// of the three functions below evaluates this in an inline `const`, so a
// provider of a `C` that does not fit fails to compile.
const fn assert_fits<C>() {
    assert!(
        size_of::<C>() <= ChecksumState::LEN,
        "a checksum larger than ChecksumState::LEN cannot be a provider",
    );
    assert!(
        align_of::<C>() <= 16,
        "a checksum aligned to more than sixteen bytes cannot be a provider",
    );
}

fn start<C: Checksum>() -> ChecksumState {
    const { assert_fits::<C>() };
    let mut state = ChecksumState::uninit();
    // SAFETY: the slot is `ChecksumState::LEN` bytes aligned to sixteen, and
    // a `C` fits in both, so this writes a whole `C` inside the slot.
    unsafe { state.as_mut_ptr().cast::<C>().write(C::default()) };
    state
}

fn update<C: Checksum>(state: &mut ChecksumState, bytes: &[u8]) {
    const { assert_fits::<C>() };
    // SAFETY: a client only ever calls this on a state that `start::<C>`
    // wrote a `C` into, and nothing between the two reads or writes the
    // bytes, so the slot holds a live `C` that this borrow does not alias.
    let checksum = unsafe { &mut *state.as_mut_ptr().cast::<C>() };
    checksum.update(bytes);
}

fn finish<C: Checksum>(mut state: ChecksumState) -> Digest {
    const { assert_fits::<C>() };
    // SAFETY: as in `update`, the slot holds a live `C`. The state is taken
    // by value and is dropped here, so the `C` is moved out exactly once and
    // the bytes are never read again.
    let checksum = unsafe { state.as_mut_ptr().cast::<C>().read() };
    checksum.finish()
}

/// SHA-256 computed one piece of the input at a time, and HMAC-SHA256
/// computed in one call.
///
/// Implement this for a SHA-256 of your own, such as a library you already
/// link, and pass the type to [`sha256_provider`]. The encoder creates the
/// value with [`Default`], calls [`Self::update`] with each piece of the
/// input in order, and calls [`Self::finish`] once.
///
/// # Size
///
/// The value lives in a [`Sha256State`] on the encoder's stack. It must be
/// no larger than [`Sha256State::LEN`] bytes and aligned to no more than
/// sixteen. [`sha256_provider`] fails to compile for a type that breaks
/// either limit.
pub trait Sha256: Default {
    /// Adds `bytes` to the input seen so far.
    fn update(&mut self, bytes: &[u8]);

    /// Returns the SHA-256 of everything added so far.
    fn finish(self) -> [u8; 32];

    /// Returns the HMAC-SHA256 of `message` under `key`.
    fn hmac(key: &[u8], message: &[u8]) -> [u8; 32];
}

/// Returns a provider that computes SHA-256 and HMAC-SHA256 with `S`.
///
/// Pass it to
/// [`s3::Objects::new`](borink_object_storage_proto::s3::Objects::new).
/// This is a `const fn`, so the result can be a `const`, as
/// [`SHA256_RUSTCRYPTO`] is.
///
/// # Compile-time check
///
/// An `S` larger than [`Sha256State::LEN`] bytes, or aligned to more than
/// sixteen, fails to compile:
///
/// ```compile_fail
/// use borink_object_storage_crypto::{Sha256, sha256_provider};
///
/// #[derive(Default)]
/// struct Wide([u64; 32]);
///
/// impl Sha256 for Wide {
///     fn update(&mut self, _: &[u8]) {}
///     fn finish(self) -> [u8; 32] {
///         [0; 32]
///     }
///     fn hmac(_: &[u8], _: &[u8]) -> [u8; 32] {
///         [0; 32]
///     }
/// }
///
/// let too_wide = sha256_provider::<Wide>();
/// ```
pub const fn sha256_provider<S: Sha256>() -> Sha256Provider {
    Sha256Provider::new(
        sha256_start::<S>,
        sha256_update::<S>,
        sha256_finish::<S>,
        S::hmac,
    )
}

// An `S` lives in the state slot for the length of one hash. Each of the
// three functions below evaluates this in an inline `const`, so a provider
// of an `S` that does not fit fails to compile.
const fn assert_sha256_fits<S>() {
    assert!(
        size_of::<S>() <= Sha256State::LEN,
        "a SHA-256 larger than Sha256State::LEN cannot be a provider",
    );
    assert!(
        align_of::<S>() <= 16,
        "a SHA-256 aligned to more than sixteen bytes cannot be a provider",
    );
}

fn sha256_start<S: Sha256>() -> Sha256State {
    const { assert_sha256_fits::<S>() };
    let mut state = Sha256State::uninit();
    // SAFETY: the slot is `Sha256State::LEN` bytes aligned to sixteen, and
    // an `S` fits in both, so this writes a whole `S` inside the slot.
    unsafe { state.as_mut_ptr().cast::<S>().write(S::default()) };
    state
}

fn sha256_update<S: Sha256>(state: &mut Sha256State, bytes: &[u8]) {
    const { assert_sha256_fits::<S>() };
    // SAFETY: a client only ever calls this on a state that
    // `sha256_start::<S>` wrote an `S` into, and nothing between the two
    // reads or writes the bytes, so the slot holds a live `S` that this
    // borrow does not alias.
    let sum = unsafe { &mut *state.as_mut_ptr().cast::<S>() };
    sum.update(bytes);
}

fn sha256_finish<S: Sha256>(mut state: Sha256State) -> [u8; 32] {
    const { assert_sha256_fits::<S>() };
    // SAFETY: as in `sha256_update`, the slot holds a live `S`. The state is
    // taken by value and is dropped here, so the `S` is moved out exactly
    // once and the bytes are never read again.
    let sum = unsafe { state.as_mut_ptr().cast::<S>().read() };
    sum.finish()
}
