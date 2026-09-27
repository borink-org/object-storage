//! S3 requests and responses, signed with AWS Signature Version 4.
//!
//! # How a request works
//!
//! 1. Create a [`Bucket`] and [`Credentials`], and from them an [`Objects`]
//!    client with a [`Sha256Provider`]. The `borink-object-storage-crypto`
//!    crate has providers, and the function that [`Credentials::new`] takes
//!    to wipe its copy of the secret.
//! 2. Describe the operation with a [`PhysicalGet`], a [`PhysicalPut`], a
//!    [`PhysicalDelete`] or a [`PhysicalList`], the same plans that an Azure
//!    client takes.
//! 3. Call [`Objects::encode_get`], [`Objects::encode_put`],
//!    [`Objects::encode_delete`] or [`Objects::encode_list`] to write the
//!    signed request head into your buffer, and send the [`WireRequest`]
//!    with your HTTP client.
//! 4. Put the response headers into a [`ResponseHead`] and call the
//!    `accept_*_head` method of the same operation.
//!
//! S3 names an error in the response body, not in a header. A failure of a
//! GET, a PUT, a DELETE or a listing is therefore a `NeedErrorBody` outcome.
//! Read the body and pass it to the `accept_*_error_body` method of the same
//! operation. A HEAD response has no body, and its outcome is final.
//!
//! # Example
//!
//! ```
//! use borink_object_storage_proto::s3::{Bucket, Service, Objects, PayloadHash};
//! use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
//! use borink_object_storage_proto::{
//!     GetHeadOutcome, HeaderSpan, Payload, PhysicalGet, PhysicalPut, PutHeadOutcome,
//!     ResponseHead, Timestamps, layered,
//! };
//! # use borink_object_storage_proto::sigv4::{Sha256Provider, Sha256State};
//! # const SHA256: Sha256Provider =
//! #     Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32], |_, _| [0; 32]);
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bucket = Bucket::new(
//!     "https://s3.eu-west-1.amazonaws.com", "objects", "eu-west-1", Service::Aws,
//! )?;
//! // Pass `borink_object_storage_crypto::wipe` in place of `wipe_best_effort`.
//! let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe_best_effort)?;
//! let now = Timestamps::from_unix(1_787_400_000);
//! // SHA256 is a provider, such as `borink_object_storage_crypto::SHA256_RUSTCRYPTO`.
//! let objects = Objects::new(bucket, credentials, SHA256).with_signing_key(&now);
//!
//! let put = PhysicalPut::new("directory/object.txt");
//! let content = Payload::Slice(b"contents");
//! let size = layered::s3::put_requirements(&objects, &put, content, PayloadHash::Compute, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request = objects.encode_put(
//!     &mut buffer, &mut headers, &put, content, PayloadHash::Compute, &now,
//! )?;
//! assert_eq!(
//!     request.url(),
//!     "https://s3.eu-west-1.amazonaws.com/objects/directory/object.txt"
//! );
//!
//! let head = ResponseHead::from_headers(200, [("ETag", b"\"tag\"".as_slice())]);
//! assert!(matches!(
//!     objects.accept_put_head(put.shape(), head)?,
//!     PutHeadOutcome::Created { .. }
//! ));
//!
//! let get = PhysicalGet::new("directory/object.txt");
//! let size = layered::s3::get_requirements(&objects, &get, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request = objects.encode_get(&mut buffer, &mut headers, &get, &now)?;
//! # let _ = request;
//! let head = ResponseHead::from_headers(404, [("x-amz-request-id", b"4442587FB7D0A2F9".as_slice())]);
//! let GetHeadOutcome::NeedErrorBody(failure) = objects.accept_get_head(get.shape(), head)? else {
//!     panic!("S3 names the error in the body");
//! };
//! let body = b"<Error><Code>NoSuchKey</Code></Error>";
//! assert!(matches!(
//!     objects.accept_error_body(failure.status, failure.request_id, body),
//!     GetHeadOutcome::NotFound { .. }
//! ));
//! # Ok(())
//! # }
//! ```
//!
//! # Which service answers
//!
//! A [`Bucket`] names the [`Service`] that answers its requests. A client of
//! [`Service::Aws`] holds requests and responses to the rules that AWS
//! documents for S3. A client of [`Service::Compatible`] refuses less, and
//! accepts every response that a service implementing the S3 API is known to
//! send.
//!
//! # Signing
//!
//! Every request is signed for the bucket's region. Signing takes five
//! HMACs, and four of them depend only on the day. Call
//! [`Objects::with_signing_key`] to compute those four once. A client whose
//! key is for another day computes them again for each request.
//!
//! The signature covers the `host` header, which your HTTP client writes from
//! the URL. Send the URL as the request holds it. An endpoint that names the
//! default port of its scheme is written without it, as HTTP clients send
//! it.
//!
//! # Metadata
//!
//! Put the pairs of a write in [`PhysicalPut::metadata`]. A header value
//! holds only ASCII without control characters. A value with any other text
//! is sent as an RFC 2047 encoded word, which S3 decodes. S3 returns
//! such a value encoded as well: pass each response header to
//! [`metadata_name`], and its value to [`metadata_value`].
//!
//! A value that a read would not return exactly is refused with
//! [`InvalidPlan::MetadataValue`].
//! [`MetadataPair::value`](crate::MetadataPair::value) lists those values.
//!
//! # Listing
//!
//! A listing is one page at a time, as on Azure: encode a [`PhysicalList`]
//! with [`Objects::encode_list`], read the body of a
//! [`ListHeadOutcome::Page`] whole, and pass it to
//! [`Objects::fill_listing`]. Pass the page's
//! [`Listing::next_marker`] as the marker of the next plan.
//!
//! ```
//! # use borink_object_storage_proto::s3::{Bucket, Service, Objects};
//! # use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
//! # use borink_object_storage_proto::sigv4::{Sha256Provider, Sha256State};
//! # const SHA256: Sha256Provider =
//! #     Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32], |_, _| [0; 32]);
//! use borink_object_storage_proto::{
//!     EntryKind, HeaderSpan, ListEntry, PhysicalList, Timestamps, layered,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let bucket = Bucket::new(
//! #     "https://s3.eu-west-1.amazonaws.com", "objects", "eu-west-1", Service::Aws,
//! # )?;
//! # let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe_best_effort)?;
//! # let now = Timestamps::from_unix(1_787_400_000);
//! # let objects = Objects::new(bucket, credentials, SHA256);
//! let list = PhysicalList {
//!     delimited: true,
//!     ..PhysicalList::new("photos/")
//! };
//! let size = layered::s3::list_requirements(&objects, &list, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request = objects.encode_list(&mut buffer, &mut headers, &list, &now)?;
//! assert_eq!(
//!     request.url(),
//!     "https://s3.eu-west-1.amazonaws.com/objects\
//!      ?delimiter=%2F&encoding-type=url&list-type=2&prefix=photos%2F"
//! );
//!
//! let mut body = Vec::from(
//!     b"<ListBucketResult><EncodingType>url</EncodingType>\
//!       <IsTruncated>false</IsTruncated>\
//!       <Contents><Key>photos/a+b.jpg</Key><Size>8</Size></Contents>\
//!       <CommonPrefixes><Prefix>photos/2026/</Prefix></CommonPrefixes>\
//!       </ListBucketResult>"
//!         .as_slice(),
//! );
//! let mut entries = vec![ListEntry::default(); 1000];
//! let page = objects.fill_listing(&mut body, &mut entries)?;
//! assert_eq!(page.filled, 2);
//! assert_eq!(entries[0].key, "photos/a b.jpg");
//! assert_eq!(entries[1].kind, EntryKind::Prefix);
//! assert_eq!(page.next_marker, None);
//! # Ok(())
//! # }
//! ```
//!
//! The request asks S3 to URL-encode the keys of the page, so that a key
//! with a character that XML cannot carry still arrives, and
//! [`Objects::fill_listing`] decodes them.
//!
//! # Content that is not signed
//!
//! [`PayloadHash::Unsigned`] sends a write without the SHA-256 of its
//! content, so this crate never reads the content. The signature then does
//! not cover the content. TLS still protects the bytes in transit, but
//! nothing checks the bytes that you handed to your HTTP client. Send an MD5
//! of the content in [`WriteOptions::checksum`] to have S3 check them.

use core::cmp::Ordering;

#[cfg(doc)]
use crate::WriteOptions;
use crate::checksum::{ChecksumKind, ChecksumProvider, KINDS};
use crate::common::{
    ContentRange, accept_success, condition_header, decimal_header, encoded, failure,
    parse_content_range, text_header, validate_condition, write_range,
};
use crate::encoding::{self, rfc2047};
use crate::query::{self, Parameter, QueryValue};
use crate::request::{HeadWriter, U64Decimal};
use crate::sigv4::{self, Credentials, EMPTY_SHA256, MAX_REGION_LEN, Sha256Provider, SigningKey};
use crate::{
    Classification, ConditionKind, DeleteHeadOutcome, DeleteKind, DeleteShape, Error,
    GetHeadOutcome, GetKind, GetShape, HeaderSpan, InvalidPlan, ListEntry, ListHeadOutcome,
    ListInclude, Listing, MetadataPair, Method, ObjectMeta, Payload, PhysicalDelete, PhysicalGet,
    PhysicalList, PhysicalPut, PutHeadOutcome, PutShape, RequestedRange, ResponseFault,
    ResponseHead, Result, ServiceErrorKind, Timestamps, TransactionalChecksum, WireRequest,
};

// What `x-amz-content-sha256` carries for content that is not signed.
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// The most bytes that an S3 object key holds.
///
/// Every encoding method refuses a longer key with
/// [`InvalidPlan::KeyTooLong`]. S3 counts the bytes of the key's UTF-8.
pub const MAX_KEY_LEN: usize = 1024;

/// The most bytes that S3 writes in one `PutObject` request.
///
/// [`Objects::encode_put`] refuses a longer payload with
/// [`InvalidPlan::PayloadTooLarge`].
pub const MAX_PUT_LEN: u64 = 5 * 1024 * 1024 * 1024;

/// The prefix of the header that carries one metadata pair.
pub const METADATA_PREFIX: &str = "x-amz-meta-";

/// Returns the metadata name that a response header carries, or [`None`]
/// for a header that carries no pair.
///
/// S3 stores a metadata name in lowercase, and returns it so.
pub fn metadata_name(header: &str) -> Option<&str> {
    let (prefix, name) = header.split_at_checked(METADATA_PREFIX.len())?;
    (prefix.eq_ignore_ascii_case(METADATA_PREFIX) && !name.is_empty()).then_some(name)
}

/// Returns the text of a metadata value that a response header carries.
///
/// A write sends a value outside ASCII, or one with a control character, as
/// an RFC 2047 encoded word, such as `=?UTF-8?B?Y2Fmw6k=?=` for `café`. S3
/// returns it in that form. This
/// function decodes a value of one or more UTF-8 encoded words. It returns
/// any other value unchanged, including an encoded word that it cannot
/// decode.
///
/// Copies the text into `into` and returns what it wrote, which is never
/// longer than `value`. Returns [`None`] if `into` is shorter than
/// `value`.
pub fn metadata_value<'a>(value: &[u8], into: &'a mut [u8]) -> Option<&'a [u8]> {
    let into = into.get_mut(..value.len())?;
    match rfc2047::decode(value, into) {
        Some(len) => Some(&into[..len]),
        None => {
            into.copy_from_slice(value);
            Some(into)
        }
    }
}

/// Where the bucket name goes in the URL of a request.
///
/// The signature covers the path and the host, so a service that expects the
/// other form refuses the request with 403 `SignatureDoesNotMatch`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u16)]
pub enum Addressing {
    /// The bucket is the first segment of the path:
    /// `https://s3.example.com/bucket/key`.
    #[default]
    Path = 1,
    /// The bucket is the first label of the host:
    /// `https://bucket.s3.example.com/key`.
    ///
    /// The bucket name must then be usable as a DNS label.
    VirtualHosted = 2,
}

impl Addressing {
    /// Returns the addressing with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Path,
            2 => Self::VirtualHosted,
            _ => return None,
        })
    }
}

/// The service that answers a client's requests.
///
/// Services that implement the S3 API differ in what they take and in what
/// they answer. A client holds its requests and responses to the rules of the
/// service that its bucket names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum Service {
    /// Amazon S3, with a general purpose bucket.
    ///
    /// The client refuses a plan that AWS refuses, and a response that AWS
    /// does not send:
    ///
    /// - A bucket name has 3 to 63 lowercase letters, digits, `.` and `-`.
    ///   It starts and ends with a letter or a digit, and holds no `..`. It is
    ///   not written as an IPv4 address, and has no prefix or suffix that AWS
    ///   reserves, such as `xn--` or `--x-s3`.
    /// - A region name holds only lowercase letters, digits and `-`.
    /// - The metadata of a write holds at most [`MAX_METADATA_LEN`] bytes.
    /// - A write conditional on `If-None-Match` carries `*`.
    /// - A removal is conditional on `If-Match` alone.
    /// - A successful GET or HEAD states `Content-Length`.
    /// - A removal succeeds with status 204.
    Aws = 1,
    /// Any service that implements the S3 API.
    ///
    /// The client refuses only what no such service takes, and accepts every
    /// response that one is known to send:
    ///
    /// - A bucket name holds ASCII letters, digits, `-`, `.`, `_` and `~`.
    /// - A region name holds ASCII letters, digits, `-`, `.` and `_`.
    /// - A condition is sent as the plan gives it, and the metadata of a write
    ///   may be of any size.
    /// - A successful GET or HEAD may leave out `Content-Length`.
    /// - A removal succeeds with status 200 or 204.
    Compatible = 2,
}

impl Service {
    /// Returns the service with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Aws,
            2 => Self::Compatible,
            _ => return None,
        })
    }
}

/// The most bytes of metadata that a write takes for [`Service::Aws`].
///
/// AWS counts the UTF-8 bytes of each name and each value, without the
/// `x-amz-meta-` prefix, and documents 2 KB.
pub const MAX_METADATA_LEN: usize = 2048;

/// An S3 endpoint, bucket name and region, all borrowed.
#[derive(Debug, Clone, Copy)]
pub struct Bucket<'a> {
    scheme: &'a str,
    authority: &'a str,
    name: &'a str,
    region: &'a str,
    addressing: Addressing,
    service: Service,
}

impl<'a> Bucket<'a> {
    /// Creates a bucket reference with path-style addressing, answered by
    /// `service`.
    ///
    /// `endpoint` is the origin of the service, such as
    /// `https://s3.eu-west-1.amazonaws.com`. `region` is the region that the
    /// requests are signed for. A service with no regions of its own names
    /// one in its documentation, often `auto` or `us-east-1`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidEndpoint`] if `endpoint` is not an ASCII HTTP
    /// or HTTPS origin.
    ///
    /// Returns [`Error::InvalidContainer`] if `name` is not a bucket name that
    /// `service` takes. [`Service`] states the rules of each.
    ///
    /// Returns [`Error::InvalidRegion`] if `region` is empty, is longer than
    /// [`MAX_REGION_LEN`] bytes, or is not a region name that `service`
    /// takes.
    pub fn new(
        endpoint: &'a str,
        name: &'a str,
        region: &'a str,
        service: Service,
    ) -> Result<Self> {
        if !crate::http::valid_http_origin(endpoint) {
            return Err(Error::InvalidEndpoint);
        }
        let Some((scheme, authority)) = endpoint.split_once("://") else {
            return Err(Error::InvalidEndpoint);
        };
        // An HTTP client leaves the default port out of the `host` header,
        // and the signature must cover what the client sends.
        let default_port = if scheme == "https" { ":443" } else { ":80" };
        let authority = authority.strip_suffix(default_port).unwrap_or(authority);
        if authority.is_empty() {
            return Err(Error::InvalidEndpoint);
        }
        let name_is_valid = match service {
            Service::Aws => valid_aws_bucket_name(name),
            Service::Compatible => {
                !name.is_empty()
                    && name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
                    })
            }
        };
        if !name_is_valid {
            return Err(Error::InvalidContainer);
        }
        let region_byte = |byte: u8| match service {
            Service::Aws => byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-',
            Service::Compatible => {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_')
            }
        };
        if region.is_empty() || region.len() > MAX_REGION_LEN || !region.bytes().all(region_byte) {
            return Err(Error::InvalidRegion);
        }
        Ok(Self {
            scheme,
            authority,
            name,
            region,
            addressing: Addressing::Path,
            service,
        })
    }

    /// Returns this bucket with `addressing` in place of path-style
    /// addressing.
    pub const fn with_addressing(mut self, addressing: Addressing) -> Self {
        self.addressing = addressing;
        self
    }

    /// Returns the service that answers requests to this bucket.
    pub const fn service(&self) -> Service {
        self.service
    }

    /// Returns the region that requests are signed for.
    pub const fn region(&self) -> &'a str {
        self.region
    }

    // The service name that requests to this bucket are signed for.
    fn signing_service(&self) -> &'static str {
        "s3"
    }

    // The value of the `host` header that the URL implies.
    fn write_host(&self, out: &mut dyn FnMut(&[u8])) {
        if self.addressing == Addressing::VirtualHosted {
            out(self.name.as_bytes());
            out(b".");
        }
        out(self.authority.as_bytes());
    }

    // The path of the URL, which the signature covers as it is written. A
    // request to the bucket itself has the path `/` with virtual-hosted
    // addressing and `/bucket` with path-style addressing.
    fn write_path(&self, out: &mut dyn FnMut(&[u8]), key: Option<&str>) {
        out(b"/");
        if self.addressing == Addressing::Path {
            out(self.name.as_bytes());
            if key.is_some() {
                out(b"/");
            }
        }
        if let Some(key) = key {
            for part in crate::path::encode_object_key(key) {
                out(part);
            }
        }
    }
}

/// The SHA-256 of the content of a write, which the request signs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum PayloadHash {
    /// The encoder computes the SHA-256 of the content.
    ///
    /// This needs the bytes, so the encoder refuses it for a
    /// [`Payload::Streamed`] with [`InvalidPlan::Option`].
    #[default]
    Compute,
    /// The SHA-256 of the content, which you computed.
    ///
    /// S3 refuses the write with 400 `XAmzContentSHA256Mismatch` if the
    /// content does not match.
    Sha256([u8; 32]),
    /// The request does not sign its content, and the encoder never reads
    /// it.
    ///
    /// See the module documentation for what this leaves unchecked.
    Unsigned,
}

/// An element that an S3 listing writes for an object, other than the four
/// that every [`ListEntry`] carries.
///
/// Name the ones you want in a [`PropertySet`] and read a page with
/// [`Objects::fill_listing_with`], which hands you their values as it goes.
/// Read anything that is not listed here with [`ListEntry::property`].
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectProperty {
    /// The storage class, such as `STANDARD` or `GLACIER`.
    StorageClass,
    /// The algorithm of the checksum that S3 keeps for the object, such as
    /// `CRC64NVME`. An object written with more than one reports the first.
    ChecksumAlgorithm,
    /// Whether that checksum covers the whole object, `FULL_OBJECT`, or is
    /// made of the checksums of its parts, `COMPOSITE`.
    ChecksumType,
    /// The owner, as the bytes between the tags of the `Owner` element,
    /// which holds an `ID`. S3 writes it only when the plan asked for
    /// [`ListInclude::OWNER`]. Pass the value to
    /// [`Metadata::new`](crate::Metadata::new) to read the `ID`.
    Owner,
    /// The state of a restore out of an archive class, as the bytes between
    /// the tags of the `RestoreStatus` element. Pass the value to
    /// [`Metadata::new`](crate::Metadata::new) to read what it holds.
    RestoreStatus,
}

impl ObjectProperty {
    /// Every property, in the order of their numbers.
    pub const ALL: &[Self] = &[
        Self::StorageClass,
        Self::ChecksumAlgorithm,
        Self::ChecksumType,
        Self::Owner,
        Self::RestoreStatus,
    ];

    /// How many properties there are, which is the most a set can hold.
    pub const COUNT: usize = Self::ALL.len();

    /// The element name, as S3 writes it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::StorageClass => "StorageClass",
            Self::ChecksumAlgorithm => "ChecksumAlgorithm",
            Self::ChecksumType => "ChecksumType",
            Self::Owner => "Owner",
            Self::RestoreStatus => "RestoreStatus",
        }
    }

    /// Returns the property with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::StorageClass,
            1 => Self::ChecksumAlgorithm,
            2 => Self::ChecksumType,
            3 => Self::Owner,
            4 => Self::RestoreStatus,
            _ => return None,
        })
    }

    // Whether the element holds other elements rather than one text. The
    // page reader reads such an element to its close tag and reports
    // everything between the tags.
    pub(crate) const fn holds_elements(self) -> bool {
        matches!(self, Self::Owner | Self::RestoreStatus)
    }

    // The property that an element name stands for, if it is one of these.
    pub(crate) fn identify(name: &[u8]) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|property| property.name().as_bytes() == name)
    }

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

// A set is a byte, one bit per property.
const _: () = assert!(ObjectProperty::COUNT <= 8);

/// The properties that one read of an S3 page is asked for.
///
/// Build one with [`Self::of`] and pass it to [`Objects::fill_listing_with`].
/// The values come back in the order that [`ObjectProperty`] lists them,
/// whatever order the set was built in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PropertySet(u8);

impl PropertySet {
    /// A set of these properties. Naming one twice is the same as once.
    pub const fn of(properties: &[ObjectProperty]) -> Self {
        let mut mask = 0;
        let mut i = 0;
        while i < properties.len() {
            mask |= properties[i].bit();
            i += 1;
        }
        Self(mask)
    }

    /// A set from its bits, one per property in the order [`ObjectProperty`]
    /// numbers them. A bit that names no property is dropped.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & ((1 << ObjectProperty::COUNT) - 1))
    }

    /// The set's bits, as [`Self::from_bits`] reads them.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether the set holds this property.
    pub const fn contains(self, property: ObjectProperty) -> bool {
        self.0 & property.bit() != 0
    }

    /// How many properties the set holds, which is how many values a read
    /// reports for each entry.
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether the set holds nothing.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Where a property's value stands among the values of an entry: its
    /// rank among the set's members, in the order [`ObjectProperty`] lists
    /// them. Meaningful only for a property the set holds.
    pub const fn slot(self, property: ObjectProperty) -> usize {
        (self.0 & (property.bit() - 1)).count_ones() as usize
    }
}

/// The values that one entry gave for the properties of a set.
///
/// [`Objects::fill_listing_with`] hands one to the closure that builds each
/// entry. Each value is the bytes between the element's tags, as S3 wrote
/// them, under the rules that [`ListEntry::property`] states. A group of
/// keys gives no values.
#[derive(Clone, Copy, Debug)]
pub struct PropertyValues<'x, 'b> {
    set: PropertySet,
    values: &'x [Option<&'b [u8]>],
}

impl<'x, 'b> PropertyValues<'x, 'b> {
    pub(crate) fn new(set: PropertySet, values: &'x [Option<&'b [u8]>]) -> Self {
        Self { set, values }
    }

    /// The set that the page was read with.
    pub const fn set(&self) -> PropertySet {
        self.set
    }

    /// The value the entry gave for one property.
    ///
    /// [`None`] if the property is not in the set or the entry wrote no such
    /// element; an empty slice if it wrote the element empty.
    pub fn get(&self, property: ObjectProperty) -> Option<&'b [u8]> {
        if !self.set.contains(property) {
            return None;
        }
        self.values[self.set.slot(property)]
    }

    /// Every value, one per member of the set, in the order
    /// [`ObjectProperty`] lists them.
    pub const fn all(&self) -> &'x [Option<&'b [u8]>] {
        self.values
    }
}

/// The S3 operations that one set of credentials authorizes on one bucket.
///
/// This is a small value, and it is [`Copy`]. This crate never reads the
/// clock, so every method that encodes a request takes the current time in
/// `now`. It signs the request for that time.
#[derive(Clone, Copy)]
pub struct Objects<'a> {
    bucket: Bucket<'a>,
    credentials: Credentials<'a>,
    sha256: Sha256Provider,
    signing_key: Option<SigningKey>,
    checksums: [Option<ChecksumProvider>; KINDS],
}

impl core::fmt::Debug for Objects<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Objects")
            .field("bucket", &self.bucket)
            .field("credentials", &self.credentials)
            .field("signing_key", &self.signing_key.map(|_| "<redacted>"))
            .field("checksums", &self.checksums)
            .finish_non_exhaustive()
    }
}

// What the signature of one request covers, beside the client's own values.
struct Signed<'p> {
    method: Method,
    // The object, or `None` for the bucket itself.
    key: Option<&'p str>,
    // The query, in the order of its names, with every value one that reads
    // the same encoded again. See `query.rs`.
    query: &'p [Parameter<'p>],
    range: RequestedRange,
    condition: ConditionKind,
    condition_value: Option<&'p [u8]>,
    // Further signed headers, with lowercase names.
    headers: &'p [(&'p str, &'p [u8])],
    metadata: &'p [MetadataPair<'p>],
    content_sha256: &'p [u8],
}

// Where the value of a signed header comes from.
#[derive(Clone, Copy)]
enum HeaderValue<'a> {
    Bytes(&'a [u8]),
    Host,
    Range,
    Date,
}

#[derive(Clone, Copy)]
enum Header<'a> {
    Fixed(&'a str, HeaderValue<'a>),
    Meta(&'a MetadataPair<'a>),
}

impl<'a> Objects<'a> {
    /// Creates a client that signs requests to `bucket` with `credentials`,
    /// computing SHA-256 and HMAC-SHA256 with `sha256`.
    pub const fn new(
        bucket: Bucket<'a>,
        credentials: Credentials<'a>,
        sha256: Sha256Provider,
    ) -> Self {
        Self {
            bucket,
            credentials,
            sha256,
            signing_key: None,
            checksums: [None; KINDS],
        }
    }

    /// Returns this client with the signing key for the day of `now`.
    ///
    /// A request signed on that day uses the key. A request signed on
    /// another day computes a key of its own, which takes four more HMACs.
    /// Call this again when the day changes.
    pub fn with_signing_key(mut self, now: &Timestamps) -> Self {
        self.signing_key = Some(self.derive(now));
        self
    }

    /// Returns this client with `provider` registered for the kind that it
    /// computes.
    ///
    /// A write that asks for [`TransactionalChecksum::Compute`] of that kind
    /// then has the encoder compute the checksum. S3 takes only an MD5 here,
    /// as `Content-MD5`.
    pub const fn with_checksum(mut self, provider: ChecksumProvider) -> Self {
        self.checksums[provider.kind().slot()] = Some(provider);
        self
    }

    fn derive(&self, now: &Timestamps) -> SigningKey {
        SigningKey::derive(
            &self.credentials,
            self.bucket.region,
            self.bucket.signing_service(),
            now,
            &self.sha256,
        )
    }

    /// Writes the signed request head for `get` into `buf`.
    ///
    /// A [`GetKind::Head`] plan becomes a HEAD request. Unlike Azure, S3
    /// serves a [`RequestedRange::Suffix`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `get` cannot become an S3 request.
    /// A suffix of zero bytes is [`InvalidPlan::Range`]. This method
    /// validates the plan before it writes any byte.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or
    /// call [`layered::s3::get_requirements`](crate::layered::s3::get_requirements)
    /// first.
    pub fn encode_get<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        get: &PhysicalGet<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_get(get)?;
        let method = match get.kind {
            GetKind::Bytes => Method::Get,
            GetKind::Head => Method::Head,
        };
        let signed = Signed {
            method,
            key: Some(get.key),
            query: &[],
            headers: &[],
            range: get.range,
            condition: get.condition,
            condition_value: get.condition_value,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
        };
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        encoded(head, method, Payload::Slice(&[]))
    }

    /// Writes the signed request head for `put` into `buf`.
    ///
    /// The head states the length of `content`, which stays where you put
    /// it. `hash` says how the request signs the content.
    ///
    /// S3 takes an MD5 in [`WriteOptions::checksum`], as text or computed,
    /// and no other checksum. For [`Service::Aws`], a condition is either
    /// `If-Match` with an entity tag or `If-None-Match` with `*`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `put` cannot become an S3 request:
    ///
    /// - [`InvalidPlan::PayloadTooLarge`] if `content` is longer than
    ///   [`MAX_PUT_LEN`].
    /// - [`InvalidPlan::Option`] if the plan asks for a checksum other than
    ///   an MD5, or declares an MD5.
    /// - [`InvalidPlan::Option`] if `hash` or the checksum is to be computed
    ///   over a [`Payload::Streamed`].
    /// - [`InvalidPlan::Condition`] if `If-None-Match` carries a value other
    ///   than `*`, for [`Service::Aws`].
    /// - [`InvalidPlan::MetadataName`], [`InvalidPlan::MetadataValue`] or
    ///   [`InvalidPlan::MetadataDuplicate`] for a pair that S3 would not
    ///   store as given. [`MetadataPair`] states the rules.
    /// - [`InvalidPlan::MetadataTooLarge`] if the metadata holds more than
    ///   [`MAX_METADATA_LEN`] bytes, for [`Service::Aws`].
    ///
    /// This method validates the plan before it writes any byte.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or
    /// call [`layered::s3::put_requirements`](crate::layered::s3::put_requirements)
    /// first.
    pub fn encode_put<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        put: &PhysicalPut<'_>,
        content: Payload<'r>,
        hash: PayloadHash,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_put(put, content, hash, &self.checksums, self.bucket.service)?;
        let dry = buf.is_empty();
        let hex;
        let content_sha256 = match hash {
            PayloadHash::Unsigned => UNSIGNED_PAYLOAD.as_bytes(),
            PayloadHash::Sha256(digest) => {
                hex = encoding::hex(&digest);
                hex.as_slice()
            }
            PayloadHash::Compute => {
                // A dry run returns no request, so it does not read the
                // content. The digest is the same length whatever it is.
                hex = if dry {
                    [b'0'; 64]
                } else {
                    encoding::hex(&self.sha256.hash(content.bytes().unwrap_or_default()))
                };
                hex.as_slice()
            }
        };
        let signed = Signed {
            method: Method::Put,
            key: Some(put.key),
            query: &[],
            headers: &[],
            range: RequestedRange::Whole,
            condition: put.condition,
            condition_value: put.condition_value,
            metadata: put.metadata,
            content_sha256,
        };
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        head.header("content-length", |out| {
            out.push(U64Decimal::new(content.len()).as_bytes());
        });
        match put.options.checksum {
            Some(TransactionalChecksum::Md5(text)) => {
                head.header("content-md5", |out| out.push(text.as_bytes()));
            }
            Some(TransactionalChecksum::Compute(kind)) => {
                // Validation refused any other kind, and a kind with no
                // provider.
                if let Some(provider) = &self.checksums[kind.slot()] {
                    let mut sum = provider.start();
                    sum.update(content.bytes().unwrap_or_default());
                    let mut into = [0; crate::checksum::BASE64_LEN];
                    let text = sum.finish().base64(&mut into);
                    head.header("content-md5", |out| out.push(text.as_bytes()));
                }
            }
            _ => {}
        }
        encoded(head, Method::Put, content)
    }

    /// Writes the signed request head for `delete` into `buf`.
    ///
    /// S3 answers the removal of a key that holds no object as it answers any
    /// other removal. The outcome therefore does not say whether an object
    /// was there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `delete` cannot become an S3
    /// request:
    ///
    /// - [`InvalidPlan::Option`] for a [`DeleteKind`] other than
    ///   [`DeleteKind::Object`]. S3 keeps no snapshots.
    /// - [`InvalidPlan::Condition`] for [`ConditionKind::IfNoneMatch`], for
    ///   [`Service::Aws`]. AWS removes on `If-Match` only.
    ///
    /// This method validates the plan before it writes any byte.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or
    /// call [`layered::s3::delete_requirements`](crate::layered::s3::delete_requirements)
    /// first.
    pub fn encode_delete<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        delete: &PhysicalDelete<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_delete(delete, self.bucket.service)?;
        let signed = Signed {
            method: Method::Delete,
            key: Some(delete.key),
            query: &[],
            headers: &[],
            range: RequestedRange::Whole,
            condition: delete.condition,
            condition_value: delete.condition_value,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
        };
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        encoded(head, Method::Delete, Payload::Slice(&[]))
    }

    // Writes the URL and every signed header. A dry run writes a signature
    // of zeros, which is as long as a real one.
    fn write_head(
        &self,
        head: &mut HeadWriter<'_>,
        signed: &Signed<'_>,
        dry: bool,
        now: &Timestamps,
    ) {
        let signature = if dry {
            [b'0'; 64]
        } else {
            self.signature(signed, now)
        };
        head.url(|out| {
            out.push(self.bucket.scheme.as_bytes());
            out.push(b"://");
            self.bucket.write_host(&mut |piece| out.push(piece));
            self.bucket
                .write_path(&mut |piece| out.push(piece), signed.key);
            query::write_in_url(&mut |piece| out.push(piece), signed.query);
        });
        let token = self.token();
        head.header("authorization", |out| {
            out.push(sigv4::ALGORITHM.as_bytes());
            out.push(b" Credential=");
            out.push(self.credentials.key_id().as_bytes());
            out.push(b"/");
            self.write_scope(&mut |piece| out.push(piece), now);
            out.push(b", SignedHeaders=");
            write_signed_names(&mut |piece| out.push(piece), signed, token);
            out.push(b", Signature=");
            out.push(&signature);
        });
        for header in ordered_headers(signed, token) {
            match header {
                Header::Fixed(_, HeaderValue::Host) => {}
                Header::Fixed(name, value) => head.header(name, |out| match value {
                    HeaderValue::Bytes(bytes) => out.push(bytes),
                    HeaderValue::Range => write_range(&mut |piece| out.push(piece), signed.range),
                    HeaderValue::Date => out.push(now.iso8601().as_bytes()),
                    HeaderValue::Host => {}
                }),
                Header::Meta(pair) => head.header_parts(
                    |out| {
                        out.push(METADATA_PREFIX.as_bytes());
                        for byte in pair.name.bytes() {
                            out.push(&[byte.to_ascii_lowercase()]);
                        }
                    },
                    |out| write_metadata_value(&mut |piece| out.push(piece), pair.value),
                ),
            }
        }
    }

    // The session token and the header that carries it.
    fn token(&self) -> Option<(&'static str, &'a str)> {
        self.credentials
            .session_token()
            .map(|token| (self.credentials.token_header(), token))
    }

    fn write_scope(&self, out: &mut dyn FnMut(&[u8]), now: &Timestamps) {
        out(now.date().as_bytes());
        out(b"/");
        out(self.bucket.region.as_bytes());
        out(b"/");
        out(self.bucket.signing_service().as_bytes());
        out(b"/aws4_request");
    }

    // Returns the signature of the request, as lowercase hexadecimal. The
    // canonical request is hashed as it is written, so it is never held.
    fn signature(&self, signed: &Signed<'_>, now: &Timestamps) -> [u8; 64] {
        let mut sum = self.sha256.start();
        self.write_canonical_request(&mut |piece| sum.update(piece), signed, now);
        let canonical = sum.finish();
        let key = match self.signing_key {
            Some(key) if key.covers(now) => key,
            _ => self.derive(now),
        };
        key.sign(
            &canonical,
            self.bucket.region,
            self.bucket.signing_service(),
            now,
            &self.sha256,
        )
    }

    // The canonical request of SigV4.
    fn write_canonical_request(
        &self,
        out: &mut dyn FnMut(&[u8]),
        signed: &Signed<'_>,
        now: &Timestamps,
    ) {
        out(signed.method.as_str().as_bytes());
        out(b"\n");
        self.bucket.write_path(out, signed.key);
        out(b"\n");
        // The URL carries the query in its canonical form: every byte but the
        // unreserved ones percent-encoded, in upper case, and the parameters
        // in the order of their names. So this is the same text.
        query::write(out, signed.query);
        out(b"\n");
        let token = self.token();
        for header in ordered_headers(signed, token) {
            write_header_name(out, header);
            out(b":");
            match header {
                Header::Fixed(_, HeaderValue::Bytes(bytes)) => write_canonical_value(out, bytes),
                Header::Fixed(_, HeaderValue::Host) => self.bucket.write_host(out),
                Header::Fixed(_, HeaderValue::Range) => write_range(out, signed.range),
                Header::Fixed(_, HeaderValue::Date) => out(now.iso8601().as_bytes()),
                Header::Meta(pair) if encodes(pair.value) => write_metadata_value(out, pair.value),
                Header::Meta(pair) => write_canonical_value(out, pair.value.as_bytes()),
            }
            out(b"\n");
        }
        out(b"\n");
        write_signed_names(out, signed, token);
        out(b"\n");
        out(signed.content_sha256);
    }

    /// Reads the response head of a GET or a HEAD and reports what to do
    /// next.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_get`]. A
    /// failure of a GET is [`GetHeadOutcome::NeedErrorBody`]: read the body
    /// and pass it to [`Self::accept_error_body`]. A failure of a HEAD is
    /// final, and names no error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against
    /// `shape`. A `Content-Range` that is missing, or whose end is before its
    /// start, is [`ResponseFault::Head`], and so is a success without
    /// `Content-Length` to a [`Service::Aws`] client. A range other than the
    /// one the plan requested, and a ranged plan answered with status 200,
    /// are [`ResponseFault::Range`].
    pub fn accept_get_head<'h>(
        &self,
        shape: GetShape,
        head: ResponseHead<'h>,
    ) -> Result<GetHeadOutcome<'h>> {
        let ranged = shape.range != RequestedRange::Whole;
        match head.status {
            206 if !ranged => Err(ResponseFault::Range.into()),
            200 if ranged => Err(ResponseFault::Range.into()),
            // AWS states the length of every response that it serves.
            200 | 206 if self.bucket.service == Service::Aws && head.content_length.is_none() => {
                Err(ResponseFault::Head.into())
            }
            200 | 206 => accept_success(shape, head),
            304 if shape.condition != ConditionKind::IfNoneMatch => {
                Err(ResponseFault::Status.into())
            }
            304 => Ok(GetHeadOutcome::NotModified { e_tag: head.e_tag }),
            412 if shape.condition != ConditionKind::IfMatch => Err(ResponseFault::Status.into()),
            412 => Ok(GetHeadOutcome::PreconditionFailed),
            416 => Ok(GetHeadOutcome::RangeNotSatisfiable {
                object_size: match head.content_range.map(parse_content_range) {
                    Some(Some(ContentRange::Unsatisfied { total })) => total,
                    None => None,
                    Some(_) => return Err(ResponseFault::Head.into()),
                },
            }),
            200..=299 => Err(ResponseFault::Status.into()),
            // A HEAD response has no body to name the error.
            404 if shape.kind == GetKind::Head => Ok(GetHeadOutcome::NotFound { kind: None }),
            status if shape.kind == GetKind::Head => Ok(GetHeadOutcome::ServiceFailure(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(GetHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`GetHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// Pass the `status` and the `request_id` of that failure, and the body
    /// that you read. Pass an empty body if you could not read one: the
    /// outcome is then final with the error unnamed.
    pub fn accept_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> GetHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => GetHeadOutcome::NotFound { kind },
            status => GetHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Reads the response head of a write and reports what S3 did.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_put`]. A
    /// failure is [`PutHeadOutcome::NeedErrorBody`]: read the body and pass
    /// it to [`Self::accept_put_error_body`].
    ///
    /// A conditional write that another conditional write overtook is
    /// refused with 409 `ConditionalRequestConflict`, which reaches you as a
    /// service failure. Retry it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against
    /// `shape`. A success status that a write never returns, and a failed
    /// condition on a write that carried none, are both
    /// [`ResponseFault::Status`].
    pub fn accept_put_head<'h>(
        &self,
        shape: PutShape,
        head: ResponseHead<'h>,
    ) -> Result<PutHeadOutcome<'h>> {
        match head.status {
            200 => Ok(PutHeadOutcome::Created {
                meta: ObjectMeta {
                    size: None,
                    e_tag: head.e_tag,
                    last_modified: text_header(head.last_modified)?,
                    version: head.version,
                    content_encoding: head.content_encoding,
                    content_type: head.content_type,
                },
            }),
            412 if shape.condition == ConditionKind::None => Err(ResponseFault::Status.into()),
            412 => Ok(PutHeadOutcome::PreconditionFailed),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(PutHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`PutHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_error_body`] for a write, and reads the
    /// body the same way. A missing bucket is [`PutHeadOutcome::NotFound`].
    pub fn accept_put_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> PutHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => PutHeadOutcome::NotFound { kind },
            status => PutHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Reads the response head of a removal and reports what S3 did.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_delete`]. A
    /// failure is [`DeleteHeadOutcome::NeedErrorBody`]: read the body and
    /// pass it to [`Self::accept_delete_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against
    /// `shape`. A success status other than 204, and a failed condition on a
    /// removal that carried none, are both [`ResponseFault::Status`]. A
    /// [`Service::Compatible`] client also takes 200 as success.
    pub fn accept_delete_head<'h>(
        &self,
        shape: DeleteShape,
        head: ResponseHead<'h>,
    ) -> Result<DeleteHeadOutcome<'h>> {
        match head.status {
            204 => Ok(DeleteHeadOutcome::Accepted),
            // Some services that implement the S3 API answer 200.
            200 if self.bucket.service == Service::Compatible => Ok(DeleteHeadOutcome::Accepted),
            412 if shape.condition == ConditionKind::None => Err(ResponseFault::Status.into()),
            412 => Ok(DeleteHeadOutcome::PreconditionFailed),
            200..=203 | 205..=299 => Err(ResponseFault::Status.into()),
            status => Ok(DeleteHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`DeleteHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`Self::accept_error_body`] for a removal, and reads the
    /// body the same way. A 404 names a missing bucket, or a missing object
    /// under an `If-Match` condition.
    pub fn accept_delete_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> DeleteHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => DeleteHeadOutcome::NotFound { kind },
            status => DeleteHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Writes the signed request head for one page of `list` into `buf`.
    ///
    /// The request is a ListObjectsV2. [`PhysicalList::marker`] carries the
    /// continuation token of the previous page, and
    /// [`PhysicalList::start_after`] the key to start after.
    /// [`ListInclude::OWNER`] asks for each object's owner. The response carries the
    /// page as a document in its body: read it whole and pass it to
    /// [`Self::fill_listing`].
    ///
    /// AWS reports at most 1,000 entries in one page, and applies that limit
    /// to a larger [`PhysicalList::max_results`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `list` cannot become an S3 request:
    ///
    /// - [`InvalidPlan::Marker`] for an empty marker. The first page carries
    ///   none.
    /// - [`InvalidPlan::Option`] for [`ListInclude::METADATA`] in
    ///   [`PhysicalList::include`]. S3 lists no metadata.
    ///
    /// This method validates the plan before it writes any byte.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or
    /// call [`layered::s3::list_requirements`](crate::layered::s3::list_requirements)
    /// first.
    pub fn encode_list<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        list: &PhysicalList<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_list(list)?;
        // In the order of the names, which SigV4 signs. Each literal is
        // unreserved text, so it reads the same encoded again. The keys come
        // back URL-encoded, so a key that XML cannot carry still arrives.
        let query = [
            list.marker
                .map(|marker| ("continuation-token", QueryValue::Encoded(marker.as_bytes()))),
            list.delimited
                .then_some(("delimiter", QueryValue::Encoded(DELIMITER))),
            Some(("encoding-type", QueryValue::Literal("url"))),
            list.include
                .contains(ListInclude::OWNER)
                .then_some(("fetch-owner", QueryValue::Literal("true"))),
            Some(("list-type", QueryValue::Literal("2"))),
            list.max_results
                .map(|max| ("max-keys", QueryValue::Number(max))),
            (!list.prefix.is_empty())
                .then_some(("prefix", QueryValue::Encoded(list.prefix.as_bytes()))),
            list.start_after
                .filter(|key| !key.is_empty())
                .map(|key| ("start-after", QueryValue::Encoded(key.as_bytes()))),
        ];
        let signed = Signed {
            method: Method::Get,
            key: None,
            query: &query,
            headers: &[],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
        };
        let dry = buf.is_empty();
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, dry, now);
        encoded(head, Method::Get, Payload::Slice(&[]))
    }

    /// Reads the response head of a listing and reports what S3 did.
    ///
    /// A page is [`ListHeadOutcome::Page`]. S3 often sends it without
    /// `Content-Length`, so cap what you read. A failure is
    /// [`ListHeadOutcome::NeedErrorBody`]: read the body and pass it to
    /// [`Self::accept_list_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`], and a
    /// `Content-Length` that is not a number is [`ResponseFault::Head`].
    pub fn accept_list_head<'h>(&self, head: ResponseHead<'h>) -> Result<ListHeadOutcome<'h>> {
        match head.status {
            200 => Ok(ListHeadOutcome::Page {
                expected_len: decimal_header(head.content_length)?,
            }),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(ListHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`ListHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_error_body`] for a listing, and reads the body
    /// the same way. A missing bucket is [`ListHeadOutcome::NotFound`].
    pub fn accept_list_error_body<'h>(
        &self,
        status: u16,
        request_id: Option<&'h [u8]>,
        body: &[u8],
    ) -> ListHeadOutcome<'h> {
        let kind = body_kind(body);
        match status {
            404 => ListHeadOutcome::NotFound { kind },
            status => ListHeadOutcome::ServiceFailure(failure(status, kind, request_id)),
        }
    }

    /// Reads a page out of the response body of a listing.
    ///
    /// This is [`Blobs::fill_listing`](crate::Blobs::fill_listing) for S3,
    /// under the same rules: reading is destructive, and your array must
    /// hold the whole page. An array of `max_results` entries always does,
    /// and so does one of 1,000 entries for AWS.
    ///
    /// Each entry is an [`EntryKind::Object`](crate::EntryKind::Object) or
    /// an [`EntryKind::Prefix`](crate::EntryKind::Prefix). AWS writes every
    /// object of a page before its groups of keys, so the page is not in the
    /// order of its keys when it holds both. An object's entity tag keeps
    /// its quotes, and its date is ISO 8601: read it with
    /// [`layered::iso8601_ms`](crate::layered::iso8601_ms). Read
    /// `StorageClass` and the other elements of an object with
    /// [`ListEntry::property`], or in the same pass as the rest of the page
    /// with [`Self::fill_listing_with`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`] if the page holds more entries than the
    /// array, with `required` set to the number it holds. Ask the service for
    /// the page again, with a larger array.
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a ListObjectsV2 page. That includes a page that says more keys
    /// follow and names no continuation token. It also includes a page whose
    /// keys hold a `%` or a `+`, from a service that ignored the request to
    /// URL-encode them: such a key cannot be read back with certainty.
    pub fn fill_listing<'b, E: From<ListEntry<'b>>>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
    ) -> Result<Listing<'b>> {
        crate::xml::s3::fill_listing(body, into, PropertySet::default(), |entry, _| entry.into())
    }

    /// Reads a page the way [`Self::fill_listing`] does, and hands you the
    /// values of the properties in `wanted` as it goes.
    ///
    /// This is [`Blobs::fill_listing_with`](crate::Blobs::fill_listing_with)
    /// for S3. `build` is called once per entry, with the entry and its
    /// values, and what it returns is written into your array. The values
    /// point into `body`, like the entry. A group of keys gives no values.
    ///
    /// ```
    /// # use borink_object_storage_proto::s3::{Objects, ObjectProperty, PropertySet};
    /// # use borink_object_storage_proto::{ListEntry, Result};
    /// # fn read(objects: &Objects<'_>, body: &mut [u8]) -> Result<()> {
    /// let wanted = PropertySet::of(&[ObjectProperty::StorageClass]);
    /// let mut entries = [(ListEntry::default(), None); 1000];
    /// objects.fill_listing_with(body, &mut entries, wanted, |entry, values| {
    ///     (entry, values.get(ObjectProperty::StorageClass))
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Self::fill_listing`].
    pub fn fill_listing_with<'b, E>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
        wanted: PropertySet,
        build: impl FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) -> E,
    ) -> Result<Listing<'b>> {
        crate::xml::s3::fill_listing(body, into, wanted, build)
    }
}

// S3 groups keys at any delimiter, but a plan groups them at `/`, which Azure
// takes as well.
const DELIMITER: &[u8] = b"/";

/// Returns the error code that an S3 error body names.
///
/// Unknown codes are returned unchanged.
pub fn error_code(body: &[u8]) -> Option<&str> {
    crate::xml::error_code(body)
}

/// Classifies the S3 error that `body` names. Set `truncated` if a read
/// limit stopped the body early.
pub fn classify_error(body: &[u8], truncated: bool) -> Classification {
    match body_kind(body) {
        Some(kind) => Classification::Classified(kind),
        None if truncated => Classification::Incomplete,
        None => Classification::Unknown,
    }
}

fn body_kind(body: &[u8]) -> Option<ServiceErrorKind> {
    crate::xml::error_code(body).and_then(|code| kind_for_code(code.as_bytes()))
}

fn kind_for_code(code: &[u8]) -> Option<ServiceErrorKind> {
    Some(match code {
        b"NoSuchKey" => ServiceErrorKind::NotFound,
        b"NoSuchBucket" => ServiceErrorKind::NoSuchContainer,
        b"PreconditionFailed" => ServiceErrorKind::Precondition,
        b"InvalidRange" => ServiceErrorKind::RangeNotSatisfiable,
        b"SlowDown" | b"RequestLimitExceeded" | b"TooManyRequests" => ServiceErrorKind::Throttled,
        b"RequestTimeout" => ServiceErrorKind::Timeout,
        b"AccessDenied"
        | b"ExpiredToken"
        | b"InvalidAccessKeyId"
        | b"InvalidToken"
        | b"SignatureDoesNotMatch"
        | b"TokenRefreshRequired"
        | b"RequestTimeTooSkewed" => ServiceErrorKind::Unauthorized,
        b"InternalError" | b"ServiceUnavailable" => ServiceErrorKind::Service,
        _ => return None,
    })
}

// Every signed header in the order of its name: the fixed set sorted, merged
// with the metadata sorted by lowercase name.
fn ordered_headers<'s>(
    signed: &'s Signed<'s>,
    token: Option<(&'static str, &'s str)>,
) -> impl Iterator<Item = Header<'s>> {
    let mut fixed = [Header::Fixed("", HeaderValue::Host); 16];
    let mut count = 0;
    let entries = [
        Some(("host", HeaderValue::Host)),
        Some((
            "x-amz-content-sha256",
            HeaderValue::Bytes(signed.content_sha256),
        )),
        Some(("x-amz-date", HeaderValue::Date)),
        token.map(|(name, token)| (name, HeaderValue::Bytes(token.as_bytes()))),
        (signed.range != RequestedRange::Whole).then_some(("range", HeaderValue::Range)),
        condition_header(signed.condition)
            .zip(signed.condition_value)
            .map(|(name, value)| (name, HeaderValue::Bytes(value))),
    ]
    .into_iter()
    .flatten()
    .chain(
        signed
            .headers
            .iter()
            .map(|(name, value)| (*name, HeaderValue::Bytes(value))),
    );
    for (name, value) in entries {
        let mut at = count;
        while at > 0 && matches!(fixed[at - 1], Header::Fixed(previous, _) if previous > name) {
            fixed[at] = fixed[at - 1];
            at -= 1;
        }
        fixed[at] = Header::Fixed(name, value);
        count += 1;
    }
    let mut index = 0;
    let mut metadata = sorted_metadata(signed.metadata).peekable();
    core::iter::from_fn(move || {
        let next = (index < count).then(|| fixed[index]);
        match (next, metadata.peek()) {
            (None, None) => None,
            (Some(header), None) => {
                index += 1;
                Some(header)
            }
            (Some(Header::Fixed(name, value)), Some(pair))
                if name.bytes().cmp(metadata_header_name(pair)) == Ordering::Less =>
            {
                index += 1;
                Some(Header::Fixed(name, value))
            }
            _ => metadata.next().map(Header::Meta),
        }
    })
}

fn metadata_header_name<'p>(pair: &'p MetadataPair<'p>) -> impl Iterator<Item = u8> + 'p {
    METADATA_PREFIX
        .bytes()
        .chain(pair.name.bytes().map(|byte| byte.to_ascii_lowercase()))
}

fn write_header_name(out: &mut dyn FnMut(&[u8]), header: Header<'_>) {
    match header {
        Header::Fixed(name, _) => out(name.as_bytes()),
        Header::Meta(pair) => {
            for byte in metadata_header_name(pair) {
                out(&[byte]);
            }
        }
    }
}

// The names of the signed headers, in order, separated by `;`.
fn write_signed_names(
    out: &mut dyn FnMut(&[u8]),
    signed: &Signed<'_>,
    token: Option<(&'static str, &str)>,
) {
    for (index, header) in ordered_headers(signed, token).enumerate() {
        if index != 0 {
            out(b";");
        }
        write_header_name(out, header);
    }
}

// SigV4 signs a header value without the spaces at either end, and with each
// run of spaces inside it as one space. A value sent as it is holds no other
// whitespace.
fn write_canonical_value(out: &mut dyn FnMut(&[u8]), value: &[u8]) {
    let value = value.trim_ascii();
    let mut start = 0;
    let mut at = 0;
    while at < value.len() {
        if value[at] == b' ' {
            out(&value[start..=at]);
            while at < value.len() && value[at] == b' ' {
                at += 1;
            }
            start = at;
        } else {
            at += 1;
        }
    }
    out(&value[start..]);
}

// The pairs in the order of their lowercase names. A plan carries a handful
// of pairs, and validation refused two with the same name, so each step
// finds the least name after the previous one.
fn sorted_metadata<'m>(
    metadata: &'m [MetadataPair<'m>],
) -> impl Iterator<Item = &'m MetadataPair<'m>> {
    let mut previous: Option<&'m str> = None;
    core::iter::from_fn(move || {
        let next = metadata
            .iter()
            .filter(|pair| {
                previous.is_none_or(|name| lowercase_cmp(pair.name, name) == Ordering::Greater)
            })
            .min_by(|a, b| lowercase_cmp(a.name, b.name))?;
        previous = Some(next.name);
        Some(next)
    })
}

fn lowercase_cmp(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()))
}

// The naming rules that AWS documents for general purpose buckets.
fn valid_aws_bucket_name(name: &str) -> bool {
    const PREFIXES: [&str; 3] = ["xn--", "sthree-", "amzn-s3-demo-"];
    const SUFFIXES: [&str; 5] = ["-s3alias", "--ol-s3", ".mrap", "--x-s3", "--table-s3"];
    let bytes = name.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && !name.contains("..")
        && !looks_like_ipv4(name)
        && !PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        && !SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

// Four groups of one to three digits, separated by dots.
fn looks_like_ipv4(name: &str) -> bool {
    name.split('.').count() == 4
        && name.split('.').all(|group| {
            (1..=3).contains(&group.len()) && group.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(InvalidPlan::EmptyKey.into());
    }
    if key.len() > MAX_KEY_LEN {
        return Err(InvalidPlan::KeyTooLong.into());
    }
    // A host resolves `.` and `..` segments out of the URL before it sends
    // it, which would address another object.
    if key.split('/').any(|segment| matches!(segment, "." | "..")) {
        return Err(InvalidPlan::KeyWouldBeNormalized.into());
    }
    Ok(())
}

fn validate_get(get: &PhysicalGet<'_>) -> Result<()> {
    validate_key(get.key)?;
    match get.range {
        RequestedRange::Bounded { start, end } if start >= end => {
            return Err(InvalidPlan::Range.into());
        }
        RequestedRange::Suffix(0) => return Err(InvalidPlan::Range.into()),
        RequestedRange::Whole => {}
        _ if get.kind == GetKind::Head => return Err(InvalidPlan::RangedHead.into()),
        _ => {}
    }
    validate_condition(get.condition, get.condition_value)
}

fn validate_put(
    put: &PhysicalPut<'_>,
    content: Payload<'_>,
    hash: PayloadHash,
    checksums: &[Option<ChecksumProvider>; KINDS],
    service: Service,
) -> Result<()> {
    validate_key(put.key)?;
    if content.len() > MAX_PUT_LEN {
        return Err(InvalidPlan::PayloadTooLarge.into());
    }
    validate_metadata(put.metadata, service)?;
    let has_bytes = content.bytes().is_some();
    match put.options.checksum {
        None => {}
        Some(TransactionalChecksum::Md5(text)) => ChecksumKind::Md5.check_base64(text)?,
        Some(TransactionalChecksum::Compute(ChecksumKind::Md5))
            if has_bytes && checksums[ChecksumKind::Md5.slot()].is_some() => {}
        Some(_) => return Err(InvalidPlan::Option.into()),
    }
    if put.options.declared_md5.is_some() || (hash == PayloadHash::Compute && !has_bytes) {
        return Err(InvalidPlan::Option.into());
    }
    validate_condition(put.condition, put.condition_value)?;
    // AWS writes on `If-None-Match` only if no object holds the key, and takes
    // no value but `*`.
    if service == Service::Aws
        && put.condition == ConditionKind::IfNoneMatch
        && put.condition_value != Some(b"*")
    {
        return Err(InvalidPlan::Condition.into());
    }
    Ok(())
}

fn validate_delete(delete: &PhysicalDelete<'_>, service: Service) -> Result<()> {
    validate_key(delete.key)?;
    if delete.kind != DeleteKind::Object {
        return Err(InvalidPlan::Option.into());
    }
    if service == Service::Aws && delete.condition == ConditionKind::IfNoneMatch {
        return Err(InvalidPlan::Condition.into());
    }
    validate_condition(delete.condition, delete.condition_value)
}

// No rule of `validate_key` applies to a prefix, which the query carries, and
// S3 takes any number of entries, zero among them.
fn validate_list(list: &PhysicalList<'_>) -> Result<()> {
    // S3 hands out no empty continuation token.
    if list.marker.is_some_and(str::is_empty) {
        return Err(InvalidPlan::Marker.into());
    }
    if list.include.contains(ListInclude::METADATA) {
        return Err(InvalidPlan::Option.into());
    }
    Ok(())
}

// S3 sends a metadata pair as an `x-amz-meta-` header, so the name must be a
// token. `write_metadata_value` writes the value, and S3 matches a name
// without case.
fn validate_metadata(metadata: &[MetadataPair<'_>], service: Service) -> Result<()> {
    for (index, pair) in metadata.iter().enumerate() {
        if pair.name.is_empty() || !pair.name.bytes().all(token_byte) {
            return Err(InvalidPlan::MetadataName.into());
        }
        // An empty value is a pair with no text, which S3 stores. A value is
        // refused if a read would not return it exactly. S3 stores CR and LF
        // as spaces, even from an encoded word. It returns an ASCII value
        // unencoded, so HTTP drops the whitespace at either end, and a token
        // that reads as an encoded word is decoded.
        let edge = |byte: Option<&u8>| matches!(byte, Some(b' ' | b'\t'));
        let bytes = pair.value.as_bytes();
        if pair.value.contains(['\r', '\n'])
            || edge(bytes.first())
            || edge(bytes.last())
            || rfc2047::looks_encoded(pair.value)
        {
            return Err(InvalidPlan::MetadataValue.into());
        }
        if metadata[..index]
            .iter()
            .any(|earlier| earlier.name.eq_ignore_ascii_case(pair.name))
        {
            return Err(InvalidPlan::MetadataDuplicate.into());
        }
    }
    let len = metadata.iter().fold(0usize, |len, pair| {
        len.saturating_add(pair.name.len())
            .saturating_add(pair.value.len())
    });
    if service == Service::Aws && len > MAX_METADATA_LEN {
        return Err(InvalidPlan::MetadataTooLarge.into());
    }
    Ok(())
}

// Returns whether a write sends `value` as an encoded word. A header value
// cannot hold text outside ASCII or a control character other than a tab,
// and S3 returns such a value encoded. A tab is encoded too, because the
// canonical form of SigV4 folds spaces alone. S3 returns a tab as it is.
fn encodes(value: &str) -> bool {
    !value.is_ascii() || value.bytes().any(|byte| byte.is_ascii_control())
}

// Writes a metadata value as the header carries it, encoded or as it is. An
// encoded word holds no space, so its canonical form for SigV4 is the same.
fn write_metadata_value(out: &mut dyn FnMut(&[u8]), value: &str) {
    if encodes(value) {
        rfc2047::write(out, value);
    } else {
        out(value.as_bytes());
    }
}

// The bytes that RFC 9110 allows in a token, which S3 takes in a metadata
// name.
fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{MetadataPair, sorted_metadata, write_canonical_value};

    fn canonical(value: &str) -> Vec<u8> {
        let mut out = Vec::new();
        write_canonical_value(&mut |piece| out.extend_from_slice(piece), value.as_bytes());
        out
    }

    #[test]
    fn a_canonical_value_trims_and_folds_spaces() {
        assert_eq!(canonical("plain"), b"plain");
        assert_eq!(canonical("  two   words  "), b"two words");
        assert_eq!(canonical("a b  c   d"), b"a b c d");
        assert_eq!(canonical(""), b"");
        assert_eq!(canonical("   "), b"");
    }

    #[test]
    fn metadata_is_signed_in_the_order_of_its_lowercase_names() {
        let pairs = [
            MetadataPair {
                name: "Zeta",
                value: "",
            },
            MetadataPair {
                name: "a-b",
                value: "",
            },
            MetadataPair {
                name: "A",
                value: "",
            },
            MetadataPair {
                name: "beta",
                value: "",
            },
        ];
        let names: Vec<&str> = sorted_metadata(&pairs).map(|pair| pair.name).collect();
        assert_eq!(names, ["A", "a-b", "beta", "Zeta"]);
        assert_eq!(sorted_metadata(&[]).count(), 0);
    }
}
