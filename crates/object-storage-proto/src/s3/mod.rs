//! S3 requests and responses, signed with AWS Signature Version 4.
//!
//! # How a request works
//!
//! 1. Create a [`Bucket`] and [`Credentials`]. The
//!    `borink-object-storage-crypto` crate has the function that
//!    [`Credentials::new`] takes to wipe its copy of the secret.
//! 2. Describe what every request shares, as [`GetOptions`] or
//!    [`PutOptions`], and call [`prepare_get`] or [`prepare_put`] to write
//!    a [`Template`] into a buffer that you own.
//! 3. Describe each request, as a [`GetItem`] or a [`PutItem`], and call
//!    [`encode_get`] or [`encode_put`] to write it into a second buffer. The request is
//!    [`Unsigned`]: sign it, as [Signing](#signing) describes, and send the
//!    [`WireRequest`](crate::WireRequest) that [`set_signature`] returns with your
//!    HTTP client.
//! 4. Put the response headers into a [`ResponseHead`] and call
//!    [`accept_get_head`] or [`accept_put_head`].
//!
//! S3 names an error in the response body, not in a header. A failure of a
//! GET, a PUT, a DELETE or a listing is therefore a `NeedErrorBody` outcome.
//! Read the body and pass it to the `accept_*_error_body` function of the
//! same operation. A HEAD response has no body, and its outcome is final.
//!
//! # Example
//!
//! ```
//! use borink_object_storage_proto::s3::{self, Bucket, PayloadSigning, Service};
//! use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
//! use borink_object_storage_proto::{
//!     GetHeadOutcome, GetItem, GetOptions, Payload, PutHeadOutcome, PutItem, PutOptions,
//!     ResponseHead, Timestamps,
//! };
//! # fn hmac_of_string_to_sign(_canonical: &[u8]) -> [u8; 32] {
//! #     [0; 32]
//! # }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bucket = Bucket::new(
//!     "https://s3.eu-west-1.amazonaws.com", "objects", "eu-west-1", Service::Aws,
//! )?;
//! // Pass `borink_object_storage_crypto::wipe` in place of `wipe_best_effort`.
//! let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe_best_effort)?;
//! let now = Timestamps::from_unix(1_787_400_000);
//!
//! let options = PutOptions::default();
//! let mut template = vec![0; 1024];
//! let writes =
//!     s3::prepare_put(&bucket, &credentials, &options, PayloadSigning::Unsigned, &mut template)?;
//!
//! let put = PutItem::new("directory/object.txt", Payload::Slice(b"contents"));
//! let mut buffer = vec![0; 2048];
//! let unsigned = s3::encode_put(&writes, &template, &put, None, &now, &mut buffer)?;
//! // `borink_object_storage_crypto::sigv4::sign` computes the HMAC and finishes.
//! let mac = hmac_of_string_to_sign(unsigned.canonical(&buffer));
//! let request = s3::set_signature(&unsigned, &mut buffer, &mac);
//! assert_eq!(
//!     request.url(),
//!     "https://s3.eu-west-1.amazonaws.com/objects/directory/object.txt"
//! );
//!
//! let head = ResponseHead::from_headers(200, [("ETag", b"\"tag\"".as_slice())]);
//! assert!(matches!(
//!     s3::accept_put_head(put.shape(&options), head)?,
//!     PutHeadOutcome::Created { .. }
//! ));
//!
//! let options = GetOptions::new();
//! let mut template = vec![0; 1024];
//! let reads = s3::prepare_get(&bucket, &credentials, &options, &mut template)?;
//! let get = GetItem::new("directory/object.txt");
//! let mut buffer = vec![0; 2048];
//! let unsigned = s3::encode_get(&reads, &template, &get, &now, &mut buffer)?;
//! # let _ = unsigned;
//! let head = ResponseHead::from_headers(404, [("x-amz-request-id", b"4442587FB7D0A2F9".as_slice())]);
//! let GetHeadOutcome::NeedErrorBody(failure) =
//!     s3::accept_get_head(reads.service(), get.shape(&options), head)?
//! else {
//!     panic!("S3 names the error in the body");
//! };
//! let body = b"<Error><Code>NoSuchKey</Code></Error>";
//! assert!(matches!(
//!     s3::accept_get_error_body(get.shape(&options), failure, body),
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
//! Every request is signed for the bucket's region, in four steps:
//!
//! 1. Once a day, compute the key that signs every request of that day with
//!    [`signing_key`].
//! 2. Once for each instant you sign at, write the start of the string to
//!    sign with [`string_to_sign_prefix`].
//! 3. For each request, hash [`Unsigned::canonical`] with SHA-256, and
//!    compute the HMAC-SHA256 of the prefix and the lowercase hexadecimal of
//!    that hash, under the day's key.
//! 4. Write the HMAC into the request with [`set_signature`].
//!
//! This crate computes no hash. The `sigv4` module of the
//! `borink-object-storage-crypto` crate does steps 1 to 4 with any SHA-256,
//! for one request or for many at once.
//!
//! The signature covers the `host` header, which your HTTP client writes from
//! the URL. Send the URL as the request holds it. An endpoint that names the
//! default port of its scheme is written without it, as HTTP clients send
//! it.
//!
//! # Metadata
//!
//! Put the pairs of a write in [`PutOptions::metadata`](crate::PutOptions::metadata). A header value
//! holds only ASCII without control characters. A value with any other text
//! is sent as an RFC 2047 encoded word, which S3 decodes. S3 returns
//! such a value encoded as well: pass each response header to
//! [`metadata_name`], and its value to [`metadata_value`].
//!
//! A value that a read would not return exactly is refused with
//! [`InvalidRequest::MetadataValue`].
//! [`MetadataPair::value`](crate::MetadataPair::value) lists those values.
//!
//! # Listing
//!
//! A listing reads one page per request. Write the template of its pages
//! with [`prepare_list`], and each page with [`encode_list`]. Read the whole
//! body of a [`ListHeadOutcome::Page`], and pass it to [`fill_listing`].
//! Pass the page's [`Listing::next_marker`] as the marker of the next page.
//!
//! ```
//! # use borink_object_storage_proto::s3::{self, Bucket, Service};
//! # use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
//! use borink_object_storage_proto::{EntryKind, ListEntry, ListItem, ListOptions, Timestamps};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let bucket = Bucket::new(
//! #     "https://s3.eu-west-1.amazonaws.com", "objects", "eu-west-1", Service::Aws,
//! # )?;
//! # let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe_best_effort)?;
//! # let now = Timestamps::from_unix(1_787_400_000);
//! let options = ListOptions {
//!     delimiter: Some("/"),
//!     ..ListOptions::default()
//! };
//! let mut template = vec![0; 1024];
//! let pages = s3::prepare_list(&bucket, &credentials, &options, &mut template)?;
//! let mut buffer = vec![0; 2048];
//! let unsigned = s3::encode_list(&pages, &template, &ListItem::new("photos/"), &now, &mut buffer)?;
//! let request = s3::set_signature(&unsigned, &mut buffer, &[0; 32]);
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
//! let page = s3::fill_listing(&mut body, &mut entries)?;
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
//! 1. Create an upload with [`prepare_create_upload`] and
//!    [`encode_create_upload`], and read its ID out of the answer with
//!    [`read_upload_id`]. The metadata of the object goes into this request.
//! 2. Stage each part with [`prepare_stage_part`] and [`encode_stage_part`],
//!    under a number from 1, in any order and at once if you like. Keep the
//!    entity tag that [`StageHeadOutcome::Staged`](crate::StageHeadOutcome::Staged)
//!    carries for each. On AWS, every part but the last holds at least
//!    [`MIN_PART_LEN`] bytes.
//! 3. Commit the parts with [`prepare_commit_parts`] and
//!    [`encode_commit_parts`]. The request's body is the list of parts, whose
//!    SHA-256 it signs. S3 answers with status 200 before it has finished,
//!    and writes the result into the body, which may still be an error. Read
//!    it with [`accept_commit_parts_body`].
//!
//! [`encode_list_parts`] reads the parts that an upload holds, to resume it.
//! [`encode_abort_upload`] ends an upload without an object. An upload that
//! is neither committed nor aborted keeps its parts, and AWS bills them.
//!
//! ```
//! use borink_object_storage_proto::s3::{self, Bucket, CommitItem, CommitOptions, PartRef, Service};
//! use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
//! use borink_object_storage_proto::{CommitHeadOutcome, ResponseHead, Timestamps};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let bucket = Bucket::new(
//! #     "https://s3.eu-west-1.amazonaws.com", "objects", "eu-west-1", Service::Aws,
//! # )?;
//! # let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe_best_effort)?;
//! # let now = Timestamps::from_unix(1_787_400_000);
//! let options = CommitOptions::default();
//! let mut template = vec![0; 1024];
//! let commits = s3::prepare_commit_parts(&bucket, &credentials, &options, &mut template)?;
//!
//! let parts = [PartRef { number: 1, e_tag: b"\"e1\"" }];
//! let commit = CommitItem::new("large.bin", "VXBsb2FkSUQ", &parts);
//! let mut buffer = vec![0; 2048];
//! let unsigned = s3::encode_commit_parts(&commits, &template, &commit, &now, &mut buffer)?;
//! assert_eq!(
//!     unsigned.body(&buffer),
//!     Some(
//!         b"<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
//!           <Part><PartNumber>1</PartNumber><ETag>\"e1\"</ETag></Part>\
//!           </CompleteMultipartUpload>"
//!             .as_slice()
//!     )
//! );
//! // `borink_object_storage_crypto::sigv4::sign` writes in the body's SHA-256
//! // with `s3::set_body_hash`, and then signs.
//! # s3::set_body_hash(&unsigned, &mut buffer, &[0; 32]);
//! let request = s3::set_signature(&unsigned, &mut buffer, &[0; 32]);
//! assert_eq!(
//!     request.url(),
//!     "https://s3.eu-west-1.amazonaws.com/objects/large.bin?uploadId=VXBsb2FkSUQ"
//! );
//!
//! let head = ResponseHead::new(200);
//! let shape = commit.shape(&options);
//! let outcome = s3::accept_commit_parts_head(shape, head)?;
//! assert!(matches!(outcome, CommitHeadOutcome::NeedResultBody { .. }));
//! let mut body = Vec::from(
//!     b"<CompleteMultipartUploadResult><Key>large.bin</Key>\
//!       <ETag>&quot;c0-1&quot;</ETag></CompleteMultipartUploadResult>"
//!         .as_slice(),
//! );
//! let CommitHeadOutcome::Committed { meta } = s3::accept_commit_parts_body(shape, head, &mut body)?
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
//! 1. Write a CreateSession with [`prepare_create_session`] and
//!    [`encode_create_session`], signed with your own credentials, and send
//!    it.
//! 2. Read the head with [`accept_create_session_head`], and the body with
//!    [`read_session`].
//! 3. Write the templates of the requests to the objects with the
//!    credentials that [`Session::credentials`] returns. Each request
//!    carries the token in `x-amz-s3session-token`.
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
//!   [`InvalidRequest::Prefix`].
//! - A directory bucket stores an RFC 2047 encoded word as it is sent. So a
//!   metadata value that a general purpose bucket would store other than as
//!   given, such as one with a line break, is sent as an encoded word rather
//!   than refused.
//! - A commit of parts reports most errors under status 200, such as
//!   `NoSuchUpload` and `PreconditionFailed`.
//!   [`accept_commit_parts_body`] reads them as the outcomes that
//!   a general purpose bucket answers with. A directory bucket refuses a
//!   commit whose part numbers are not consecutive, such as 1 and 3, with
//!   `InvalidPartOrder`. The client sends such a commit as given.
//! - A copy, [`prepare_copy`] or [`prepare_stage_part_copy`], is authorized
//!   by your own credentials, not by a session's: AWS refuses the
//!   credentials of a session for it. Write its template with your own
//!   credentials, which sign it for `s3express` too. A template written
//!   with a session's credentials is refused with [`InvalidRequest::Option`].
//!
//! # Content that is not signed
//!
//! [`PayloadSigning::Unsigned`] sends a write or a part without the SHA-256
//! of its content. The signature then does not cover the content. TLS still
//! protects the bytes in transit, but nothing checks the bytes that you
//! handed to your HTTP client. Send a checksum of the content, in
//! [`PutItem::checksum`](crate::PutItem::checksum) or
//! [`StagePartItem::checksum`], to have S3 check them.

use core::cmp::Ordering;

use crate::checksum::ChecksumKind;
use crate::common::{
    ContentRange, FailureOutcome, accept_success, condition_header, decimal_header, failure,
    finish_with_body, meta_of, parse_content_range, text_header, validate_condition,
    validate_properties, validate_tags,
};
use crate::encoding::{self, rfc2047};
use crate::http::PlainHttp;
use crate::request::ByteSink;
use crate::sigv4::{Credentials, MAX_REGION_LEN};
use crate::{
    Classification, ConditionKind, ConditionValue, DeleteHeadOutcome, DeleteKind, DeleteShape,
    Error, Failure, GetHeadOutcome, GetKind, GetShape, InvalidRequest, ListEntry, ListHeadOutcome,
    ListInclude, ListMarker, Listing, MetadataPair, Method, ObjectMeta, Payload, PutHeadOutcome,
    PutShape, RequestedRange, ResponseFault, ResponseHead, Result, ServiceErrorKind, Timestamps,
    WriteOptions,
};

mod batch;
mod copy;
mod parts;
mod restore;
mod tags;
mod template;

use crate::checksum::BASE64_LEN;
use crate::common::{
    checksum_text, validate_checksum_item, validate_delimiter, validate_get_item,
    validate_get_options, validate_revision_item, validate_revision_kind,
};
use crate::template::{Body, ParamValue};
use crate::template::{Hole, Values};
use crate::template::{NO_TEXT, Param, Text};
use crate::url::QueryValue;
use crate::{
    DeleteOptions, GetItem, GetOptions, ListItem, ListOptions, ObjectItem, PutItem, PutOptions,
    Ranging, RevisionKind,
};
pub use batch::{
    DeleteResult, MAX_DELETE_KEYS, accept_delete_many_error_body, accept_delete_many_head,
    encode_delete_many, fill_delete_results, prepare_delete_many,
};
pub use copy::{
    StagePartCopyItem, accept_copy_body, accept_copy_error_body, accept_copy_head,
    accept_stage_part_copy_body, accept_stage_part_copy_head, encode_copy, encode_stage_part_copy,
    prepare_copy, prepare_stage_part_copy,
};
pub use parts::{
    CommitItem, CommitOptions, CreateUploadHeadOutcome, CreateUploadOptions, ListPartsItem,
    MAX_PART_LEN, MAX_PARTS, MIN_PART_LEN, Part, PartRef, StagePartItem, UploadItem,
    accept_abort_upload_error_body, accept_abort_upload_head, accept_commit_parts_body,
    accept_commit_parts_error_body, accept_commit_parts_head, accept_create_upload_error_body,
    accept_create_upload_head, accept_list_parts_error_body, accept_list_parts_head,
    accept_stage_part_error_body, accept_stage_part_head, encode_abort_upload, encode_commit_parts,
    encode_create_upload, encode_list_parts, encode_stage_part, fill_parts, prepare_abort_upload,
    prepare_commit_parts, prepare_create_upload, prepare_list_parts, prepare_stage_part,
    read_upload_id,
};
pub use restore::{
    accept_restore_error_body, accept_restore_head, encode_restore, prepare_restore,
};
pub use tags::{
    accept_get_tagging_error_body, accept_get_tagging_head, accept_put_tagging_head,
    accept_update_error_body, encode_get_tagging, encode_put_tagging, fill_tags,
    prepare_get_tagging, prepare_put_tagging,
};
use template::{Describe, Detail, Field, FieldList, S3Values, Value};
pub use template::{
    Operation, STRING_TO_SIGN_PREFIX_LEN, Template, Unsigned, set_body_checksum, set_body_hash,
    set_signature, signing_key, string_to_sign_prefix,
};

// What `x-amz-content-sha256` carries for content that is not signed.
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// The most bytes that an S3 object key holds.
///
/// Every encoding method refuses a longer key with
/// [`InvalidRequest::KeyTooLong`]. S3 counts the bytes of the key's UTF-8.
pub const MAX_KEY_LEN: usize = 1024;

/// The most bytes that S3 writes in one `PutObject` request.
///
/// [`encode_put`] refuses a longer payload with
/// [`InvalidRequest::PayloadTooLarge`].
pub const MAX_PUT_LEN: u64 = 5 * 1024 * 1024 * 1024;

/// The prefix of the header that carries one metadata pair.
pub const METADATA_PREFIX: &str = "x-amz-meta-";

/// Returns the metadata name that a response header carries, or [`None`]
/// for a header that carries no pair.
///
/// S3 stores a metadata name in lowercase, and returns it so.
pub fn metadata_name(header: &str) -> Option<&str> {
    crate::common::metadata_name(header, METADATA_PREFIX)
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
    /// This crate refuses a request that AWS refuses, and a response that AWS
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
    /// - A condition is sent as the item gives it, and the metadata of a write
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

    // Whether AWS answers, so that a client holds requests and responses to
    // the rules that AWS documents, which a compatible service need not
    // follow. Each caller names the rule it applies.
    pub(crate) const fn is_aws(self) -> bool {
        match self {
            Self::Aws | Self::AwsDirectory => true,
            Self::Compatible => false,
        }
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
    // The object that templates from this bucket name, if they name one.
    object: Option<&'a str>,
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
            object: None,
        })
    }

    /// Names the object `key` of this bucket, for templates of that one
    /// object.
    ///
    /// A template prepared from it writes the key as fixed text, checked
    /// once, and so does each request's line and canonical request: a
    /// request of it checks and encodes no key. Pass each encoding function
    /// an empty key, or this one; any other is [`InvalidRequest::Template`].
    /// Keep one template for each object you read or write often, such as
    /// the files of a database. An operation that names no object, such as
    /// a listing, refuses a bucket that names one with
    /// [`InvalidRequest::Option`].
    #[must_use]
    pub const fn object(self, key: &'a str) -> Self {
        Self {
            object: Some(key),
            ..self
        }
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

/// An element that an S3 listing writes for an object, other than the four
/// that every [`ListEntry`] carries.
///
/// Name the ones you want in a [`PropertySet`] and read a page with
/// [`fill_listing_with`], which hands you their values as it goes.
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
        // The names of `Self::name`, matched whole: a switch on the length
        // and one compare.
        Some(match name {
            b"StorageClass" => Self::StorageClass,
            b"ChecksumAlgorithm" => Self::ChecksumAlgorithm,
            b"ChecksumType" => Self::ChecksumType,
            b"Owner" => Self::Owner,
            b"RestoreStatus" => Self::RestoreStatus,
            b"VersionId" => Self::VersionId,
            b"IsLatest" => Self::IsLatest,
            _ => return None,
        })
    }
}

// `from_bits` shifts by `COUNT`, which must be below the `u64` shift width.
const _: () = assert!(ObjectProperty::COUNT < 64);

impl crate::properties::sealed::Sealed for ObjectProperty {}

impl crate::properties::Property for ObjectProperty {
    const COUNT: usize = ObjectProperty::ALL.len();

    fn number(self) -> u8 {
        self as u8
    }
}

/// The properties that one read of an S3 page is asked for.
///
/// Build one with [`PropertySet::of`] and pass it to [`fill_listing_with`].
pub type PropertySet = crate::properties::PropertySet<ObjectProperty>;

/// The values that one entry of an S3 listing gave for the properties of a
/// [`PropertySet`].
pub type PropertyValues<'x, 'b> = crate::properties::PropertyValues<'x, 'b, ObjectProperty>;

impl PropertySet {
    /// A set of these properties. Naming one twice is the same as once.
    pub const fn of(properties: &[ObjectProperty]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < properties.len() {
            bits |= 1 << (properties[i] as u8);
            i += 1;
        }
        Self::new(bits)
    }
}

/// The credentials of a session with one
/// [directory bucket](self#directory-buckets).
///
/// [`Self::credentials`] turns them into the credentials that sign requests
/// to that bucket. Its [`Debug`](core::fmt::Debug) output shows the key ID and hides the secret
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

impl<'a> Session<'a> {
    /// Returns the credentials of this session, which sign every request to
    /// the directory bucket but a copy, and send the token in
    /// `x-amz-s3session-token`.
    ///
    /// `wipe` clears the copy of the secret that signing makes; see
    /// [`Credentials::new`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCredentials`] if the key ID, the secret or the
    /// token cannot sign a request.
    pub fn credentials(&self, wipe: fn(&mut [u8])) -> Result<Credentials<'a>> {
        Credentials::new(self.key_id, self.secret, wipe)?.with_s3_session_token(self.token)
    }
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
/// [`accept_create_session_head`] returns an [`Err`] only for a
/// head it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionHeadOutcome<'h> {
    /// The credentials follow in the response body.
    ///
    /// Read the whole body into one buffer and pass it to
    /// [`read_session`].
    Session {
        /// The exact length of the response body, if the head states it.
        expected_len: Option<u64>,
    },
    /// The head reports a failure but names no error.
    ///
    /// This outcome is not final. Pass this failure and the response body to
    /// [`accept_create_session_error_body`], which returns the final
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

/// Reads the response head of a GET or a HEAD and reports what to do next.
///
/// Pass the service that answered, as [`Template::service`] returns it, and
/// the [`GetShape`] of the read. A failure of a GET is
/// [`GetHeadOutcome::NeedErrorBody`]: read the body and pass it to
/// [`accept_get_error_body`]. A failure of a HEAD is final, and names no
/// error.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read against `shape`.
/// A `Content-Range` that is missing, or whose end is before its start, is
/// [`ResponseFault::Head`], and so is a success without `Content-Length`
/// from [`Service::Aws`]. A range other than the one the read asked for,
/// and a ranged read answered with status 200, are [`ResponseFault::Range`].
pub fn accept_get_head<'h>(
    service: Service,
    shape: GetShape,
    head: ResponseHead<'h>,
) -> Result<GetHeadOutcome<'h>> {
    let ranged = shape.range != RequestedRange::Whole;
    // AWS states the length of every response that it serves.
    let states_length = service.is_aws();
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
        _ => Ok(failed(&head)),
    }
}

/// Writes the template of reads with `options` from `bucket`, signed with
/// `credentials`, into `buf`.
///
/// Only the key ID and the token of `credentials` are written into the
/// template. A read can name a version, and never a snapshot.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if the reads cannot be S3 requests: a
/// ranged HEAD is [`InvalidRequest::RangedHead`], and a snapshot is
/// [`InvalidRequest::Revision`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_get(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &GetOptions,
    buf: &mut [u8],
) -> Result<Template> {
    validate_get_options(options, false)?;
    template::prepare(bucket, credentials, &describe_get(options), buf)
}

// The request of a read with `options`.
fn describe_get(options: &GetOptions) -> Describe<'static, 1> {
    let method = match options.kind {
        GetKind::Bytes => Method::Get,
        GetKind::Head => Method::Head,
    };
    let mut describe = Describe::new(Operation::Get, method, Detail::Get(*options));
    describe.query = [version_param(options.revision)];
    describe.condition(options.condition);
    describe.holes.range = options.ranging == Ranging::Ranged;
    describe
}

// The query parameter that names a template's version. S3 keeps no
// snapshots.
const fn version_param(revision: RevisionKind) -> Param<'static> {
    match revision {
        RevisionKind::Version => Some(("versionId", ParamValue::Hole(Hole::Revision))),
        RevisionKind::Current | RevisionKind::Snapshot => None,
    }
}

/// Writes the read `item` at `now` from `template` into `buf`, ready to
/// sign.
///
/// `bytes` are the template's own, as [`prepare_get`] wrote them.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `item` is not a read that the template
/// writes, or not a key that S3 takes: see [Object keys](crate#object-keys).
/// A template for another operation is [`InvalidRequest::Template`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_get(
    template: &Template,
    bytes: &[u8],
    item: &GetItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    let Detail::Get(options) = template.detail else {
        return Err(InvalidRequest::Template.into());
    };
    let key = template.request_key(bytes, item.key)?;
    validate_get_item(&options, item)?;
    let values = S3Values(Values {
        now: Some(now),
        key,
        range: Some(item.range).filter(|range| *range != RequestedRange::Whole),
        condition: item.condition_value,
        revision: item.revision,
        ..Values::default()
    });
    template::encode(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_get(&options),
    )
}

/// Finishes a [`GetHeadOutcome::NeedErrorBody`] with the response body.
///
/// Pass the [`Failure`] of that outcome and the body that you read. Pass
/// an empty body if you could not read one: the outcome is then final with
/// the error unnamed.
///
/// It takes the [`GetShape`] of the request as Azure's does, so that a host
/// reads both services alike. S3 reports a failed condition in the head, so
/// the body never decides it.
pub fn accept_get_error_body<'h>(
    _shape: GetShape,
    failure: Failure<'h>,
    body: &[u8],
) -> GetHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Whether every write of a template signs the SHA-256 of its content.
///
/// The SHA-256 is the only part of a signature that grows with the
/// content. Leaving the content unsigned is sound over HTTPS: send a
/// checksum with it, which S3 checks and stores. See the
/// [module documentation](self).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PayloadSigning {
    /// Every write sends `UNSIGNED-PAYLOAD`, and nothing reads the content.
    #[default]
    Unsigned,
    /// Every write signs the SHA-256 of its content, which you compute and
    /// pass to the encoder.
    Signed,
}

/// Writes the template of whole-object writes with `options` into `bucket`,
/// signed with `credentials`, into `buf`.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a metadata pair, a content property,
/// a storage class or a tag that S3 would not store as given. For
/// [`Service::Aws`], a date condition is [`InvalidRequest::Condition`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_put(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &PutOptions<'_>,
    signing: PayloadSigning,
    buf: &mut [u8],
) -> Result<Template> {
    let service = bucket.service;
    validate_metadata(options.metadata, service)?;
    validate_stored(&options.options, service)?;
    let aws = matches!(service, Service::Aws | Service::AwsDirectory);
    if aws && options.condition.is_date() {
        return Err(InvalidRequest::Condition.into());
    }
    let mut signed = FieldList::new();
    push_stored(&mut signed, &options.options);
    let describe = Describe {
        signed: signed.as_slice(),
        metadata: options.metadata,
        ..describe_put(options.condition, options.checksum, signing)
    };
    template::prepare(bucket, credentials, &describe, buf)
}

// The request of a whole-object write of a template with `condition`,
// `checksum` and `signing`.
fn describe_put(
    condition: ConditionKind,
    checksum: Option<ChecksumKind>,
    signing: PayloadSigning,
) -> Describe<'static, 0> {
    let mut describe = Describe::new(
        Operation::Put,
        Method::Put,
        Detail::Put {
            condition,
            checksum,
            signing,
        },
    );
    describe.condition(condition);
    describe.holes.content_length = Some(Hole::ContentLength);
    if let Some(kind) = checksum {
        describe.checksum(kind, Hole::Checksum);
    }
    match signing {
        PayloadSigning::Unsigned => describe.content_sha256 = UNSIGNED_PAYLOAD.as_bytes(),
        PayloadSigning::Signed => describe.holes.content_sha256 = Some(Hole::PayloadHash),
    }
    describe
}

/// Writes the whole-object write `item` at `now` from `template` into
/// `buf`, ready to sign.
///
/// `bytes` are the template's own, as [`prepare_put`] wrote them. Pass the
/// SHA-256 of the content as `content_sha256` for a template that signs it,
/// and [`None`] for one that does not.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `item` is not a write that the template
/// writes, or not a key that S3 takes. Content longer than [`MAX_PUT_LEN`]
/// is [`InvalidRequest::PayloadTooLarge`]. A checksum that is not the base64 of
/// a digest of the template's kind is [`InvalidRequest::Checksum`]. A SHA-256
/// that the template does not take, or one missing that it does, is
/// [`InvalidRequest::Option`]. For [`Service::Aws`], `If-None-Match` with any
/// value but `*` is [`InvalidRequest::Condition`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_put<'i>(
    template: &Template,
    bytes: &[u8],
    item: &PutItem<'i>,
    content_sha256: Option<&[u8; 32]>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'i>> {
    let Detail::Put {
        condition,
        checksum,
        signing,
    } = template.detail
    else {
        return Err(InvalidRequest::Template.into());
    };
    let key = template.request_key(bytes, item.key)?;
    if item.content.len() > MAX_PUT_LEN {
        return Err(InvalidRequest::PayloadTooLarge.into());
    }
    validate_write_condition(condition, item.condition_value, template.service)?;
    validate_checksum_item(checksum, item.checksum)?;
    let hex = match (signing, content_sha256) {
        (PayloadSigning::Signed, Some(digest)) => Some(encoding::hex(digest)),
        (PayloadSigning::Unsigned, None) => None,
        _ => return Err(InvalidRequest::Option.into()),
    };
    let mut text = [0; BASE64_LEN];
    let values = S3Values(Values {
        now: Some(now),
        key,
        condition: item.condition_value,
        content_length: Some(item.content.len()),
        checksum: checksum_text(item.checksum, &mut text, true),
        payload_hash: hex.as_ref(),
        ..Values::default()
    });
    template::encode(
        template,
        bytes,
        &values,
        item.content,
        buf,
        &describe_put(condition, checksum, signing),
    )
}

/// Reads the response head of a write and reports what S3 did.
///
/// Pass the [`PutShape`] of the write. A failure is
/// [`PutHeadOutcome::NeedErrorBody`]: read the body and pass it to
/// [`accept_put_error_body`].
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read against `shape`.
/// A success status other than 200 is [`ResponseFault::Status`], and so is a
/// 412 to a write that carried no condition.
pub fn accept_put_head<'h>(shape: PutShape, head: ResponseHead<'h>) -> Result<PutHeadOutcome<'h>> {
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
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`PutHeadOutcome::NeedErrorBody`] with the response body.
///
/// Pass the [`Failure`] of that outcome and the body that you read, or an
/// empty body if you could not read one.
///
/// It takes the [`PutShape`] of the request as Azure's does, so that a host
/// reads both services alike. S3 reports a failed condition in the head, so
/// the body never decides it.
pub fn accept_put_error_body<'h>(
    _shape: PutShape,
    failure: Failure<'h>,
    body: &[u8],
) -> PutHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Writes the template of removals with `options` from `bucket`, signed
/// with `credentials`, into `buf`.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `options` cannot become an S3
/// request:
///
/// - [`InvalidRequest::Option`] for a [`DeleteKind`] other than
///   [`DeleteKind::Object`]. S3 keeps no snapshots.
/// - [`InvalidRequest::Revision`] for a snapshot.
/// - [`InvalidRequest::Condition`] for any condition but
///   [`ConditionKind::IfMatch`], for [`Service::Aws`] and
///   [`Service::AwsDirectory`]. AWS removes on `If-Match` only.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_delete(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &DeleteOptions,
    buf: &mut [u8],
) -> Result<Template> {
    if options.kind != DeleteKind::Object {
        return Err(InvalidRequest::Option.into());
    }
    // AWS removes on `If-Match` alone.
    if bucket.service.is_aws()
        && !matches!(
            options.condition,
            ConditionKind::None | ConditionKind::IfMatch
        )
    {
        return Err(InvalidRequest::Condition.into());
    }
    prepare_object(
        bucket,
        credentials,
        Operation::Delete,
        options.condition,
        options.revision,
        None,
        &ObjectText::default(),
        buf,
    )
}
/// Writes the removal `item` at `now` from `template` into `buf`, ready
/// to sign.
///
/// `bytes` are the template's own, as [`prepare_delete`] wrote them. The
/// request has no content.
///
/// S3 answers the removal of a key that holds no object as it answers any
/// other removal. The outcome therefore does not say whether an object was
/// there.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `item` is not a removal that the
/// template writes, or not a key that S3 takes: see
/// [Object keys](crate#object-keys). A template for another operation is
/// [`InvalidRequest::Template`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_delete(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    encode_object(template, bytes, Operation::Delete, item, now, buf)
}
/// Reads the response head of a removal and reports what S3 did.
///
/// Pass the service of the template and the [`DeleteShape`] of the
/// removal. A failure is [`DeleteHeadOutcome::NeedErrorBody`]: read the
/// body and pass it to [`accept_delete_error_body`].
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read against
/// `shape`. A success status other than 204, and a failed condition on a
/// removal that carried none, are both [`ResponseFault::Status`]. A
/// [`Service::Compatible`] client also takes 200 as success.
pub fn accept_delete_head<'h>(
    service: Service,
    shape: DeleteShape,
    head: ResponseHead<'h>,
) -> Result<DeleteHeadOutcome<'h>> {
    // Some services that implement the S3 API answer 200.
    let accepts_200 = !service.is_aws();
    match head.status {
        204 => Ok(DeleteHeadOutcome::Accepted),
        200 if accepts_200 => Ok(DeleteHeadOutcome::Accepted),
        412 if shape.condition == ConditionKind::None => Err(ResponseFault::Status.into()),
        412 => Ok(DeleteHeadOutcome::PreconditionFailed),
        200..=203 | 205..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`DeleteHeadOutcome::NeedErrorBody`] with the response body.
///
/// This is [`accept_get_error_body`] for a removal, and reads the body the
/// same way. A 404 names a missing bucket, or a missing object under an
/// `If-Match` condition.
///
/// It takes the [`DeleteShape`] of the request as Azure's does, so that a host
/// reads both services alike. S3 reports a failed condition in the head, so
/// the body never decides it.
pub fn accept_delete_error_body<'h>(
    _shape: DeleteShape,
    failure: Failure<'h>,
    body: &[u8],
) -> DeleteHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Writes the template of listings with `options` of `bucket`, signed with
/// `credentials`, into `buf`.
///
/// A listing is a ListObjectsV2, or a ListObjectVersions for
/// [`ListInclude::VERSIONS`]. Each page is written with its parameters in
/// the order that SigV4 signs them.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `options` cannot become an S3 request:
///
/// - [`InvalidRequest::Delimiter`] for an empty delimiter, and for any but `/`
///   on a [directory bucket](self#directory-buckets).
/// - [`InvalidRequest::Option`] for [`ListInclude::METADATA`] and
///   [`ListInclude::SNAPSHOTS`], which S3 does not list, and for
///   [`ListInclude::VERSIONS`] on a directory bucket, which keeps no
///   versions.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_list(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    options: &ListOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    // A general purpose bucket groups keys at any text. A directory bucket
    // groups them at `/` alone, and answers anything else with 400
    // `InvalidRequest`.
    let directory = bucket.service == Service::AwsDirectory;
    validate_delimiter(options.delimiter, directory)?;
    let versions = options.include.contains(ListInclude::VERSIONS);
    if options
        .include
        .intersects(ListInclude::METADATA | ListInclude::SNAPSHOTS)
        || (versions && directory)
    {
        return Err(InvalidRequest::Option.into());
    }
    let detail = Detail::List {
        include: options.include,
        delimiter: options.delimiter.is_some(),
        max_keys: options.max_results.is_some(),
    };
    template::prepare(
        bucket,
        credentials,
        &describe_list(detail, Some(options)),
        buf,
    )
}

// The request of a page of a listing of `detail`. `options` are the
// template's, which hold the values of its fixed parameters, and none for a
// request: whether each is there is the detail's.
fn describe_list<'p>(detail: Detail, options: Option<&ListOptions<'p>>) -> Describe<'p, 8> {
    let Detail::List {
        include,
        delimiter,
        max_keys,
    } = detail
    else {
        unreachable!("a listing's template has the detail of a listing");
    };
    let fixed = ParamValue::Fixed;
    let delimiter = delimiter.then(|| {
        let text = options.and_then(|options| options.delimiter);
        (
            "delimiter",
            fixed(text.map_or(QueryValue::Literal(""), |text| {
                QueryValue::Encoded(text.as_bytes())
            })),
        )
    });
    let max_keys = max_keys.then(|| {
        let count = options.and_then(|options| options.max_results);
        (
            "max-keys",
            fixed(count.map_or(QueryValue::Literal(""), QueryValue::Number)),
        )
    });
    // SigV4 signs the parameters in the order of their names. With
    // `encoding-type=url`, a key that XML cannot carry still arrives.
    let encoding = Some(("encoding-type", fixed(QueryValue::Literal("url"))));
    let mut describe = Describe::new(Operation::List, Method::Get, detail);
    describe.key = false;
    describe.query = match include.contains(ListInclude::VERSIONS) {
        true => [
            delimiter,
            encoding,
            Some(("key-marker", ParamValue::Optional(Hole::Marker))),
            max_keys,
            Some(("prefix", ParamValue::Optional(Hole::Prefix))),
            Some((
                "version-id-marker",
                ParamValue::Optional(Hole::VersionMarker),
            )),
            Some(("versions", fixed(QueryValue::Literal("")))),
            None,
        ],
        false => [
            Some(("continuation-token", ParamValue::Optional(Hole::Marker))),
            delimiter,
            encoding,
            include
                .contains(ListInclude::OWNER)
                .then_some(("fetch-owner", fixed(QueryValue::Literal("true")))),
            Some(("list-type", fixed(QueryValue::Literal("2")))),
            max_keys,
            Some(("prefix", ParamValue::Optional(Hole::Prefix))),
            Some(("start-after", ParamValue::Optional(Hole::StartAfter))),
        ],
    };
    describe
}

/// Writes the page `item` at `now` from `template` into `buf`, ready to
/// sign.
///
/// `bytes` are the template's own, as [`prepare_list`] wrote them. Read
/// the whole response body and pass it to [`fill_listing`].
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `item` cannot become an S3 request:
///
/// - [`InvalidRequest::Marker`] for a marker of the other kind than the
///   listing hands out, or an empty one.
/// - [`InvalidRequest::Prefix`] for a prefix that does not end in `/`, on a
///   [directory bucket](self#directory-buckets).
/// - [`InvalidRequest::Option`] for [`ListItem::start_after`] in a listing of
///   versions, which starts at its markers alone.
/// - [`InvalidRequest::Template`] for a template for another operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_list(
    template: &Template,
    bytes: &[u8],
    item: &ListItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    let detail @ Detail::List { include, .. } = template.detail else {
        return Err(InvalidRequest::Template.into());
    };
    // A listing continues from the kind of marker that its pages hand out,
    // and S3 hands out no empty one: a version marker names a key.
    let versions = include.contains(ListInclude::VERSIONS);
    let (marker, version_marker) = match item.marker {
        None => (None, None),
        Some(ListMarker::Text(token)) if !versions && !token.is_empty() => (Some(token), None),
        Some(ListMarker::Version { key, version })
            if versions && !key.is_empty() && version.is_none_or(|version| !version.is_empty()) =>
        {
            (Some(key), version)
        }
        Some(_) => return Err(InvalidRequest::Marker.into()),
    };
    // A directory bucket lists only at a prefix that ends in `/`.
    if template.service == Service::AwsDirectory
        && !item.prefix.is_empty()
        && !item.prefix.ends_with('/')
    {
        return Err(InvalidRequest::Prefix.into());
    }
    if versions && item.start_after.is_some_and(|key| !key.is_empty()) {
        return Err(InvalidRequest::Option.into());
    }
    let values = S3Values(Values {
        now: Some(now),
        prefix: Some(item.prefix),
        marker,
        start_after: item.start_after,
        version_marker,
        ..Values::default()
    });
    template::encode(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_list(detail, None),
    )
}

/// Reads the response head of a listing and reports what S3 did.
///
/// S3 often sends a page without `Content-Length`, so cap what you read.
/// A failure is [`ListHeadOutcome::NeedErrorBody`]: read the body and
/// pass it to [`accept_list_error_body`].
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success
/// status other than 200 is [`ResponseFault::Status`], and a
/// `Content-Length` that is not a number is [`ResponseFault::Head`].
pub fn accept_list_head<'h>(head: ResponseHead<'h>) -> Result<ListHeadOutcome<'h>> {
    match head.status {
        200 => Ok(ListHeadOutcome::Page {
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`ListHeadOutcome::NeedErrorBody`] with the response body.
///
/// This is [`accept_get_error_body`] for a listing, and reads the
/// body the same way. A missing bucket is a
/// [`ListHeadOutcome::ServiceFailure`] with
/// [`ServiceErrorKind::NoSuchContainer`].
pub fn accept_list_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> ListHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Reads a page out of the response body of a listing.
///
/// Reading is destructive, and your array must hold the whole page. An
/// array of 1,000 entries holds any page from AWS.
///
/// AWS writes all objects of a page before its groups of keys. An
/// object's entity tag keeps its quotes. Read its date with
/// [`iso8601_ms`](crate::iso8601_ms).
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
    body: &'b mut [u8],
    into: &mut [E],
) -> Result<Listing<'b>> {
    refuse_error_document(body)?;
    crate::xml::s3::fill_listing(body, into, PropertySet::default(), |entry, _| entry.into())
}

/// Reads a page the way [`fill_listing`] does, and hands you the
/// values of the properties in `wanted` as it goes.
///
/// What `build` returns for each entry is written into your array.
///
/// ```
/// # use borink_object_storage_proto::s3::{self, ObjectProperty, PropertySet};
/// # use borink_object_storage_proto::{ListEntry, Result};
/// # fn read(body: &mut [u8]) -> Result<()> {
/// let wanted = PropertySet::of(&[ObjectProperty::StorageClass]);
/// let mut entries = [(ListEntry::default(), None); 1000];
/// s3::fill_listing_with(body, &mut entries, wanted, |entry, values| {
///     (entry, values.get(ObjectProperty::StorageClass))
/// })?;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// As [`fill_listing`].
pub fn fill_listing_with<'b, E>(
    body: &'b mut [u8],
    into: &mut [E],
    wanted: PropertySet,
    build: impl FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) -> E,
) -> Result<Listing<'b>> {
    refuse_error_document(body)?;
    crate::xml::s3::fill_listing(body, into, wanted, build)
}

// Reads a head that reports a failure. S3 names its error in the body
// alone, so every failure needs the body.
pub(crate) fn failed<'h, O: FailureOutcome<'h>>(head: &ResponseHead<'h>) -> O {
    O::need_error_body(failure(head.status, None, head.request_id))
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

// SigV4 signs a header value without the spaces at either end, and with each
// run of spaces inside it as one space. A value sent as it is holds no other
// whitespace.
pub(crate) fn write_canonical_value(out: &mut dyn ByteSink, value: &[u8]) {
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

// The pairs in the order of their lowercase names. A request carries a handful
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
        return Err(InvalidRequest::EmptyKey.into());
    }
    if key.len() > MAX_KEY_LEN {
        return Err(InvalidRequest::KeyTooLong.into());
    }
    // A host resolves `.` and `..` segments out of the URL before it sends
    // it, which would address another object.
    if has_dot_segment(key.as_bytes()) {
        return Err(InvalidRequest::KeyWouldBeNormalized.into());
    }
    Ok(())
}

// Whether `key` has a segment, between slashes or at either end, that is
// `.` or `..`. One pass, looking only at a dot that starts a segment.
fn has_dot_segment(key: &[u8]) -> bool {
    let ends = |at: usize| key.get(at).is_none_or(|&byte| byte == b'/');
    key.iter().enumerate().any(|(at, &byte)| {
        byte == b'.'
            && (at == 0 || key[at - 1] == b'/')
            && (ends(at + 1) || (key[at + 1] == b'.' && ends(at + 2)))
    })
}

// Checks a checksum of S3's: one that a write signs as text, or one that the
// encoder computes, an MD5 or a CRC-64/NVME.
// Checks the content properties, the storage class and the tags that a write
// stores with the object.
pub(crate) fn validate_stored(options: &WriteOptions<'_>, service: Service) -> Result<()> {
    // A general purpose bucket returns header bytes as it got them. A
    // directory bucket refuses a byte outside ASCII with 400 InvalidRequest.
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

// The characters that S3 takes in the key and the value of a tag.
pub(crate) fn s3_tag_char(character: char) -> bool {
    character.is_alphanumeric() || " +-=._:/@".contains(character)
}

// The header that carries a CRC-64/NVME on S3.
const CRC64_HEADER: &str = "x-amz-checksum-crc64nvme";

// Checks the condition of a write: a whole object, or a commit of parts.
pub(crate) fn validate_write_condition(
    condition: ConditionKind,
    value: Option<ConditionValue<'_>>,
    service: Service,
) -> Result<()> {
    validate_condition(condition, value)?;
    // AWS writes on `If-None-Match` only if no object holds the key, and takes
    // no value but `*`. It takes no date condition on a write.
    let aws = service.is_aws();
    if aws
        && (condition.is_date()
            || (condition == ConditionKind::IfNoneMatch
                && value != Some(ConditionValue::ETag(b"*"))))
    {
        return Err(InvalidRequest::Condition.into());
    }
    Ok(())
}

// S3 sends a metadata pair as an `x-amz-meta-` header, so the name must be a
// token. `write_metadata_value` writes the value, and S3 matches a name
// without case.
pub(crate) fn validate_metadata(metadata: &[MetadataPair<'_>], service: Service) -> Result<()> {
    for (index, pair) in metadata.iter().enumerate() {
        if pair.name.is_empty() || !pair.name.bytes().all(token_byte) {
            return Err(InvalidRequest::MetadataName.into());
        }
        // An empty value is a pair with no text, which S3 stores. A value is
        // refused if a read would not return it exactly. A general purpose
        // bucket stores CR and LF as spaces, even from an encoded word. It
        // returns an ASCII value unencoded, so HTTP drops the whitespace at
        // either end, and a token that reads as an encoded word is decoded. A
        // directory bucket stores an encoded word as it is sent, so
        // `encodes` sends each of these values as one.
        if !stores_encoded_words(service) && general_purpose_changes(pair.value) {
            return Err(InvalidRequest::MetadataValue.into());
        }
        if metadata[..index]
            .iter()
            .any(|earlier| earlier.name.eq_ignore_ascii_case(pair.name))
        {
            return Err(InvalidRequest::MetadataDuplicate.into());
        }
    }
    let len = metadata.iter().fold(0usize, |len, pair| {
        len.saturating_add(pair.name.len())
            .saturating_add(pair.value.len())
    });
    let limited = service.is_aws();
    if limited && len > MAX_METADATA_LEN {
        return Err(InvalidRequest::MetadataTooLarge.into());
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

// Writes the template of `request` into `buf`.
// What every request of a template that names one object sends alike,
// beside its structure: the fixed fields, signed and not, the metadata, and
// the text of a fixed body.
#[derive(Default)]
pub(in crate::s3) struct ObjectText<'p> {
    pub(in crate::s3) signed: &'p [Field<'p>],
    pub(in crate::s3) unsigned: &'p [Field<'p>],
    pub(in crate::s3) metadata: &'p [MetadataPair<'p>],
    pub(in crate::s3) body: Option<Text<'p>>,
}

// Writes the template of `operation`, which names one object, with the
// condition `condition`, the kind of revision `revision`, and the checksum
// of its body `checksum`.
#[allow(clippy::too_many_arguments)]
pub(in crate::s3) fn prepare_object(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    operation: Operation,
    condition: ConditionKind,
    revision: RevisionKind,
    checksum: Option<ChecksumKind>,
    text: &ObjectText<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    validate_revision_kind(revision, false)?;
    let describe = Describe {
        signed: text.signed,
        unsigned: text.unsigned,
        metadata: text.metadata,
        ..describe_object(operation, condition, revision, checksum, text.body)
    };
    template::prepare(bucket, credentials, &describe, buf)
}

// The request of `operation`, which names one object, for a template with
// `condition`, `revision` and `checksum`: its method, its parameter, and
// its body, which a PutObjectTagging and a RestoreObject write from their
// templates. `body` is the text of that body, the template's, and none for
// a request.
pub(in crate::s3) fn describe_object<'p>(
    operation: Operation,
    condition: ConditionKind,
    revision: RevisionKind,
    checksum: Option<ChecksumKind>,
    body: Option<Text<'p>>,
) -> Describe<'p, 4> {
    let (method, param) = match operation {
        Operation::Delete => (Method::Delete, None),
        Operation::GetTagging => (Method::Get, Some("tagging")),
        Operation::PutTagging => (Method::Put, Some("tagging")),
        Operation::Restore => (Method::Post, Some("restore")),
        _ => (Method::Post, Some("uploads")),
    };
    let mut describe = Describe::new(
        operation,
        method,
        Detail::Object {
            condition,
            revision,
            checksum,
        },
    );
    describe.query[0] = param.map(|name| (name, ParamValue::Fixed(QueryValue::Literal(""))));
    describe.query[3] = version_param(revision);
    describe.condition(condition);
    if matches!(operation, Operation::PutTagging | Operation::Restore) {
        describe.with_body(Body::Fixed(body.unwrap_or(NO_TEXT)), checksum);
    }
    describe
}

// Adds the fields of what a write stores beside its content and its
// metadata: the content properties, the storage class and the tags.
pub(in crate::s3) fn push_stored<'d>(fields: &mut FieldList<'d>, options: &WriteOptions<'d>) {
    for (name, value) in options.properties.iter() {
        fields.push(Field::new(name, Value::Bytes(value.as_bytes())));
    }
    if let Some(class) = options.storage_class {
        fields.push(Field::new(
            "x-amz-storage-class",
            Value::Bytes(class.as_bytes()),
        ));
    }
    if !options.tags.is_empty() {
        fields.push(Field::new("x-amz-tagging", Value::Tags(options.tags)));
    }
}

// Writes the request of `operation` that names the one object `item`, from
// a template of that operation.
pub(in crate::s3) fn encode_object(
    template: &Template,
    bytes: &[u8],
    operation: Operation,
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    let Detail::Object {
        condition,
        revision,
        checksum,
    } = template.detail
    else {
        return Err(InvalidRequest::Template.into());
    };
    if template.operation != operation {
        return Err(InvalidRequest::Template.into());
    }
    let key = template.request_key(bytes, item.key)?;
    validate_revision_item(revision, item.revision)?;
    validate_condition(condition, item.condition_value)?;
    let values = S3Values(Values {
        now: Some(now),
        key,
        condition: item.condition_value,
        revision: item.revision,
        body_checksum: checksum,
        ..Values::default()
    });
    template::encode(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_object(operation, condition, revision, checksum, None),
    )
}

/// Writes the template of CreateSessions into `buf`, signed with
/// `credentials`, which ask the [directory bucket](self#directory-buckets)
/// `bucket` for the credentials of a session.
///
/// # Errors
///
/// Returns [`InvalidRequest::Option`] unless the bucket is of
/// [`Service::AwsDirectory`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_create_session(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    if bucket.service != Service::AwsDirectory {
        return Err(InvalidRequest::Option.into());
    }
    template::prepare(bucket, credentials, &describe_session(), buf)
}

// The request of a CreateSession.
fn describe_session() -> Describe<'static, 4> {
    let mut describe = Describe::new(
        Operation::CreateSession,
        Method::Get,
        Detail::Object {
            condition: ConditionKind::None,
            revision: RevisionKind::Current,
            checksum: None,
        },
    );
    describe.key = false;
    describe.query[0] = Some(("session", ParamValue::Fixed(QueryValue::Literal(""))));
    describe
}

/// Writes a CreateSession at `now` from `template` into `buf`, ready to
/// sign.
///
/// # Errors
///
/// Returns [`InvalidRequest::Template`] for a template of another operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_create_session(
    template: &Template,
    bytes: &[u8],
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Unsigned<'static>> {
    if template.operation != Operation::CreateSession {
        return Err(InvalidRequest::Template.into());
    }
    let values = S3Values(Values {
        now: Some(now),
        ..Values::default()
    });
    template::encode(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_session(),
    )
}

/// Reads the response head of a CreateSession and reports what to do next.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 200 is [`ResponseFault::Status`].
pub fn accept_create_session_head<'h>(head: ResponseHead<'h>) -> Result<SessionHeadOutcome<'h>> {
    match head.status {
        200 => Ok(SessionHeadOutcome::Session {
            expected_len: decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(failed(&head)),
    }
}

/// Finishes a [`SessionHeadOutcome::NeedErrorBody`] with the response body.
pub fn accept_create_session_error_body<'h>(
    failure: Failure<'h>,
    body: &[u8],
) -> SessionHeadOutcome<'h> {
    finish_with_body(failure, body_kind(body))
}

/// Reads the credentials of a session out of the response body of a
/// CreateSession. Sign the requests to the directory bucket with
/// [`Session::credentials`].
///
/// # Errors
///
/// Returns [`Error::Service`] if `body` is an error document, and
/// [`Error::Response`] if it holds no session.
pub fn read_session(body: &mut [u8]) -> Result<Session<'_>> {
    refuse_error_document(body)?;
    crate::xml::s3::read_session(body)
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
