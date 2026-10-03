//! S3 requests and responses, signed with AWS Signature Version 4.
//!
//! # How a request works
//!
//! 1. Create a [`Bucket`] and [`Credentials`], and from them an [`Objects`]
//!    client with a [`Sha256Provider`]. The `borink-object-storage-crypto`
//!    crate has providers, and the function that [`Credentials::new`] takes
//!    to wipe its copy of the secret.
//! 2. Describe the operation with a [`PhysicalGet`], a [`PhysicalPut`], a
//!    [`PhysicalDelete`] or a [`PhysicalList`].
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
//!     objects.accept_get_error_body(get.shape(), failure, body),
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
//! A listing reads one page per request. Encode a [`PhysicalList`] with
//! [`Objects::encode_list`], read the whole body of a
//! [`ListHeadOutcome::Page`], and pass it to [`Objects::fill_listing`]. Pass
//! the page's [`Listing::next_marker`] as the marker of the next plan.
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
//!     delimiter: Some("/"),
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
//! # Uploads in parts
//!
//! An object longer than [`MAX_PUT_LEN`], or one that you send as it
//! arrives, is written in parts:
//!
//! 1. Create an upload with [`Objects::encode_create_upload`], and read its
//!    ID out of the answer with [`Objects::read_upload_id`]. The metadata of
//!    the object goes into this request.
//! 2. Stage each part with [`Objects::encode_stage_part`], under a number
//!    from 1, in any order and at once if you like. Keep the entity tag that
//!    [`StageHeadOutcome::Staged`](crate::StageHeadOutcome::Staged) carries
//!    for each. On AWS, every part but the last holds at least
//!    [`MIN_PART_LEN`] bytes.
//! 3. Commit the parts with [`Objects::encode_commit_parts`], which takes a
//!    [`PhysicalCommit`](crate::PhysicalCommit).
//!    S3 answers with status 200 before it has finished, and writes the
//!    result into the body, which may still be an error. Read it with
//!    [`Objects::accept_commit_parts_body`].
//!
//! [`Objects::encode_list_parts`] reads the parts that an upload holds, to
//! resume it. [`Objects::encode_abort_upload`] ends an upload without an
//! object. An upload that is neither committed nor aborted keeps its parts,
//! and AWS bills them.
//!
//! ```
//! # use borink_object_storage_proto::s3::{Bucket, Service, Objects, PayloadHash};
//! # use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
//! # use borink_object_storage_proto::sigv4::{Sha256Provider, Sha256State};
//! # const SHA256: Sha256Provider =
//! #     Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32], |_, _| [0; 32]);
//! use borink_object_storage_proto::s3::{PartRef, PhysicalCreateUpload, PhysicalStagePart};
//! use borink_object_storage_proto::{
//!     CommitHeadOutcome, HeaderSpan, Payload, PhysicalCommit, ResponseHead, StageHeadOutcome,
//!     Timestamps, layered,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let bucket = Bucket::new(
//! #     "https://s3.eu-west-1.amazonaws.com", "objects", "eu-west-1", Service::Aws,
//! # )?;
//! # let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe_best_effort)?;
//! # let now = Timestamps::from_unix(1_787_400_000);
//! # let objects = Objects::new(bucket, credentials, SHA256);
//! let create = PhysicalCreateUpload::new("large.bin");
//! let size = layered::s3::create_upload_requirements(&objects, &create, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request = objects.encode_create_upload(&mut buffer, &mut headers, &create, &now)?;
//! assert_eq!(
//!     request.url(),
//!     "https://s3.eu-west-1.amazonaws.com/objects/large.bin?uploads="
//! );
//! let mut body = Vec::from(
//!     b"<InitiateMultipartUploadResult><Bucket>objects</Bucket><Key>large.bin</Key>\
//!       <UploadId>VXBsb2FkSUQ</UploadId></InitiateMultipartUploadResult>"
//!         .as_slice(),
//! );
//! let upload_id = String::from(objects.read_upload_id(&mut body)?);
//!
//! let stage = PhysicalStagePart::new("large.bin", &upload_id, 1);
//! let part = Payload::Slice(b"the only part");
//! let hash = PayloadHash::Compute;
//! let size = layered::s3::stage_part_requirements(&objects, &stage, part, hash, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request = objects.encode_stage_part(&mut buffer, &mut headers, &stage, part, hash, &now)?;
//! assert_eq!(
//!     request.url(),
//!     "https://s3.eu-west-1.amazonaws.com/objects/large.bin?partNumber=1&uploadId=VXBsb2FkSUQ"
//! );
//! let head = ResponseHead::from_headers(200, [("ETag", b"\"e1\"".as_slice())]);
//! let StageHeadOutcome::Staged { e_tag: Some(e_tag) } = objects.accept_stage_part_head(head)?
//! else {
//!     panic!("S3 staged no part");
//! };
//!
//! let commit = PhysicalCommit::new("large.bin");
//! let parts = [PartRef { number: 1, e_tag }];
//! let size = layered::s3::commit_parts_requirements(&objects, &commit, &upload_id, &parts, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request =
//!     objects.encode_commit_parts(&mut buffer, &mut headers, &commit, &upload_id, &parts, &now)?;
//! assert_eq!(
//!     request.payload().bytes(),
//!     Some(
//!         b"<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
//!           <Part><PartNumber>1</PartNumber><ETag>\"e1\"</ETag></Part>\
//!           </CompleteMultipartUpload>"
//!             .as_slice()
//!     )
//! );
//! let head = ResponseHead::new(200);
//! let outcome = objects.accept_commit_parts_head(commit.shape(), head)?;
//! assert!(matches!(outcome, CommitHeadOutcome::NeedResultBody { .. }));
//! let mut body = Vec::from(
//!     b"<CompleteMultipartUploadResult><Key>large.bin</Key>\
//!       <ETag>&quot;c0-1&quot;</ETag></CompleteMultipartUploadResult>"
//!         .as_slice(),
//! );
//! let CommitHeadOutcome::Committed { meta } =
//!     objects.accept_commit_parts_body(commit.shape(), head, &mut body)?
//! else {
//!     panic!("S3 committed no object");
//! };
//! assert_eq!(meta.e_tag, Some(b"\"c0-1\"".as_slice()));
//! # Ok(())
//! # }
//! ```
//!
//! # Directory buckets
//!
//! A bucket of [`Service::AwsDirectory`] is a directory bucket, as in S3
//! Express One Zone. Its endpoint is the zonal endpoint, such as
//! `https://s3express-euc1-az1.eu-central-1.amazonaws.com`, and its name
//! ends in the zone and `--x-s3`, as in `objects--euc1-az1--x-s3`.
//!
//! It serves its objects to requests signed with the credentials of a
//! session, which it hands out itself:
//!
//! 1. Encode a CreateSession with [`Objects::encode_create_session`], signed
//!    with your own credentials, and send it.
//! 2. Read the head with [`Objects::accept_create_session_head`], and the
//!    body with [`Objects::read_session`].
//! 3. Encode the requests to the objects with the client that
//!    [`Objects::with_session`] returns. Each carries the token in
//!    `x-amz-s3session-token`.
//!
//! AWS ends a session five minutes after it creates it, so create the next
//! one before [`Session::expires_at`].
//!
//! Beside the rules of [`Service::Aws`], the client follows those that AWS
//! documents for a directory bucket:
//!
//! - Requests are signed for the service `s3express`, and go to
//!   virtual-hosted URLs.
//! - A listing prefix ends in `/`. Any other is refused with
//!   [`InvalidPlan::Prefix`].
//! - A directory bucket stores an RFC 2047 encoded word as it is sent. So a
//!   metadata value that a general purpose bucket would store other than as
//!   given, such as one with a line break, is sent as an encoded word rather
//!   than refused.
//! - A commit of parts reports most errors under status 200, such as
//!   `NoSuchUpload` and `PreconditionFailed`.
//!   [`Objects::accept_commit_parts_body`] reads them as the outcomes that
//!   a general purpose bucket answers with. A directory bucket refuses a
//!   commit whose part numbers are not consecutive, such as 1 and 3, with
//!   `InvalidPartOrder`. The client sends such a commit as given.
//! - A copy, [`Objects::encode_copy`] or [`Objects::encode_stage_part_copy`],
//!   is authorized by your own credentials, not by a session's: AWS refuses
//!   the credentials of a session for it. Encode it with the client that
//!   you asked for the session, which signs it for `s3express` too. A
//!   client that holds a session refuses it with [`InvalidPlan::Option`].
//!
//! # Content that is not signed
//!
//! [`PayloadHash::Unsigned`] sends a write without the SHA-256 of its
//! content, so this crate never reads the content. The signature then does
//! not cover the content. TLS still protects the bytes in transit, but
//! nothing checks the bytes that you handed to your HTTP client. Send an MD5
//! of the content in [`WriteOptions::checksum`] to have S3 check them.

use core::cmp::Ordering;

use crate::checksum::{ChecksumKind, ChecksumProvider, KINDS, Sum, check_base64_len};
use crate::common::{
    ContentRange, FailureOutcome, accept_success, condition_header, decimal_header, encoded,
    failure, finish_with_body, meta_of, parse_content_range, push_checksum, text_header,
    validate_checksum, validate_condition, validate_properties, validate_revision, validate_tags,
    write_range, write_tags,
};
use crate::encoding::{self, rfc2047};
use crate::http::PlainHttp;
use crate::request::{ByteSink, HeadWriter, HeaderValue, Pass, U64Decimal};
use crate::sigv4::{self, Credentials, EMPTY_SHA256, MAX_REGION_LEN, Sha256Provider, SigningKey};
use crate::url::{self, Parameter};
use crate::{
    Classification, ConditionKind, ConditionValue, CopySource, DeleteHeadOutcome, DeleteKind,
    DeleteShape, Error, Failure, GetHeadOutcome, GetKind, GetShape, HeaderSpan, InvalidPlan,
    ListEntry, ListHeadOutcome, ListInclude, ListMarker, Listing, MetadataPair, Method, ObjectMeta,
    Payload, PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut, PutHeadOutcome, PutShape,
    RequestedRange, ResponseFault, ResponseHead, Result, Revision, ServiceErrorKind, Tag,
    Timestamps, TransactionalChecksum, WireRequest, WriteOptions,
};

mod batch;
mod copy;
mod parts;
mod restore;
mod tags;

pub use batch::{DeleteResult, MAX_DELETE_KEYS};
pub use copy::PhysicalStagePartCopy;
pub use parts::{
    CreateUploadHeadOutcome, MAX_PART_LEN, MAX_PARTS, MIN_PART_LEN, Part, PartRef,
    PhysicalAbortUpload, PhysicalCreateUpload, PhysicalListParts, PhysicalStagePart,
};

// What `x-amz-content-sha256` carries for content that is not signed.
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

// The value of `x-amz-content-sha256`: the SHA-256 of the content in
// lowercase hexadecimal, or `UNSIGNED-PAYLOAD`.
pub(crate) enum ContentSha256 {
    Hex([u8; 64]),
    Unsigned,
}

impl ContentSha256 {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Hex(hex) => hex,
            Self::Unsigned => UNSIGNED_PAYLOAD.as_bytes(),
        }
    }
}

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
    /// Amazon S3, with a directory bucket, as in S3 Express One Zone.
    ///
    /// See [Directory buckets](self#directory-buckets).
    AwsDirectory = 3,
}

impl Service {
    /// Returns the service with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Aws,
            2 => Self::Compatible,
            3 => Self::AwsDirectory,
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
    scheme: Scheme,
    authority: &'a str,
    name: &'a str,
    region: &'a str,
    addressing: Addressing,
    pub(crate) service: Service,
}

impl<'a> Bucket<'a> {
    /// Creates a bucket reference with path-style addressing, answered by
    /// `service`. A [directory bucket](self#directory-buckets) is addressed
    /// virtual-hosted.
    ///
    /// `endpoint` is the origin of the service, such as
    /// `https://s3.eu-west-1.amazonaws.com`. `region` is the region that the
    /// requests are signed for. A service with no regions of its own names
    /// one in its documentation, often `auto` or `us-east-1`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidEndpoint`] if `endpoint` is not an ASCII HTTPS
    /// origin. An `http://` origin is refused: see [`Self::new_allowing_http`].
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
        Self::create(endpoint, name, region, service, PlainHttp::Refused)
    }

    /// Creates a bucket reference as [`Self::new`] does, from an HTTP origin
    /// as well as an HTTPS one, such as a local emulator's
    /// `http://127.0.0.1:9000`.
    ///
    /// Without TLS, every request travels in clear, with any session token
    /// it carries. SigV4 never sends the secret, but anyone on the path reads
    /// the requests. Use this for a local emulator or a network you trust,
    /// never for a service on the internet.
    ///
    /// # Errors
    ///
    /// As [`Self::new`], except that an `http://` origin is taken.
    pub fn new_allowing_http(
        endpoint: &'a str,
        name: &'a str,
        region: &'a str,
        service: Service,
    ) -> Result<Self> {
        Self::create(endpoint, name, region, service, PlainHttp::Allowed)
    }

    fn create(
        endpoint: &'a str,
        name: &'a str,
        region: &'a str,
        service: Service,
        plain: PlainHttp,
    ) -> Result<Self> {
        if !crate::http::valid_http_origin(endpoint, plain) {
            return Err(Error::InvalidEndpoint);
        }
        let (scheme, authority) = match endpoint.split_once("://") {
            Some(("http", authority)) => (Scheme::Http, authority),
            Some(("https", authority)) => (Scheme::Https, authority),
            _ => return Err(Error::InvalidEndpoint),
        };
        // An HTTP client leaves the default port out of the `host` header,
        // and the signature must cover what the client sends.
        let authority = authority
            .strip_suffix(scheme.default_port())
            .unwrap_or(authority);
        if authority.is_empty() {
            return Err(Error::InvalidEndpoint);
        }
        let name_is_valid = match service {
            Service::Aws => valid_aws_bucket_name(name),
            Service::AwsDirectory => valid_directory_bucket_name(name),
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
            Service::Aws | Service::AwsDirectory => {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
            }
            Service::Compatible => {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_')
            }
        };
        if region.is_empty() || region.len() > MAX_REGION_LEN || !region.bytes().all(region_byte) {
            return Err(Error::InvalidRegion);
        }
        // AWS serves a directory bucket at virtual-hosted URLs alone.
        let addressing = match service {
            Service::AwsDirectory => Addressing::VirtualHosted,
            Service::Aws | Service::Compatible => Addressing::Path,
        };
        Ok(Self {
            scheme,
            authority,
            name,
            region,
            addressing,
            service,
        })
    }

    /// Returns this bucket with `addressing` in place of the addressing that
    /// [`Self::new`] chose.
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
        match self.service {
            Service::AwsDirectory => "s3express",
            Service::Aws | Service::Compatible => "s3",
        }
    }

    // The value of the `host` header that the URL implies.
    fn write_host(&self, out: &mut dyn ByteSink) {
        if self.addressing == Addressing::VirtualHosted {
            out.push(self.name.as_bytes());
            out.push(b".");
        }
        out.push(self.authority.as_bytes());
    }

    // The path of the URL, which the signature covers as it is written. A
    // request to the bucket itself has the path `/` with virtual-hosted
    // addressing and `/bucket` with path-style addressing.
    fn write_path(&self, out: &mut dyn ByteSink, key: Option<&str>) {
        out.push(b"/");
        if self.addressing == Addressing::Path {
            out.push(self.name.as_bytes());
            if key.is_some() {
                out.push(b"/");
            }
        }
        if let Some(key) = key {
            for part in url::encode_object_key(key) {
                out.push(part);
            }
        }
    }
}

// The scheme of an endpoint, which decides its default port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    Http,
    Https,
}

impl Scheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    // The port that a client leaves out of the `host` header, as an
    // authority writes it.
    const fn default_port(self) -> &'static str {
        match self {
            Self::Http => ":80",
            Self::Https => ":443",
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
#[non_exhaustive]
pub enum ObjectProperty {
    /// The storage class, such as `STANDARD` or `GLACIER`.
    StorageClass,
    /// The algorithm of the checksum that S3 keeps for the object, such as
    /// `CRC64NVME`.
    ///
    /// S3's API describes this element as a list, and the value is the
    /// first. Read every one with [`ListEntry::properties`]. An object that
    /// PutObject wrote has one checksum: S3 refuses a write that sends two.
    ChecksumAlgorithm,
    /// Whether that checksum covers the whole object, `FULL_OBJECT`, or is
    /// made of the checksums of its parts, `COMPOSITE`.
    ChecksumType,
    /// The owner, as the bytes between the tags of the `Owner` element. S3
    /// writes it only for [`ListInclude::OWNER`]. Read its `ID` with
    /// [`Metadata::new`](crate::Metadata::new).
    Owner,
    /// The state of a restore from an archive storage class, as the bytes
    /// between the tags of the `RestoreStatus` element. Read what it holds
    /// with [`Metadata::new`](crate::Metadata::new).
    RestoreStatus,
    /// The version that an entry of a listing of versions names. Read it
    /// for any entry with [`ListEntry::version`].
    VersionId,
    /// Whether that version is the latest, `true` or `false`. Read it for
    /// any entry with [`ListEntry::is_current_version`].
    IsLatest,
}

impl ObjectProperty {
    /// Every property, in the order of their numbers.
    pub const ALL: &[Self] = &[
        Self::StorageClass,
        Self::ChecksumAlgorithm,
        Self::ChecksumType,
        Self::Owner,
        Self::RestoreStatus,
        Self::VersionId,
        Self::IsLatest,
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
            Self::VersionId => "VersionId",
            Self::IsLatest => "IsLatest",
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
            5 => Self::VersionId,
            6 => Self::IsLatest,
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

    const fn bit(self) -> u64 {
        1 << (self as u8)
    }
}

// `from_bits` shifts by `COUNT`, which must be below the `u64` shift width.
const _: () = assert!(ObjectProperty::COUNT < 64);

/// The properties that one read of an S3 page is asked for.
///
/// Build one with [`Self::of`] and pass it to [`Objects::fill_listing_with`].
/// The values come back in the order that [`ObjectProperty`] lists them,
/// whatever order the set was built in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PropertySet(u64);

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
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits & ((1 << ObjectProperty::COUNT) - 1))
    }

    /// The set's bits, as [`Self::from_bits`] reads them.
    pub const fn bits(self) -> u64 {
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

/// The credentials of a session with one
/// [directory bucket](self#directory-buckets).
///
/// Only [`Objects::with_session`] takes them. Its
/// [`Debug`](core::fmt::Debug) output shows the key ID and hides the secret
/// and the token.
#[derive(Clone, Copy)]
pub struct Session<'a> {
    /// The access key ID.
    pub key_id: &'a str,
    /// The secret access key.
    pub secret: &'a str,
    /// The token of the session.
    pub token: &'a str,
    /// When the credentials expire, in seconds since the Unix epoch, if the
    /// answer says.
    pub expires_at: Option<u64>,
}

impl core::fmt::Debug for Session<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The result of reading the response head of a CreateSession.
///
/// A head that reports a failure is one of these too.
/// [`Objects::accept_create_session_head`] returns an [`Err`] only for a
/// head it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionHeadOutcome<'h> {
    /// The credentials follow in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`Objects::read_session`].
    Session {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
    /// [`Objects::accept_create_session_error_body`], which returns the final
    /// outcome. If you cannot read the body, pass an empty one and the error
    /// stays unnamed.
    NeedErrorBody(Failure<'h>),
    /// The service refused to create the session, or it failed to.
    ///
    /// A bucket that does not exist is refused here, with
    /// [`ServiceErrorKind::NoSuchContainer`].
    ServiceFailure(Failure<'h>),
}

crate::common::container_failure_outcome!(SessionHeadOutcome<'h>);

impl core::fmt::Display for SessionHeadOutcome<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Session { .. } => f.write_str("the credentials follow in the response body"),
            Self::NeedErrorBody(failure) | Self::ServiceFailure(failure) => {
                core::fmt::Display::fmt(failure, f)
            }
        }
    }
}

/// The S3 operations that one set of credentials authorizes on one bucket.
///
/// This is a small value, and it is [`Copy`]. This crate never reads the
/// clock, so every method that encodes a request takes the current time in
/// `now`. It signs the request for that time.
#[derive(Clone, Copy)]
pub struct Objects<'a> {
    pub(crate) bucket: Bucket<'a>,
    credentials: Credentials<'a>,
    pub(crate) sha256: Sha256Provider,
    signing_key: Option<SigningKey>,
    pub(crate) checksums: [Option<ChecksumProvider>; KINDS],
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
pub(crate) struct Signed<'p> {
    pub(crate) method: Method,
    // The object, or `None` for the bucket itself.
    pub(crate) key: Option<&'p str>,
    // The query, in the order of its names. See `url.rs`.
    pub(crate) query: &'p [Parameter<'p>],
    pub(crate) range: RequestedRange,
    pub(crate) condition: ConditionKind,
    pub(crate) condition_value: Option<ConditionValue<'p>>,
    // Further signed headers, with lowercase names.
    pub(crate) headers: &'p [(&'p str, &'p [u8])],
    pub(crate) metadata: &'p [MetadataPair<'p>],
    pub(crate) content_sha256: &'p [u8],
    // The tags of a write, signed as `x-amz-tagging`.
    pub(crate) tags: &'p [Tag<'p>],
    // The source of a copy, signed with its condition.
    pub(crate) copy: Option<SignedCopy<'p>>,
}

// The source of a copy, signed as `x-amz-copy-source`, and the bytes of it
// that an UploadPartCopy copies, as `x-amz-copy-source-range`.
#[derive(Clone, Copy)]
pub(crate) struct SignedCopy<'p> {
    pub(crate) source: CopySource<'p>,
    pub(crate) range: RequestedRange,
}

// Where the value of a signed header comes from.
#[derive(Clone, Copy)]
enum SignedValue<'a> {
    Bytes(&'a [u8]),
    Host,
    Range,
    Date,
    Tags,
    CopySource,
    CopyRange,
    Condition(ConditionValue<'a>),
}

#[derive(Clone, Copy)]
enum Header<'a> {
    Fixed(&'a str, SignedValue<'a>),
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
    /// then has the encoder compute the checksum: an MD5, sent as
    /// `Content-MD5`, or a CRC-64/NVME, sent as `x-amz-checksum-crc64nvme`.
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
    /// A [`GetKind::Head`] plan becomes a HEAD request. S3 serves every
    /// [`RequestedRange`], including a [`RequestedRange::Suffix`].
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
        let query = [version_parameter(get.revision)];
        let signed = Signed {
            method,
            key: Some(get.key),
            query: &query,
            headers: &[],
            range: get.range,
            condition: get.condition,
            condition_value: get.condition_value,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
            tags: &[],
            copy: None,
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        encoded(head, method, Payload::Slice(&[]))
    }

    /// Writes the signed request head for `put` into `buf`.
    ///
    /// The head states the length of `content`, which stays where you put
    /// it. `hash` says how the request signs the content.
    ///
    /// S3 takes any one checksum in [`WriteOptions::checksum`], and the
    /// content properties, the tags and the storage class there. For
    /// [`Service::Aws`], a condition is either `If-Match` with an entity tag
    /// or `If-None-Match` with `*`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `put` cannot become an S3 request:
    ///
    /// - [`InvalidPlan::PayloadTooLarge`] if `content` is longer than
    ///   [`MAX_PUT_LEN`].
    /// - [`InvalidPlan::Option`] if the plan declares an MD5, or computes a
    ///   checksum whose provider the client has not registered.
    /// - [`InvalidPlan::Checksum`] for a checksum that is not the base64 of
    ///   a digest of its kind.
    /// - [`InvalidPlan::ContentProperty`] or [`InvalidPlan::Tag`] for a
    ///   content property, a storage class or a tag that S3 would not store
    ///   as given.
    /// - [`InvalidPlan::Option`] if `hash` or the checksum is to be computed
    ///   over a [`Payload::Streamed`].
    /// - [`InvalidPlan::Condition`] if `If-None-Match` carries a value other
    ///   than `*`, or for a date condition, for [`Service::Aws`].
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
        let pass = Pass::of(buf);
        let content_sha256 = self.content_sha256(hash, content, pass);
        let mut checksum = [0; CHECKSUM_TEXT_LEN];
        let checksum = self.signed_checksum(
            put.options.checksum,
            |sum| sum.update(content.bytes().unwrap_or_default()),
            pass,
            &mut checksum,
        );
        let (stored, count) = stored_headers(&put.options, checksum);
        let signed = Signed {
            method: Method::Put,
            key: Some(put.key),
            query: &[],
            headers: &stored[..count],
            range: RequestedRange::Whole,
            condition: put.condition,
            condition_value: put.condition_value,
            metadata: put.metadata,
            content_sha256: content_sha256.as_bytes(),
            tags: put.options.tags,
            copy: None,
        };
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        self.push_content(&mut head, content, put.options.checksum);
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
    /// - [`InvalidPlan::Condition`] for any condition but
    ///   [`ConditionKind::IfMatch`], for [`Service::Aws`]. AWS removes on
    ///   `If-Match` only.
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
        let query = [version_parameter(delete.revision)];
        let signed = Signed {
            method: Method::Delete,
            key: Some(delete.key),
            query: &query,
            headers: &[],
            range: RequestedRange::Whole,
            condition: delete.condition,
            condition_value: delete.condition_value,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
            tags: &[],
            copy: None,
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        encoded(head, Method::Delete, Payload::Slice(&[]))
    }

    /// Writes the signed request head of a CreateSession into `buf`, which
    /// asks a [directory bucket](self#directory-buckets) for the credentials
    /// of a session.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] with [`InvalidPlan::Option`] unless the
    /// bucket is of [`Service::AwsDirectory`].
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or
    /// call [`layered::s3::create_session_requirements`](crate::layered::s3::create_session_requirements)
    /// first.
    pub fn encode_create_session<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        match self.bucket.service {
            Service::AwsDirectory => {}
            Service::Aws | Service::Compatible => return Err(InvalidPlan::Option.into()),
        }
        let signed = Signed {
            method: Method::Get,
            key: None,
            query: &[url::literal("session", "")],
            headers: &[],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
            tags: &[],
            copy: None,
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        encoded(head, Method::Get, Payload::Slice(&[]))
    }

    /// Reads the response head of a CreateSession and reports what to do
    /// next.
    ///
    /// A failure is [`SessionHeadOutcome::NeedErrorBody`]: read the body and
    /// pass it to [`Self::accept_create_session_error_body`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 200 is [`ResponseFault::Status`], and a
    /// `Content-Length` that is not a number is [`ResponseFault::Head`].
    pub fn accept_create_session_head<'h>(
        &self,
        head: ResponseHead<'h>,
    ) -> Result<SessionHeadOutcome<'h>> {
        match head.status {
            200 => Ok(SessionHeadOutcome::Session {
                expected_len: decimal_header(head.content_length)?,
            }),
            201..=299 => Err(ResponseFault::Status.into()),
            status => Ok(SessionHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`SessionHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`Self::accept_get_error_body`] for a CreateSession, and reads
    /// the body the same way. A missing bucket is a
    /// [`SessionHeadOutcome::ServiceFailure`] with
    /// [`ServiceErrorKind::NoSuchContainer`].
    pub fn accept_create_session_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> SessionHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Reads the credentials out of the response body of a CreateSession.
    ///
    /// The body is decoded in place, and the credentials borrow it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a `CreateSessionResult` that holds an access key ID, a secret
    /// access key and a session token, or if its expiration is not an
    /// ISO 8601 time in UTC.
    ///
    /// Returns [`Error::Service`] if `body` is an error document, which S3
    /// can send under status 200.
    pub fn read_session<'b>(&self, body: &'b mut [u8]) -> Result<Session<'b>> {
        refuse_error_document(body)?;
        crate::xml::s3::read_session(body)
    }

    /// Returns a client that signs its requests with the credentials of a
    /// session, which [`Self::read_session`] read.
    ///
    /// The client wipes its copy of the secret with the function of this
    /// client's credentials. It has no signing key yet: call
    /// [`Self::with_signing_key`] on it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCredentials`] if [`Credentials::new`] would
    /// refuse the key ID or the secret of `session`, or if the token is not
    /// usable as one HTTP header value.
    pub fn with_session<'s>(self, session: Session<'s>) -> Result<Objects<'s>>
    where
        'a: 's,
    {
        let credentials =
            Credentials::new(session.key_id, session.secret, self.credentials.wipe())?
                .with_s3_session_token(session.token)?;
        Ok(Objects {
            credentials,
            signing_key: None,
            ..self
        })
    }

    // Writes the URL and every signed header. A measuring pass writes a signature
    // of zeros, which is as long as a real one.
    pub(crate) fn write_head(
        &self,
        head: &mut HeadWriter<'_>,
        signed: &Signed<'_>,
        pass: Pass,
        now: &Timestamps,
    ) {
        let signature = if pass == Pass::Measure {
            [b'0'; 64]
        } else {
            self.signature(signed, now)
        };
        head.url(|out| {
            out.push(self.bucket.scheme.as_str().as_bytes());
            out.push(b"://");
            self.bucket.write_host(out);
            self.bucket.write_path(out, signed.key);
            url::write_query_in_url(out, signed.query);
        });
        let token = self.token();
        head.header_with("authorization", |out| {
            out.push(sigv4::ALGORITHM.as_bytes());
            out.push(b" Credential=");
            out.push(self.credentials.key_id().as_bytes());
            out.push(b"/");
            self.write_scope(out, now);
            out.push(b", SignedHeaders=");
            write_signed_names(out, signed, token);
            out.push(b", Signature=");
            out.push(&signature);
        });
        for header in ordered_headers(signed, token) {
            match header {
                Header::Fixed(_, SignedValue::Host) => {}
                Header::Fixed(name, value) => head.header_with(name, |out| match value {
                    SignedValue::Bytes(bytes) => out.push(bytes),
                    SignedValue::Condition(value) => value.write_to(out),
                    SignedValue::Range => write_range(out, signed.range),
                    SignedValue::Date => out.push(now.iso8601().as_bytes()),
                    SignedValue::Tags => write_tags(out, signed.tags),
                    SignedValue::CopySource => self.write_copy_source(out, signed),
                    SignedValue::CopyRange => write_copy_range(out, signed),
                    SignedValue::Host => {}
                }),
                Header::Meta(pair) => head.header_parts(
                    |out| {
                        out.push(METADATA_PREFIX.as_bytes());
                        for byte in pair.name.bytes() {
                            out.push(&[byte.to_ascii_lowercase()]);
                        }
                    },
                    |out| write_metadata_value(out, pair.value, self.bucket.service),
                ),
            }
        }
    }

    // The value of `x-amz-copy-source`: the source's bucket, or the client's
    // own, the key encoded as a path, and the version.
    fn write_copy_source(&self, out: &mut dyn ByteSink, signed: &Signed<'_>) {
        let Some(copy) = signed.copy else { return };
        out.push(b"/");
        out.push(copy.source.container.unwrap_or(self.bucket.name).as_bytes());
        out.push(b"/");
        for part in url::encode_object_key(copy.source.key) {
            out.push(part);
        }
        url::write_query_in_url(out, &[version_parameter(copy.source.revision)]);
    }

    // The session token and the header that carries it.
    fn token(&self) -> Option<(&'static str, &'a str)> {
        self.credentials.token().header()
    }

    // The text of `x-amz-content-sha256` for `content`. A measuring pass returns no
    // request, so it does not read the content: the digest is the same
    // length whatever it is.
    pub(crate) fn content_sha256(
        &self,
        hash: PayloadHash,
        content: Payload<'_>,
        pass: Pass,
    ) -> ContentSha256 {
        match hash {
            PayloadHash::Unsigned => ContentSha256::Unsigned,
            PayloadHash::Sha256(digest) => ContentSha256::Hex(encoding::hex(&digest)),
            PayloadHash::Compute if pass == Pass::Measure => ContentSha256::Hex([b'0'; 64]),
            PayloadHash::Compute => ContentSha256::Hex(encoding::hex(
                &self.sha256.hash(content.bytes().unwrap_or_default()),
            )),
        }
    }

    // The headers that describe the content of a write, after the signed
    // ones: its length, and its MD5 if the plan carries one. Any other
    // checksum is an `x-amz-` header, which the request signs.
    pub(crate) fn push_content(
        &self,
        head: &mut HeadWriter<'_>,
        content: Payload<'_>,
        checksum: Option<TransactionalChecksum<'_>>,
    ) {
        head.header("content-length", U64Decimal::new(content.len()).as_bytes());
        let md5 = checksum.filter(|checksum| {
            matches!(
                checksum,
                TransactionalChecksum::Md5(_) | TransactionalChecksum::Compute(ChecksumKind::Md5)
            )
        });
        push_checksum(head, md5, &self.checksums, |sum| {
            sum.update(content.bytes().unwrap_or_default());
        });
    }

    // The `x-amz-checksum-` header of a write and its value, written into
    // `into`, for a checksum other than an MD5. `content` feeds the content
    // to a computed checksum. A measuring pass computes nothing: the text is as
    // long whatever it is.
    pub(crate) fn signed_checksum<'x>(
        &self,
        checksum: Option<TransactionalChecksum<'x>>,
        content: impl FnOnce(&mut Sum),
        pass: Pass,
        into: &'x mut [u8; CHECKSUM_TEXT_LEN],
    ) -> Option<(&'static str, &'x [u8])> {
        Some(match checksum? {
            TransactionalChecksum::Crc64(text) => (CRC64_HEADER, text.as_bytes()),
            TransactionalChecksum::Crc32(text) => ("x-amz-checksum-crc32", text.as_bytes()),
            TransactionalChecksum::Crc32c(text) => ("x-amz-checksum-crc32c", text.as_bytes()),
            TransactionalChecksum::Sha1(text) => ("x-amz-checksum-sha1", text.as_bytes()),
            TransactionalChecksum::Sha256(text) => ("x-amz-checksum-sha256", text.as_bytes()),
            TransactionalChecksum::Compute(ChecksumKind::Md5) => return None,
            TransactionalChecksum::Compute(kind) => {
                let len = kind.digest_len();
                let mut bytes = [0; 32];
                if pass == Pass::Write
                    && let Some(provider) = &self.checksums[kind.slot()]
                {
                    let mut sum = provider.start();
                    content(&mut sum);
                    bytes[..len].copy_from_slice(sum.finish().as_bytes());
                }
                // A digest holds a CRC-64 in the little-endian order that
                // Azure reads, and S3 reads it big-endian.
                if kind == ChecksumKind::Crc64 {
                    bytes[..len].reverse();
                }
                let text_len = len.div_ceil(3) * 4;
                let text = encoding::base64_into(&bytes[..len], &mut into[..text_len]);
                let name = match kind {
                    ChecksumKind::Crc64 => CRC64_HEADER,
                    kind => kind.header(),
                };
                (name, text.as_bytes())
            }
            _ => return None,
        })
    }

    fn write_scope(&self, out: &mut dyn ByteSink, now: &Timestamps) {
        out.push(now.date().as_bytes());
        out.push(b"/");
        out.push(self.bucket.region.as_bytes());
        out.push(b"/");
        out.push(self.bucket.signing_service().as_bytes());
        out.push(b"/aws4_request");
    }

    // Returns the signature of the request, as lowercase hexadecimal. The
    // canonical request is hashed as it is written, so it is never held.
    fn signature(&self, signed: &Signed<'_>, now: &Timestamps) -> [u8; 64] {
        let mut sum = self.sha256.start();
        self.write_canonical_request(&mut sum, signed, now);
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
        out: &mut dyn ByteSink,
        signed: &Signed<'_>,
        now: &Timestamps,
    ) {
        out.push(signed.method.as_str().as_bytes());
        out.push(b"\n");
        self.bucket.write_path(out, signed.key);
        out.push(b"\n");
        // The URL carries the query in canonical form, so this is the same
        // text.
        url::write_query(out, signed.query);
        out.push(b"\n");
        let token = self.token();
        for header in ordered_headers(signed, token) {
            write_header_name(out, header);
            out.push(b":");
            match header {
                Header::Fixed(_, SignedValue::Bytes(bytes)) => write_canonical_value(out, bytes),
                Header::Fixed(_, SignedValue::Condition(ConditionValue::ETag(tag))) => {
                    write_canonical_value(out, tag);
                }
                // An HTTP date has no space at either end, nor a run of them.
                Header::Fixed(_, SignedValue::Condition(value)) => value.write_to(out),
                Header::Fixed(_, SignedValue::Host) => self.bucket.write_host(out),
                Header::Fixed(_, SignedValue::Range) => write_range(out, signed.range),
                Header::Fixed(_, SignedValue::Date) => out.push(now.iso8601().as_bytes()),
                // The encoded form holds no space, so it is its canonical form.
                Header::Fixed(_, SignedValue::Tags) => write_tags(out, signed.tags),
                Header::Fixed(_, SignedValue::CopySource) => self.write_copy_source(out, signed),
                Header::Fixed(_, SignedValue::CopyRange) => write_copy_range(out, signed),
                Header::Meta(pair) if encodes(pair.value, self.bucket.service) => {
                    write_metadata_value(out, pair.value, self.bucket.service)
                }
                Header::Meta(pair) => write_canonical_value(out, pair.value.as_bytes()),
            }
            out.push(b"\n");
        }
        out.push(b"\n");
        write_signed_names(out, signed, token);
        out.push(b"\n");
        out.push(signed.content_sha256);
    }

    /// Reads the response head of a GET or a HEAD and reports what to do
    /// next.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_get`]. A
    /// failure of a GET is [`GetHeadOutcome::NeedErrorBody`]: read the body
    /// and pass it to [`Self::accept_get_error_body`]. A failure of a HEAD is
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
        // AWS states the length of every response that it serves.
        let states_length = match self.bucket.service {
            Service::Aws | Service::AwsDirectory => true,
            Service::Compatible => false,
        };
        match head.status {
            206 if !ranged => Err(ResponseFault::Range.into()),
            200 if ranged => Err(ResponseFault::Range.into()),
            200 | 206 if states_length && head.content_length.is_none() => {
                Err(ResponseFault::Head.into())
            }
            200 | 206 => accept_success(shape, head),
            304 if !shape.condition.fails_as_not_modified() => Err(ResponseFault::Status.into()),
            304 => Ok(GetHeadOutcome::NotModified { e_tag: head.e_tag }),
            412 if shape.condition == ConditionKind::None
                || shape.condition.fails_as_not_modified() =>
            {
                Err(ResponseFault::Status.into())
            }
            412 => Ok(GetHeadOutcome::PreconditionFailed),
            416 => Ok(GetHeadOutcome::RangeNotSatisfiable {
                object_size: match head.content_range.map(parse_content_range) {
                    Some(Some(ContentRange::Unsatisfied { total })) => total,
                    None => None,
                    Some(_) => return Err(ResponseFault::Head.into()),
                },
            }),
            200..=299 => Err(ResponseFault::Status.into()),
            // A HEAD response has no body to name the error, so S3's bare 404
            // for a missing bucket reads as a missing key here.
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
    /// Pass the `shape` that you passed to [`Self::accept_get_head`], the
    /// [`Failure`] of that outcome, and the body that you
    /// read. Pass an empty body if you could not read one: the outcome is
    /// then final with the error unnamed.
    ///
    /// S3 reports a failed condition in the head, so this method reads no
    /// part of `shape`.
    pub fn accept_get_error_body<'h>(
        &self,
        shape: GetShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> GetHeadOutcome<'h> {
        let _ = shape;
        finish_with_body(failure, body_kind(body))
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
                    last_modified: text_header(head.last_modified)?,
                    ..meta_of(head)
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
    /// This is [`Self::accept_get_error_body`] for a write, and reads the
    /// body the same way. A missing bucket is a
    /// [`PutHeadOutcome::ServiceFailure`] with
    /// [`ServiceErrorKind::NoSuchContainer`].
    pub fn accept_put_error_body<'h>(
        &self,
        shape: PutShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> PutHeadOutcome<'h> {
        let _ = shape;
        finish_with_body(failure, body_kind(body))
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
        // Some services that implement the S3 API answer 200.
        let accepts_200 = match self.bucket.service {
            Service::Compatible => true,
            Service::Aws | Service::AwsDirectory => false,
        };
        match head.status {
            204 => Ok(DeleteHeadOutcome::Accepted),
            200 if accepts_200 => Ok(DeleteHeadOutcome::Accepted),
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
    /// This is [`Self::accept_get_error_body`] for a removal, and reads the
    /// body the same way. A 404 names a missing bucket, or a missing object
    /// under an `If-Match` condition.
    pub fn accept_delete_error_body<'h>(
        &self,
        shape: DeleteShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> DeleteHeadOutcome<'h> {
        let _ = shape;
        finish_with_body(failure, body_kind(body))
    }

    /// Writes the signed request head for one page of `list` into `buf`.
    ///
    /// The request is a ListObjectsV2. Read the whole response body and pass
    /// it to [`Self::fill_listing`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `list` cannot become an S3 request:
    ///
    /// - [`InvalidPlan::Marker`] for an empty marker.
    /// - [`InvalidPlan::Option`] for [`ListInclude::METADATA`], which S3 does
    ///   not list.
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
        validate_list(list, self.bucket.service)?;
        // An empty prefix is no parameter at all.
        let prefix = Some(list.prefix).filter(|prefix| !prefix.is_empty());
        // Validation matched the kind of marker to the listing.
        let (key_marker, version_marker) = match list.marker {
            Some(ListMarker::Version { key, version }) => (Some(key), version),
            _ => (None, None),
        };
        let token = list.marker.and_then(ListMarker::text);
        // SigV4 signs the parameters in the order of their names. With
        // `encoding-type=url`, a key that XML cannot carry still arrives.
        let versions = [
            url::encoded("delimiter", list.delimiter),
            url::literal("encoding-type", "url"),
            url::encoded("key-marker", key_marker),
            url::number("max-keys", list.max_results),
            url::encoded("prefix", prefix),
            url::encoded("version-id-marker", version_marker),
            url::literal("versions", ""),
        ];
        let objects = [
            url::encoded("continuation-token", token),
            url::encoded("delimiter", list.delimiter),
            url::literal("encoding-type", "url"),
            url::literal("fetch-owner", "true")
                .filter(|_| list.include.contains(ListInclude::OWNER)),
            url::literal("list-type", "2"),
            url::number("max-keys", list.max_results),
            url::encoded("prefix", prefix),
            url::encoded(
                "start-after",
                list.start_after.filter(|key| !key.is_empty()),
            ),
        ];
        let query: &[Parameter<'_>] = if list.include.contains(ListInclude::VERSIONS) {
            &versions
        } else {
            &objects
        };
        let signed = Signed {
            method: Method::Get,
            key: None,
            query,
            headers: &[],
            range: RequestedRange::Whole,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            content_sha256: EMPTY_SHA256.as_bytes(),
            tags: &[],
            copy: None,
        };
        let pass = Pass::of(buf);
        let mut head = HeadWriter::new(buf, headers);
        self.write_head(&mut head, &signed, pass, now);
        encoded(head, Method::Get, Payload::Slice(&[]))
    }

    /// Reads the response head of a listing and reports what S3 did.
    ///
    /// S3 often sends a page without `Content-Length`, so cap what you read.
    /// A failure is [`ListHeadOutcome::NeedErrorBody`]: read the body and
    /// pass it to [`Self::accept_list_error_body`].
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
    /// This is [`Self::accept_get_error_body`] for a listing, and reads the
    /// body the same way. A missing bucket is a
    /// [`ListHeadOutcome::ServiceFailure`] with
    /// [`ServiceErrorKind::NoSuchContainer`].
    pub fn accept_list_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> ListHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
    }

    /// Reads a page out of the response body of a listing.
    ///
    /// Reading is destructive, and your array must hold the whole page. An
    /// array of 1,000 entries holds any page from AWS.
    ///
    /// AWS writes all objects of a page before its groups of keys. An
    /// object's entity tag keeps its quotes. Read its date with
    /// [`layered::iso8601_ms`](crate::layered::iso8601_ms).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capacity`] if the page holds more entries than the
    /// array, with `required` set to the number it holds. Ask the service for
    /// the page again, with a larger array.
    ///
    /// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
    /// not a ListObjectsV2 page, or if the page contradicts itself. A page
    /// that does not say it URL-encoded its keys is refused if decoding
    /// changes a key.
    ///
    /// Returns [`Error::Service`] if `body` is an error document, which S3
    /// can send under status 200.
    pub fn fill_listing<'b, E: From<ListEntry<'b>>>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
    ) -> Result<Listing<'b>> {
        refuse_error_document(body)?;
        crate::xml::s3::fill_listing(body, into, PropertySet::default(), |entry, _| entry.into())
    }

    /// Reads a page the way [`Self::fill_listing`] does, and hands you the
    /// values of the properties in `wanted` as it goes.
    ///
    /// What `build` returns for each entry is written into your array.
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
        refuse_error_document(body)?;
        crate::xml::s3::fill_listing(body, into, wanted, build)
    }
}

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

pub(crate) fn body_kind(body: &[u8]) -> Option<ServiceErrorKind> {
    crate::xml::error_code(body).and_then(|code| kind_for_code(code.as_bytes()))
}

// Returns whether `body` is an error document rather than the result of a
// request. S3 can send one under status 200.
pub(crate) fn is_error_document(body: &[u8]) -> bool {
    crate::xml::root_is(body, b"Error")
}

// Returns the error that `body` names if it is an error document, so that a
// method that reads a result does not report it as a malformed result.
pub(crate) fn refuse_error_document(body: &[u8]) -> Result<()> {
    if is_error_document(body) {
        return Err(Error::Service(body_kind(body)));
    }
    Ok(())
}

fn kind_for_code(code: &[u8]) -> Option<ServiceErrorKind> {
    Some(match code {
        b"NoSuchKey" => ServiceErrorKind::NotFound,
        b"NoSuchBucket" => ServiceErrorKind::NoSuchContainer,
        b"NoSuchUpload" => ServiceErrorKind::NoSuchUpload,
        b"InvalidPart" | b"InvalidPartOrder" | b"EntityTooSmall" => ServiceErrorKind::InvalidUpload,
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
    let mut fixed = [Header::Fixed("", SignedValue::Host); 24];
    let mut count = 0;
    let entries = [
        Some(("host", SignedValue::Host)),
        Some((
            "x-amz-content-sha256",
            SignedValue::Bytes(signed.content_sha256),
        )),
        Some(("x-amz-date", SignedValue::Date)),
        token.map(|(name, token)| (name, SignedValue::Bytes(token.as_bytes()))),
        (signed.range != RequestedRange::Whole).then_some(("range", SignedValue::Range)),
        condition_header(signed.condition)
            .zip(signed.condition_value)
            .map(|(name, value)| (name, SignedValue::Condition(value))),
        (!signed.tags.is_empty()).then_some(("x-amz-tagging", SignedValue::Tags)),
        signed
            .copy
            .map(|_| ("x-amz-copy-source", SignedValue::CopySource)),
        signed
            .copy
            .filter(|copy| copy.range != RequestedRange::Whole)
            .map(|_| ("x-amz-copy-source-range", SignedValue::CopyRange)),
        signed.copy.and_then(|copy| {
            let name = match copy.source.condition {
                ConditionKind::None => return None,
                ConditionKind::IfMatch => "x-amz-copy-source-if-match",
                ConditionKind::IfNoneMatch => "x-amz-copy-source-if-none-match",
                ConditionKind::IfModifiedSince => "x-amz-copy-source-if-modified-since",
                ConditionKind::IfUnmodifiedSince => "x-amz-copy-source-if-unmodified-since",
            };
            Some((name, SignedValue::Condition(copy.source.condition_value?)))
        }),
    ]
    .into_iter()
    .flatten()
    .chain(
        signed
            .headers
            .iter()
            .map(|(name, value)| (*name, SignedValue::Bytes(value))),
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

fn write_header_name(out: &mut dyn ByteSink, header: Header<'_>) {
    match header {
        Header::Fixed(name, _) => out.push(name.as_bytes()),
        Header::Meta(pair) => {
            for byte in metadata_header_name(pair) {
                out.push(&[byte]);
            }
        }
    }
}

// The names of the signed headers, in order, separated by `;`.
fn write_signed_names(
    out: &mut dyn ByteSink,
    signed: &Signed<'_>,
    token: Option<(&'static str, &str)>,
) {
    for (index, header) in ordered_headers(signed, token).enumerate() {
        if index != 0 {
            out.push(b";");
        }
        write_header_name(out, header);
    }
}

// SigV4 signs a header value without the spaces at either end, and with each
// run of spaces inside it as one space. A value sent as it is holds no other
// whitespace.
fn write_canonical_value(out: &mut dyn ByteSink, value: &[u8]) {
    let value = value.trim_ascii();
    let mut start = 0;
    let mut at = 0;
    while at < value.len() {
        if value[at] == b' ' {
            out.push(&value[start..=at]);
            while at < value.len() && value[at] == b' ' {
                at += 1;
            }
            start = at;
        } else {
            at += 1;
        }
    }
    out.push(&value[start..]);
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

// The naming rules that AWS documents for directory buckets: a base name,
// the zone ID and `--x-s3`, in 3 to 63 lowercase letters, digits and `-`.
// The base name starts with a letter or a digit.
fn valid_directory_bucket_name(name: &str) -> bool {
    let Some((base, zone)) = name
        .strip_suffix("--x-s3")
        .and_then(|rest| rest.rsplit_once("--"))
    else {
        return false;
    };
    (3..=63).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && base
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && zone
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
}

// Four groups of one to three digits, separated by dots.
fn looks_like_ipv4(name: &str) -> bool {
    name.split('.').count() == 4
        && name.split('.').all(|group| {
            (1..=3).contains(&group.len()) && group.bytes().all(|byte| byte.is_ascii_digit())
        })
}

pub(crate) fn validate_key(key: &str) -> Result<()> {
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
    validate_revision(get.revision, false)?;
    validate_condition(get.condition, get.condition_value)
}

// The value of `x-amz-copy-source-range`, which names its last byte as
// `Range` does.
fn write_copy_range(out: &mut dyn ByteSink, signed: &Signed<'_>) {
    if let Some(copy) = signed.copy {
        write_range(out, copy.range);
    }
}

// The query parameter that names a version, or none for the object as it is
// now. Validation refused a snapshot.
pub(crate) fn version_parameter(revision: Option<Revision<'_>>) -> Parameter<'_> {
    match revision? {
        Revision::Version(id) => url::encoded("versionId", id),
        Revision::Snapshot(_) => None,
    }
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
    validate_content(
        &put.options,
        content,
        hash,
        checksums,
        Stores::Object(service),
    )?;
    validate_write_condition(put.condition, put.condition_value, service)
}

// The writes that take options. A write of a whole object stores the
// content properties, the tags and the storage class of the object, under
// the rules of a service, and takes any checksum. A part, staged or
// committed, takes none of those, and an MD5 alone.
#[derive(Clone, Copy)]
pub(crate) enum Stores {
    Object(Service),
    Part,
}

// Checks what a write sends beside its content. S3 stores no declared MD5.
// A checksum or a SHA-256 that the encoder computes needs the bytes.
pub(crate) fn validate_content(
    options: &WriteOptions<'_>,
    content: Payload<'_>,
    hash: PayloadHash,
    checksums: &[Option<ChecksumProvider>; KINDS],
    stores: Stores,
) -> Result<()> {
    let has_bytes = content.bytes().is_some();
    if options.declared_md5.is_some() || (hash == PayloadHash::Compute && !has_bytes) {
        return Err(InvalidPlan::Option.into());
    }
    let md5_or_none = matches!(
        options.checksum,
        None | Some(
            TransactionalChecksum::Md5(_) | TransactionalChecksum::Compute(ChecksumKind::Md5)
        )
    );
    if matches!(stores, Stores::Part) && !md5_or_none {
        return Err(InvalidPlan::Option.into());
    }
    validate_s3_checksum(options.checksum, has_bytes, checksums)?;
    match stores {
        Stores::Object(service) => {
            // A general purpose bucket returns header bytes as it got
            // them. A directory bucket refuses a byte outside ASCII with
            // 400 InvalidRequest.
            let utf8: &[&str] = match service {
                Service::Aws | Service::Compatible => &["content-disposition", "content-type"],
                Service::AwsDirectory => &[],
            };
            validate_properties(options, utf8)?;
            let limits = match service {
                Service::Aws | Service::AwsDirectory => Some((10, 128, 256)),
                Service::Compatible => None,
            };
            validate_tags(options.tags, s3_tag_char, limits)
        }
        Stores::Part
            if !options.properties.is_empty()
                || !options.tags.is_empty()
                || options.storage_class.is_some() =>
        {
            Err(InvalidPlan::Option.into())
        }
        Stores::Part => Ok(()),
    }
}

// Checks a checksum of S3's: one that a write signs as text, or one that the
// encoder computes, an MD5 or a CRC-64/NVME.
pub(crate) fn validate_s3_checksum(
    checksum: Option<TransactionalChecksum<'_>>,
    has_bytes: bool,
    checksums: &[Option<ChecksumProvider>; KINDS],
) -> Result<()> {
    let len = match checksum {
        Some(TransactionalChecksum::Crc32(text) | TransactionalChecksum::Crc32c(text)) => (text, 4),
        Some(TransactionalChecksum::Sha1(text)) => (text, 20),
        Some(TransactionalChecksum::Sha256(text)) => (text, 32),
        _ => return validate_checksum(checksum, has_bytes, checksums),
    };
    check_base64_len(len.0, len.1)
}

// The characters that S3 takes in the key and the value of a tag.
pub(crate) fn s3_tag_char(character: char) -> bool {
    character.is_alphanumeric() || " +-=._:/@".contains(character)
}

// The header that carries a CRC-64/NVME on S3.
const CRC64_HEADER: &str = "x-amz-checksum-crc64nvme";

// The longest text of a checksum that a write signs: the base64 of a
// SHA-256.
pub(crate) const CHECKSUM_TEXT_LEN: usize = 44;

// The headers that a write signs beside the client's own: the content
// properties, the storage class and a checksum other than an MD5.
pub(crate) fn stored_headers<'a>(
    options: &WriteOptions<'a>,
    checksum: Option<(&'static str, &'a [u8])>,
) -> ([(&'static str, &'a [u8]); 7], usize) {
    let mut headers = [("", &[][..]); 7];
    let mut count = 0;
    let properties = options
        .properties
        .iter()
        .map(|(name, value)| (name, value.as_bytes()));
    let class = options
        .storage_class
        .map(|class| ("x-amz-storage-class", class.as_bytes()));
    for header in properties.chain(class).chain(checksum) {
        headers[count] = header;
        count += 1;
    }
    (headers, count)
}

// Checks the condition of a write: a whole object, or a commit of parts.
pub(crate) fn validate_write_condition(
    condition: ConditionKind,
    value: Option<ConditionValue<'_>>,
    service: Service,
) -> Result<()> {
    validate_condition(condition, value)?;
    // AWS writes on `If-None-Match` only if no object holds the key, and takes
    // no value but `*`. It takes no date condition on a write.
    let aws = match service {
        Service::Aws | Service::AwsDirectory => true,
        Service::Compatible => false,
    };
    if aws
        && (condition.is_date()
            || (condition == ConditionKind::IfNoneMatch
                && value != Some(ConditionValue::ETag(b"*"))))
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
    validate_revision(delete.revision, false)?;
    // AWS removes on `If-Match` alone.
    let if_match_only = match service {
        Service::Aws | Service::AwsDirectory => true,
        Service::Compatible => false,
    };
    if if_match_only
        && !matches!(
            delete.condition,
            ConditionKind::None | ConditionKind::IfMatch
        )
    {
        return Err(InvalidPlan::Condition.into());
    }
    validate_condition(delete.condition, delete.condition_value)
}

// A prefix is not a key, so `validate_key` does not apply. S3 takes any
// number of entries, zero included.
fn validate_list(list: &PhysicalList<'_>, service: Service) -> Result<()> {
    // A listing continues from the kind of marker that its pages hand out,
    // and S3 hands out no empty one: a version marker names a key.
    let versions = list.include.contains(ListInclude::VERSIONS);
    match list.marker {
        None => {}
        Some(ListMarker::Text(token)) if !versions && !token.is_empty() => {}
        Some(ListMarker::Version { key, version })
            if versions && !key.is_empty() && version.is_none_or(|version| !version.is_empty()) => {
        }
        Some(_) => return Err(InvalidPlan::Marker.into()),
    }
    // A general purpose bucket groups keys at any text. A directory bucket
    // groups them at `/` alone, lists only at a prefix that ends in it, and
    // answers anything else with 400 `InvalidRequest`.
    let slash_only = match service {
        Service::AwsDirectory => true,
        Service::Aws | Service::Compatible => false,
    };
    if let Some(delimiter) = list.delimiter
        && (delimiter.is_empty() || (slash_only && delimiter != "/"))
    {
        return Err(InvalidPlan::Delimiter.into());
    }
    if slash_only && !list.prefix.is_empty() && !list.prefix.ends_with('/') {
        return Err(InvalidPlan::Prefix.into());
    }
    // S3 keeps no snapshots, and lists no metadata.
    if list
        .include
        .intersects(ListInclude::METADATA | ListInclude::SNAPSHOTS)
    {
        return Err(InvalidPlan::Option.into());
    }
    if list.include.contains(ListInclude::VERSIONS) {
        // A directory bucket keeps no versions, and a ListObjectVersions
        // starts at its markers alone.
        if slash_only || list.start_after.is_some_and(|key| !key.is_empty()) {
            return Err(InvalidPlan::Option.into());
        }
    }
    Ok(())
}

// S3 sends a metadata pair as an `x-amz-meta-` header, so the name must be a
// token. `write_metadata_value` writes the value, and S3 matches a name
// without case.
pub(crate) fn validate_metadata(metadata: &[MetadataPair<'_>], service: Service) -> Result<()> {
    for (index, pair) in metadata.iter().enumerate() {
        if pair.name.is_empty() || !pair.name.bytes().all(token_byte) {
            return Err(InvalidPlan::MetadataName.into());
        }
        // An empty value is a pair with no text, which S3 stores. A value is
        // refused if a read would not return it exactly. A general purpose
        // bucket stores CR and LF as spaces, even from an encoded word. It
        // returns an ASCII value unencoded, so HTTP drops the whitespace at
        // either end, and a token that reads as an encoded word is decoded. A
        // directory bucket stores an encoded word as it is sent, so
        // `encodes` sends each of these values as one.
        if !stores_encoded_words(service) && general_purpose_changes(pair.value) {
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
    let limited = match service {
        Service::Aws | Service::AwsDirectory => true,
        Service::Compatible => false,
    };
    if limited && len > MAX_METADATA_LEN {
        return Err(InvalidPlan::MetadataTooLarge.into());
    }
    Ok(())
}

// Returns whether a write sends `value` as an encoded word. A header value
// cannot hold text outside ASCII or a control character other than a tab,
// and S3 returns such a value encoded. A tab is encoded too, because the
// canonical form of SigV4 folds spaces alone. S3 returns a tab as it is.
//
// A directory bucket stores an encoded word as it is sent, and a read
// decodes it, so the values that a general purpose bucket would change are
// encoded for it as well.
fn encodes(value: &str, service: Service) -> bool {
    !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_control())
        || (stores_encoded_words(service) && general_purpose_changes(value))
}

// Whether the service stores an RFC 2047 encoded word as it is sent, rather
// than decoding it.
const fn stores_encoded_words(service: Service) -> bool {
    match service {
        Service::AwsDirectory => true,
        Service::Aws | Service::Compatible => false,
    }
}

// Whether a general purpose bucket would store `value` other than as it is
// given, sent as it is or as an encoded word: CR and LF, whitespace at
// either end, or text that reads as an encoded word.
fn general_purpose_changes(value: &str) -> bool {
    let edge = |byte: Option<&u8>| matches!(byte, Some(b' ' | b'\t'));
    let bytes = value.as_bytes();
    value.contains(['\r', '\n'])
        || edge(bytes.first())
        || edge(bytes.last())
        || rfc2047::looks_encoded(value)
}

// Writes a metadata value as the header carries it, encoded or as it is. An
// encoded word holds no space, so its canonical form for SigV4 is the same.
fn write_metadata_value(out: &mut dyn ByteSink, value: &str, service: Service) {
    if encodes(value, service) {
        rfc2047::write(out, value);
    } else {
        out.push(value.as_bytes());
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

    use super::{ByteSink, MetadataPair, sorted_metadata, write_canonical_value};

    // Collects what a writer writes, for a test to compare.
    struct Collected(Vec<u8>);

    impl ByteSink for Collected {
        fn push(&mut self, bytes: &[u8]) {
            self.0.extend_from_slice(bytes);
        }
    }

    fn canonical(value: &str) -> Vec<u8> {
        let mut out = Collected(Vec::new());
        write_canonical_value(&mut out, value.as_bytes());
        out.0
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
