//! Signs S3 requests written from a template, with AWS Signature Version 4.
//!
//! `borink_object_storage_proto::s3` writes each request with a hole for its
//! signature and computes no hash. This module computes the hashes, with any
//! [`Sha256`] implementation, and writes the signature in.
//!
//! # Signing one request
//!
//! 1. Derive the day's [`SigningKey`] once a day with [`signing_key`].
//! 2. Build a [`Stamp`] with [`stamp`] for each second you sign at. It holds
//!    the key and the start of the string to sign, which every request
//!    signed in that second shares.
//! 3. Encode the request with the operation's `encode_*`, and pass the
//!    result to [`sign`]. It returns the signed [`WireRequest`], which
//!    reads each part of the request from the buffer.
//!
//! # Signing many requests
//!
//! [`sign_batch`] computes the signatures of many requests, one hash step
//! over all of them before the next. Pass each signature to
//! `borink_object_storage_proto::s3::set_signature`.

use borink_object_storage_proto::s3::{self, STRING_TO_SIGN_PREFIX_LEN, Template, Unsigned};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{Result, Timestamps, WireRequest};

use crate::Sha256;

/// A key of HMAC-SHA256, absorbed: the SHA-256 after its inner padding
/// block and the SHA-256 after its outer one.
///
/// Each HMAC under the key starts from copies of the two, so the padding is
/// hashed once per key rather than once per message.
#[derive(Clone, Copy)]
pub struct HmacKey<S> {
    inner: S,
    outer: S,
}

impl<S> core::fmt::Debug for HmacKey<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("HmacKey(<redacted>)")
    }
}

impl<S: Sha256> HmacKey<S> {
    /// Absorbs `key`. A key longer than a block is hashed first, as RFC
    /// 2104 says.
    pub fn new(key: &[u8]) -> Self {
        let mut pad = [0u8; 64];
        if key.len() > 64 {
            pad[..32].copy_from_slice(&sha256::<S>(key));
        } else {
            pad[..key.len()].copy_from_slice(key);
        }
        pad.iter_mut().for_each(|byte| *byte ^= 0x36);
        let mut inner = S::default();
        inner.update(&pad);
        pad.iter_mut().for_each(|byte| *byte ^= 0x36 ^ 0x5c);
        let mut outer = S::default();
        outer.update(&pad);
        crate::wipe(&mut pad);
        Self { inner, outer }
    }

    /// Returns the HMAC-SHA256 of `message` under this key.
    pub fn mac(&self, message: &[u8]) -> [u8; 32] {
        let mut inner = self.inner;
        inner.update(message);
        let mut outer = self.outer;
        outer.update(&inner.finish());
        outer.finish()
    }
}

/// Returns the SHA-256 of `bytes`.
pub fn sha256<S: Sha256>(bytes: &[u8]) -> [u8; 32] {
    let mut sum = S::default();
    sum.update(bytes);
    sum.finish()
}

/// Returns the HMAC-SHA256 of `message` under `key`.
pub fn hmac<S: Sha256>(key: &[u8], message: &[u8]) -> [u8; 32] {
    HmacKey::<S>::new(key).mac(message)
}

/// The key that signs every request of one day, in one region, for one
/// service.
pub type SigningKey<S> = HmacKey<S>;

/// Derives the key that signs every request from `template` on the day of
/// `now`, from the secret of `credentials`.
///
/// It takes four HMACs. Derive it once a day and keep it.
///
/// # Errors
///
/// Returns what [`s3::signing_key`] returns.
pub fn signing_key<S: Sha256>(
    template: &Template,
    bytes: &[u8],
    credentials: &Credentials<'_>,
    now: &Timestamps,
) -> Result<SigningKey<S>> {
    let mut key = s3::signing_key(template, bytes, credentials, now, hmac::<S>)?;
    let hmac_key = HmacKey::new(&key);
    crate::wipe(&mut key);
    Ok(hmac_key)
}

/// The HMAC of a signing key, after the start of the string to sign of one
/// second.
///
/// Build one with [`stamp`] for each second you sign at, and sign every
/// request of that second from it.
#[derive(Clone, Copy)]
pub struct Stamp<S> {
    inner: S,
    outer: S,
}

impl<S> core::fmt::Debug for Stamp<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Stamp(<redacted>)")
    }
}

impl<S: Sha256> Stamp<S> {
    /// Returns the signature of a request whose canonical request hashes to
    /// `canonical`, before it is written in hexadecimal.
    pub fn sign(&self, canonical: &[u8; 32]) -> [u8; 32] {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut hex = [0u8; 64];
        for (pair, byte) in hex.chunks_exact_mut(2).zip(canonical) {
            pair[0] = DIGITS[usize::from(byte >> 4)];
            pair[1] = DIGITS[usize::from(byte & 15)];
        }
        let mut inner = self.inner;
        inner.update(&hex);
        let mut outer = self.outer;
        outer.update(&inner.finish());
        outer.finish()
    }
}

/// Builds the stamp of `key` for requests from `template` signed at `now`.
///
/// # Errors
///
/// Returns what [`s3::string_to_sign_prefix`] returns.
pub fn stamp<S: Sha256>(
    key: &SigningKey<S>,
    template: &Template,
    bytes: &[u8],
    now: &Timestamps,
) -> Result<Stamp<S>> {
    let mut prefix = [0u8; STRING_TO_SIGN_PREFIX_LEN];
    let prefix = s3::string_to_sign_prefix(template, bytes, now, &mut prefix)?;
    let mut inner = key.inner;
    inner.update(prefix);
    Ok(Stamp {
        inner,
        outer: key.outer,
    })
}

/// Signs one request and returns where it is in `buf`, ready to send.
///
/// `buf` is the buffer that `unsigned` was written into. A request with a
/// body that the crate wrote signs the SHA-256 of that body, which this
/// function computes first. Write in a checksum of the body with
/// `borink_object_storage_proto::s3::set_body_checksum` before you call it.
/// Another buffer gives a wrong request, but never a panic.
pub fn sign<S: Sha256>(stamp: &Stamp<S>, unsigned: &Unsigned, buf: &mut [u8]) -> WireRequest {
    hash_body::<S>(unsigned, buf);
    let mac = stamp.sign(&sha256::<S>(unsigned.canonical(buf)));
    s3::set_signature(unsigned, buf, &mac)
}

/// Writes the SHA-256 of the body of `unsigned` into `buf`, where the request
/// signs it. Does nothing for a request that signs no body of its own.
///
/// [`sign`] calls this. Call it yourself before [`sign_batch`].
pub fn hash_body<S: Sha256>(unsigned: &Unsigned, buf: &mut [u8]) {
    if unsigned.signs_body()
        && let Some(body) = unsigned.body(buf)
    {
        let hash = sha256::<S>(body);
        s3::set_body_hash(unsigned, buf, &hash);
    }
}

/// Computes the signatures of a batch of requests: the SHA-256 of every
/// canonical request, then the HMAC of every string to sign.
///
/// `macs` receives one signature for each canonical request. Pass each to
/// `borink_object_storage_proto::s3::set_signature` with its request.
pub fn sign_batch<S: Sha256>(stamp: &Stamp<S>, canonicals: &[&[u8]], macs: &mut [[u8; 32]]) {
    for (canonical, mac) in canonicals.iter().zip(macs.iter_mut()) {
        *mac = sha256::<S>(canonical);
    }
    for mac in macs.iter_mut().take(canonicals.len()) {
        *mac = stamp.sign(mac);
    }
}
