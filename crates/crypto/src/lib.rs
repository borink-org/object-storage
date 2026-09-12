//! Checksum implementations for [`borink-object-storage-proto`], and the
//! trait that turns one into a provider that crate can call.
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
//! let blobs = Blobs::new(container, "access-token")?.with_checksum(borink_crypto::CRC64);
//! # }
//! # Ok::<(), borink_object_storage_proto::Error>(())
//! ```
//!
//! [`CRC64`] and [`MD5`] are the providers of this crate. Each is behind a
//! feature, and neither is on by default, so you link the one you register
//! and nothing else. The `crc64` feature builds [`Crc64`], this crate's own
//! CRC-64/NVME. The `md5` feature builds [`Md5`], an adapter for RustCrypto's
//! `md-5`.
//!
//! To register an implementation of your own, implement [`Checksum`] for it
//! and pass the type to [`provider`].
//!
//! Neither checksum is cryptography. Both detect corruption in transit and
//! nothing else.
//!
//! [`borink-object-storage-proto`]: borink_object_storage_proto

#![no_std]

use borink_object_storage_proto::checksum::{
    ChecksumKind, ChecksumProvider, ChecksumState, Digest,
};

#[cfg(feature = "crc64")]
mod crc64;
#[cfg(feature = "md5")]
mod md5;

#[cfg(feature = "crc64")]
pub use crc64::Crc64;
#[cfg(feature = "md5")]
pub use md5::Md5;

/// [`Crc64`] as a provider.
#[cfg(feature = "crc64")]
pub const CRC64: ChecksumProvider = provider::<Crc64>();

/// [`Md5`] as a provider.
#[cfg(feature = "md5")]
pub const MD5: ChecksumProvider = provider::<Md5>();

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
/// [`MD5`] are.
///
/// # Compile-time check
///
/// A `C` larger than [`ChecksumState::LEN`] bytes, or aligned to more than
/// sixteen, fails to compile:
///
/// ```compile_fail
/// use borink_crypto::{Checksum, provider};
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
