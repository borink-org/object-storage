//! The credentials that sign S3 requests with AWS Signature Version 4.
//!
//! This crate computes no SHA-256. A request from an S3 template leaves room
//! for its signature. The `sigv4` module of the `borink-object-storage-crypto`
//! crate computes it and writes it in. To sign with a SHA-256 of your own,
//! call [`s3::signing_key`](crate::s3::signing_key),
//! [`s3::string_to_sign_prefix`](crate::s3::string_to_sign_prefix) and
//! [`s3::set_signature`](crate::s3::set_signature).
//!
//! A request is signed over its method, its path, the headers this crate
//! writes and the SHA-256 of its content. The signature is written into the
//! `authorization` header. Your HTTP client sends the `host` header from the
//! URL, and the signature covers that value too, so send the URL unchanged.

use core::fmt;

use crate::{Error, Result};

/// The most bytes of secret access key that [`Credentials::new`] takes.
///
/// A key that AWS issues is 40 bytes.
pub const MAX_SECRET_LEN: usize = 124;

/// The most bytes of region name that a bucket takes.
pub const MAX_REGION_LEN: usize = 64;

/// The access key that signs requests, the session token that goes with it,
/// and the function that wipes a copy of the secret.
///
/// This borrows the key. Its [`Debug`](fmt::Debug) output shows the key ID
/// and hides the secret and the token.
#[derive(Clone, Copy)]
pub struct Credentials<'a> {
    key_id: &'a str,
    secret: &'a str,
    token: Token<'a>,
    wipe: fn(&mut [u8]),
}

// The token that a set of credentials carries, which decides the header
// that sends it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Token<'a> {
    // Long-lived credentials, with no token.
    None,
    // Temporary credentials, such as STS hands out, sent as
    // `x-amz-security-token`.
    Temporary(&'a str),
    // The credentials of an S3 Express session, sent as
    // `x-amz-s3session-token`.
    S3Session(&'a str),
}

impl<'a> Token<'a> {
    // The header that carries the token, and the token.
    pub(crate) fn header(self) -> Option<(&'static str, &'a str)> {
        match self {
            Self::None => None,
            Self::Temporary(token) => Some(("x-amz-security-token", token)),
            Self::S3Session(token) => Some(("x-amz-s3session-token", token)),
        }
    }
}

impl<'a> Credentials<'a> {
    /// Creates credentials from an access key ID, its secret access key, and
    /// the function that wipes a copy of the secret.
    ///
    /// A client copies the secret onto its stack to derive a signing key,
    /// and calls `wipe` on that copy. `wipe` must set every byte to zero in
    /// writes that the compiler keeps. This crate forbids `unsafe` code, so
    /// it cannot guarantee that itself. Pass
    /// `borink_object_storage_crypto::wipe`, or [`wipe_best_effort`] to
    /// accept writes that the compiler may remove.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCredentials`] if `key_id` is empty, or if it
    /// holds a byte that the `authorization` header cannot carry. Those bytes
    /// are the control characters, a space, `/`, `,` and every byte outside
    /// ASCII. Returns it also if `secret` is empty or longer than
    /// [`MAX_SECRET_LEN`] bytes.
    pub fn new(key_id: &'a str, secret: &'a str, wipe: fn(&mut [u8])) -> Result<Self> {
        let key_id_is_valid = !key_id.is_empty()
            && key_id
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'/' | b','));
        if !key_id_is_valid || secret.is_empty() || secret.len() > MAX_SECRET_LEN {
            return Err(Error::InvalidCredentials);
        }
        Ok(Self {
            key_id,
            secret,
            token: Token::None,
            wipe,
        })
    }

    /// Returns these credentials with the session token of temporary
    /// credentials.
    ///
    /// Each request then carries the token in `x-amz-security-token`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCredentials`] if `token` is not usable as one
    /// HTTP header value.
    pub fn with_session_token(self, token: &'a str) -> Result<Self> {
        self.with_token(Token::Temporary(token), token)
    }

    // These credentials, with the token of an S3 Express session.
    pub(crate) fn with_s3_session_token(self, token: &'a str) -> Result<Self> {
        self.with_token(Token::S3Session(token), token)
    }

    fn with_token(mut self, kind: Token<'a>, token: &str) -> Result<Self> {
        if !crate::common::valid_header(token.as_bytes()) {
            return Err(Error::InvalidCredentials);
        }
        self.token = kind;
        Ok(self)
    }

    /// Returns the access key ID.
    pub fn key_id(&self) -> &'a str {
        self.key_id
    }

    pub(crate) fn token(&self) -> Token<'a> {
        self.token
    }

    pub(crate) fn wipe(&self) -> fn(&mut [u8]) {
        self.wipe
    }

    pub(crate) fn secret(&self) -> &'a str {
        self.secret
    }
}

/// Sets every byte of `bytes` to zero, on a best-effort basis.
///
/// [`core::hint::black_box`] keeps the compiler from removing the writes in
/// practice, but Rust does not guarantee it. Pass this to
/// [`Credentials::new`] only if you accept that. The
/// `borink-object-storage-crypto` crate's `wipe` writes with volatile
/// writes, which the compiler keeps.
pub fn wipe_best_effort(bytes: &mut [u8]) {
    bytes.fill(0);
    core::hint::black_box(bytes);
}

impl fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .field(
                "token",
                &self
                    .token
                    .header()
                    .map(|(header, _)| (header, "<redacted>")),
            )
            .finish()
    }
}

pub(crate) const ALGORITHM: &str = "AWS4-HMAC-SHA256";

// The SHA-256 of no bytes, which a request without content signs.
pub(crate) const EMPTY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
