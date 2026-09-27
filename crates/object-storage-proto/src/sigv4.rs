//! AWS Signature Version 4, and the SHA-256 and HMAC-SHA256 it is built
//! from.
//!
//! This crate computes neither. An [`s3::Objects`](crate::s3::Objects) client
//! signs every request with the [`Sha256Provider`] you give it. The
//! `borink-object-storage-crypto` crate has two, and a trait that turns any
//! other implementation into one. To build a provider by hand, pass four functions
//! to [`Sha256Provider::new`].
//!
//! A request is signed over its method, its path, the headers this crate
//! writes and the SHA-256 of its content. The signature is written into the
//! `authorization` header. Your HTTP client sends the `host` header from the
//! URL, and the signature covers that value too, so send the URL unchanged.

use core::fmt;
use core::mem::MaybeUninit;

use crate::{Error, Result, Timestamps};

/// The bytes in which a provider keeps a SHA-256 while it is computed.
///
/// The encoder creates one with [`Self::uninit`], passes it to the
/// provider's `start`, then to each `update`, then to `finish`, and drops
/// it. Only the provider reads or writes the bytes.
///
/// The slot is [`Self::LEN`] bytes long and aligned to sixteen bytes. It is
/// on the encoder's stack: nothing here is allocated.
#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub struct Sha256State {
    bytes: [MaybeUninit<u8>; Sha256State::LEN],
}

impl Sha256State {
    /// The number of bytes in the slot.
    ///
    /// A provider's state must fit in it. RustCrypto's SHA-256 takes 104
    /// bytes, and `hmac-sha256`'s takes 112. `borink-object-storage-crypto`
    /// refuses at compile time to build a provider whose state is larger.
    pub const LEN: usize = 128;

    /// Creates a slot whose bytes are not written yet.
    ///
    /// A provider's `start` writes them.
    pub const fn uninit() -> Self {
        Self {
            bytes: [MaybeUninit::uninit(); Self::LEN],
        }
    }

    /// Returns a pointer to the first of the [`Self::LEN`] bytes.
    pub fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr().cast()
    }

    /// Returns a mutable pointer to the first of the [`Self::LEN`] bytes.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr().cast()
    }
}

impl fmt::Debug for Sha256State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sha256State").finish_non_exhaustive()
    }
}

/// An implementation of SHA-256 and HMAC-SHA256, which a client signs with.
///
/// A provider holds four function pointers. The first three compute one
/// SHA-256 a piece at a time, keeping their state in a [`Sha256State`]. The
/// fourth computes one HMAC-SHA256 in one call.
///
/// A provider holds no borrows, so it can be a `const`. The providers of
/// `borink-object-storage-crypto` are.
#[derive(Clone, Copy)]
pub struct Sha256Provider {
    start: fn() -> Sha256State,
    update: fn(&mut Sha256State, &[u8]),
    finish: fn(Sha256State) -> [u8; 32],
    hmac: fn(&[u8], &[u8]) -> [u8; 32],
}

impl Sha256Provider {
    /// Creates a provider from the calls that compute SHA-256 and
    /// HMAC-SHA256.
    ///
    /// For one SHA-256, the encoder calls `start` once, `update` once for
    /// each piece of the input in order, and `finish` once. `hmac` takes the
    /// key first and the message second.
    pub const fn new(
        start: fn() -> Sha256State,
        update: fn(&mut Sha256State, &[u8]),
        finish: fn(Sha256State) -> [u8; 32],
        hmac: fn(key: &[u8], message: &[u8]) -> [u8; 32],
    ) -> Self {
        Self {
            start,
            update,
            finish,
            hmac,
        }
    }

    /// Returns the SHA-256 of `bytes`.
    pub fn hash(&self, bytes: &[u8]) -> [u8; 32] {
        let mut sum = self.start();
        sum.update(bytes);
        sum.finish()
    }

    /// Returns the HMAC-SHA256 of `message` under `key`.
    pub fn hmac(&self, key: &[u8], message: &[u8]) -> [u8; 32] {
        (self.hmac)(key, message)
    }

    pub(crate) fn start(&self) -> Sum {
        Sum {
            provider: *self,
            state: (self.start)(),
        }
    }
}

impl fmt::Debug for Sha256Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sha256Provider").finish_non_exhaustive()
    }
}

// A SHA-256 in progress.
pub(crate) struct Sum {
    provider: Sha256Provider,
    state: Sha256State,
}

impl Sum {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        (self.provider.update)(&mut self.state, bytes);
    }

    pub(crate) fn finish(self) -> [u8; 32] {
        (self.provider.finish)(self.state)
    }
}

/// The most bytes of secret access key that [`Credentials::new`] takes.
///
/// A key that AWS issues is 40 bytes.
pub const MAX_SECRET_LEN: usize = 124;

/// The most bytes of region name that a bucket takes.
pub const MAX_REGION_LEN: usize = 64;

/// The access key that signs requests, and the session token that goes with
/// it.
///
/// This borrows the key. Its [`Debug`](fmt::Debug) output shows the key ID
/// and hides the secret and the token.
#[derive(Clone, Copy)]
pub struct Credentials<'a> {
    key_id: &'a str,
    secret: &'a str,
    session_token: Option<&'a str>,
}

impl<'a> Credentials<'a> {
    /// Creates credentials from an access key ID and its secret access key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCredentials`] if `key_id` is empty, or if it
    /// holds a byte that the `authorization` header cannot carry. Those bytes
    /// are the control characters, a space, `/`, `,` and every byte outside
    /// ASCII. Returns it also if `secret` is empty or longer than
    /// [`MAX_SECRET_LEN`] bytes.
    pub fn new(key_id: &'a str, secret: &'a str) -> Result<Self> {
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
            session_token: None,
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
    pub fn with_session_token(mut self, token: &'a str) -> Result<Self> {
        if !crate::common::valid_header(token.as_bytes()) {
            return Err(Error::InvalidCredentials);
        }
        self.session_token = Some(token);
        Ok(self)
    }

    /// Returns the access key ID.
    pub fn key_id(&self) -> &'a str {
        self.key_id
    }

    pub(crate) fn session_token(&self) -> Option<&'a str> {
        self.session_token
    }

    // The header that carries the session token.
    pub(crate) fn token_header(&self) -> &'static str {
        "x-amz-security-token"
    }
}

impl fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .field("session_token", &self.session_token.map(|_| "<redacted>"))
            .finish()
    }
}

pub(crate) const ALGORITHM: &str = "AWS4-HMAC-SHA256";

// The SHA-256 of no bytes, which a request without content signs.
pub(crate) const EMPTY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

// The key that signs every request of one day, in one region, for one
// service. Deriving it takes four HMACs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SigningKey {
    key: [u8; 32],
    date: [u8; 8],
}

impl SigningKey {
    pub(crate) fn derive(
        credentials: &Credentials<'_>,
        region: &str,
        service: &str,
        now: &Timestamps,
        provider: &Sha256Provider,
    ) -> Self {
        let date = now.date();
        let mut seed = [0u8; 4 + MAX_SECRET_LEN];
        let secret = credentials.secret.as_bytes();
        // `Credentials::new` bounds the secret by MAX_SECRET_LEN.
        seed[..4].copy_from_slice(b"AWS4");
        seed[4..4 + secret.len()].copy_from_slice(secret);
        let mut key = provider.hmac(&seed[..4 + secret.len()], date.as_bytes());
        // Best effort: the secret should not outlive this call on the stack.
        seed.fill(0);
        core::hint::black_box(&seed);
        key = provider.hmac(&key, region.as_bytes());
        key = provider.hmac(&key, service.as_bytes());
        key = provider.hmac(&key, b"aws4_request");
        let mut day = [0; 8];
        day.copy_from_slice(date.as_bytes());
        Self { key, date: day }
    }

    pub(crate) fn covers(&self, now: &Timestamps) -> bool {
        self.date == *now.date().as_bytes()
    }

    // Signs a request whose canonical form hashes to `canonical`, and
    // returns the signature as lowercase hexadecimal.
    pub(crate) fn sign(
        &self,
        canonical: &[u8; 32],
        region: &str,
        service: &str,
        now: &Timestamps,
        provider: &Sha256Provider,
    ) -> [u8; 64] {
        // The string to sign is 122 bytes plus the region and the service. A
        // bucket bounds the region by MAX_REGION_LEN, and a service name is
        // one of this crate's own.
        let mut text = [0u8; 122 + MAX_REGION_LEN + 16];
        let mut at = 0;
        for piece in [
            ALGORITHM.as_bytes(),
            b"\n",
            now.iso8601().as_bytes(),
            b"\n",
            now.date().as_bytes(),
            b"/",
            region.as_bytes(),
            b"/",
            service.as_bytes(),
            b"/aws4_request\n",
            &hex(canonical),
        ] {
            text[at..at + piece.len()].copy_from_slice(piece);
            at += piece.len();
        }
        hex(&provider.hmac(&self.key, &text[..at]))
    }
}

pub(crate) fn hex(bytes: &[u8; 32]) -> [u8; 64] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0; 64];
    for (index, byte) in bytes.iter().enumerate() {
        out[2 * index] = DIGITS[usize::from(byte >> 4)];
        out[2 * index + 1] = DIGITS[usize::from(byte & 0xF)];
    }
    out
}
