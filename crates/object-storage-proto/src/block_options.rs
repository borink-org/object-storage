//! Azure block options, validated before any request storage is written.

use crate::request::HeadWriter;
use crate::{InvalidPlan, Result};

/// A supported Azure block-operation option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum BlockOptionKind {
    /// The `x-ms-lease-id` header.
    LeaseId = 1,
    /// The `Content-MD5` header.
    ContentMd5 = 2,
    /// The `x-ms-content-crc64` header.
    ContentCrc64 = 3,
    /// The `x-ms-encryption-key` header.
    EncryptionKey = 4,
    /// The `x-ms-encryption-key-sha256` header.
    EncryptionKeySha256 = 5,
    /// The `x-ms-encryption-algorithm` header.
    EncryptionAlgorithm = 6,
    /// The `x-ms-encryption-scope` header.
    EncryptionScope = 7,
    /// The `x-ms-client-request-id` header.
    ClientRequestId = 8,
    /// The `x-ms-blob-content-type` header.
    ContentType = 9,
    /// The `x-ms-blob-content-encoding` header.
    ContentEncoding = 10,
    /// The `x-ms-blob-content-language` header.
    ContentLanguage = 11,
    /// The `x-ms-blob-cache-control` header.
    CacheControl = 12,
    /// The `x-ms-blob-content-md5` header.
    BlobContentMd5 = 13,
    /// The `x-ms-blob-content-disposition` header.
    ContentDisposition = 14,
    /// The `x-ms-tags` header.
    Tags = 15,
    /// The `x-ms-access-tier` header.
    AccessTier = 16,
    /// The `If-Modified-Since` header.
    IfModifiedSince = 17,
    /// The `If-Unmodified-Since` header.
    IfUnmodifiedSince = 18,
    /// The `x-ms-if-tags` header.
    IfTags = 19,
    /// The `x-ms-immutability-policy-until-date` header.
    ImmutabilityUntil = 20,
    /// The `x-ms-immutability-policy-mode` header.
    ImmutabilityMode = 21,
    /// The `x-ms-legal-hold` header.
    LegalHold = 22,
    /// The `x-ms-expiry-option` header.
    ExpiryOption = 23,
    /// The `x-ms-expiry-time` header.
    ExpiryTime = 24,
    /// The `x-ms-meta-` header prefix.
    Metadata = 25,
    /// HNS encryption context.
    EncryptionContext = 26,
    /// Server-side timeout in seconds, encoded in the query.
    Timeout = 27,
}

impl BlockOptionKind {
    /// Decodes a checked binding discriminant.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::LeaseId),
            2 => Some(Self::ContentMd5),
            3 => Some(Self::ContentCrc64),
            4 => Some(Self::EncryptionKey),
            5 => Some(Self::EncryptionKeySha256),
            6 => Some(Self::EncryptionAlgorithm),
            7 => Some(Self::EncryptionScope),
            8 => Some(Self::ClientRequestId),
            9 => Some(Self::ContentType),
            10 => Some(Self::ContentEncoding),
            11 => Some(Self::ContentLanguage),
            12 => Some(Self::CacheControl),
            13 => Some(Self::BlobContentMd5),
            14 => Some(Self::ContentDisposition),
            15 => Some(Self::Tags),
            16 => Some(Self::AccessTier),
            17 => Some(Self::IfModifiedSince),
            18 => Some(Self::IfUnmodifiedSince),
            19 => Some(Self::IfTags),
            20 => Some(Self::ImmutabilityUntil),
            21 => Some(Self::ImmutabilityMode),
            22 => Some(Self::LegalHold),
            23 => Some(Self::ExpiryOption),
            24 => Some(Self::ExpiryTime),
            25 => Some(Self::Metadata),
            26 => Some(Self::EncryptionContext),
            27 => Some(Self::Timeout),
            _ => None,
        }
    }
    fn header(self) -> &'static str {
        match self {
            Self::LeaseId => "x-ms-lease-id",
            Self::ContentMd5 => "Content-MD5",
            Self::ContentCrc64 => "x-ms-content-crc64",
            Self::EncryptionKey => "x-ms-encryption-key",
            Self::EncryptionKeySha256 => "x-ms-encryption-key-sha256",
            Self::EncryptionAlgorithm => "x-ms-encryption-algorithm",
            Self::EncryptionScope => "x-ms-encryption-scope",
            Self::ClientRequestId => "x-ms-client-request-id",
            Self::ContentType => "x-ms-blob-content-type",
            Self::ContentEncoding => "x-ms-blob-content-encoding",
            Self::ContentLanguage => "x-ms-blob-content-language",
            Self::CacheControl => "x-ms-blob-cache-control",
            Self::BlobContentMd5 => "x-ms-blob-content-md5",
            Self::ContentDisposition => "x-ms-blob-content-disposition",
            Self::Tags => "x-ms-tags",
            Self::AccessTier => "x-ms-access-tier",
            Self::IfModifiedSince => "If-Modified-Since",
            Self::IfUnmodifiedSince => "If-Unmodified-Since",
            Self::IfTags => "x-ms-if-tags",
            Self::ImmutabilityUntil => "x-ms-immutability-policy-until-date",
            Self::ImmutabilityMode => "x-ms-immutability-policy-mode",
            Self::LegalHold => "x-ms-legal-hold",
            Self::ExpiryOption => "x-ms-expiry-option",
            Self::ExpiryTime => "x-ms-expiry-time",
            Self::Metadata => "x-ms-meta-",
            Self::EncryptionContext => "x-ms-encryption-context",
            Self::Timeout => "timeout",
        }
    }
    fn operations(self) -> u8 {
        match self {
            Self::LeaseId => 7,
            Self::ContentMd5 => 3,
            Self::ContentCrc64 => 3,
            Self::EncryptionKey => 3,
            Self::EncryptionKeySha256 => 3,
            Self::EncryptionAlgorithm => 3,
            Self::EncryptionScope => 3,
            Self::ClientRequestId => 7,
            Self::ContentType => 2,
            Self::ContentEncoding => 2,
            Self::ContentLanguage => 2,
            Self::CacheControl => 2,
            Self::BlobContentMd5 => 2,
            Self::ContentDisposition => 2,
            Self::Tags => 2,
            Self::AccessTier => 2,
            Self::IfModifiedSince => 2,
            Self::IfUnmodifiedSince => 2,
            Self::IfTags => 6,
            Self::ImmutabilityUntil => 2,
            Self::ImmutabilityMode => 2,
            Self::LegalHold => 2,
            Self::ExpiryOption => 2,
            Self::ExpiryTime => 2,
            Self::Metadata => 2,
            Self::EncryptionContext => 2,
            Self::Timeout => 7,
        }
    }
}

/// One borrowed native option. Values use the provider's wire representation.
#[derive(Debug, Clone, Copy)]
pub struct BlockOption<'a> {
    /// Supported option, not an arbitrary HTTP header.
    pub kind: BlockOptionKind,
    /// Metadata key, empty for every other kind.
    pub name: &'a str,
    /// Header value; tags are URL-encoded and hashes are base64.
    pub value: &'a str,
}

impl<'a> BlockOption<'a> {
    /// A non-metadata option.
    pub const fn new(kind: BlockOptionKind, value: &'a str) -> Self {
        Self {
            kind,
            name: "",
            value,
        }
    }
    /// One metadata key/value pair.
    pub const fn metadata(name: &'a str, value: &'a str) -> Self {
        Self {
            kind: BlockOptionKind::Metadata,
            name,
            value,
        }
    }
}

pub(crate) fn validate<'a>(
    options: impl Iterator<Item = BlockOption<'a>> + Clone,
    operation: u8,
) -> Result<()> {
    let mut seen = 0u32;
    for (index, option) in options.clone().enumerate() {
        if option.kind.operations() & operation == 0
            || !option.value.is_ascii()
            || option.value.bytes().any(|b| b.is_ascii_control())
        {
            return Err(InvalidPlan::Option.into());
        }
        if option.kind == BlockOptionKind::EncryptionContext && option.value.len() > 1024 {
            return Err(InvalidPlan::Option.into());
        }
        if option.kind == BlockOptionKind::Timeout
            && !crate::azure::decimal(option.value.as_bytes())
                .is_some_and(|seconds| seconds > 0 && seconds <= u32::MAX as u64)
        {
            return Err(InvalidPlan::Option.into());
        }
        // BlockOptionKind discriminants are 1..=27, so shifts are 0..=26.
        let bit = 1u32 << (option.kind as u16 - 1);
        if option.kind == BlockOptionKind::Metadata {
            let mut bytes = option.name.bytes();
            if !bytes
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || options.clone().take(index).any(|other| {
                    other.kind == option.kind && other.name.eq_ignore_ascii_case(option.name)
                })
            {
                return Err(InvalidPlan::Option.into());
            }
        } else if !option.name.is_empty() || seen & bit != 0 {
            return Err(InvalidPlan::Option.into());
        }
        seen |= bit;
    }
    // The same 1..=27 discriminant bound applies to these membership probes.
    let has = |kind: BlockOptionKind| seen & (1 << (kind as u16 - 1)) != 0;
    if has(BlockOptionKind::ContentMd5) && has(BlockOptionKind::ContentCrc64) {
        return Err(InvalidPlan::Option.into());
    }
    let encryption = [
        BlockOptionKind::EncryptionKey,
        BlockOptionKind::EncryptionKeySha256,
        BlockOptionKind::EncryptionAlgorithm,
    ]
    .into_iter()
    .filter(|kind| has(*kind))
    .count();
    if (encryption != 0 && encryption != 3)
        || (encryption != 0 && has(BlockOptionKind::EncryptionScope))
    {
        return Err(InvalidPlan::Option.into());
    }
    Ok(())
}

pub(crate) fn write<'a>(head: &mut HeadWriter<'_>, options: impl Iterator<Item = BlockOption<'a>>) {
    for option in options {
        if option.kind == BlockOptionKind::Timeout {
            continue;
        }
        head.header_parts(
            |out| {
                out.push(option.kind.header().as_bytes());
                out.push(option.name.as_bytes());
            },
            |out| out.push(option.value.as_bytes()),
        );
    }
}

pub(crate) fn timeout<'a>(mut options: impl Iterator<Item = BlockOption<'a>>) -> Option<u32> {
    options
        .find(|option| option.kind == BlockOptionKind::Timeout)
        .and_then(|option| crate::azure::decimal(option.value.as_bytes()))
        .map(|seconds| seconds as u32)
}
