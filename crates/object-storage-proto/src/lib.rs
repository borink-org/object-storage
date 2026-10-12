//! Azure Blob Storage and S3 requests for callers that own their memory and
//! their I/O.
//!
//! This crate builds HTTP requests and reads HTTP responses. It never opens a
//! socket, never reads the clock, and never allocates. You supply the buffer,
//! the current time and the HTTP client.
//!
//! # How a read works
//!
//! A read has three steps.
//!
//! 1. Describe what every read shares as [`GetOptions`], and call
//!    [`azure::prepare_get`] to write a [`azure::Template`] into a buffer
//!    that you own. Keep the template and its bytes for as many reads as you
//!    like.
//! 2. Describe each read as a [`GetItem`], and call [`azure::encode_get`]
//!    to write the request into a second buffer. Send the [`WireRequest`]
//!    with your HTTP client. Every byte of the request is in that buffer, so
//!    [`WireRequest`] can also name each part of it by offset and length: see
//!    [`WireRequest::url_span`].
//! 3. Put the response headers into a [`ResponseHead`] and call
//!    [`azure::accept_get_head`] with the read's [`GetItem::shape`]. It
//!    returns a [`GetHeadOutcome`] that tells you what to do with the body.
//!
//! A write has the same three steps, with [`PutOptions`], [`PutItem`],
//! [`azure::prepare_put`], [`azure::encode_put`] and
//! [`azure::accept_put_head`]. The content stays where you put it: the
//! request states its length in the head, and borrows the bytes or leaves
//! them to you. Describe the content with a [`Payload`], which names a
//! length whether or not you hold the bytes, so a write can stream from a
//! file or a socket.
//!
//! A removal has the same three steps too, with [`DeleteOptions`],
//! [`DeleteItem`], [`azure::prepare_delete`], [`azure::encode_delete`] and
//! [`azure::accept_delete_head`].
//!
//! An object can also be written in parts. Stage each part, publish an
//! ordered list of them with a [`PhysicalCommit`], and read what is staged
//! with a listing of the parts. Both services take the same commit plan and
//! answer with the same [`StageHeadOutcome`], [`CommitHeadOutcome`] and
//! [`ListPartsHeadOutcome`]. They name a part differently. On Azure a part
//! is a block, named by an ID that you choose: see
//! [`azure::PhysicalStageBlock`] and [`azure::PhysicalListBlocks`]. On S3 a
//! part belongs to an upload that S3 creates, and is named by its number and
//! the entity tag that S3 returned for it: see
//! [Uploads in parts](s3#uploads-in-parts).
//!
//! S3 works the same way, as the [`s3`] module describes, with one more
//! step: you sign each request with AWS Signature Version 4. The
//! `borink-object-storage-crypto` crate signs it for you.
//!
//! # Example
//!
//! ```
//! use borink_object_storage_proto::{
//!     AzureNamespace, Container, GetHeadOutcome, GetItem, GetOptions, ListEntry,
//!     ListHeadOutcome, ListItem, ListOptions, Method, Payload, PutHeadOutcome, PutItem,
//!     PutOptions, ResponseHead, Timestamps, azure,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let container = Container::new("https://account.blob.core.windows.net", "objects")?;
//! let token = "access-token";
//! let now = Timestamps::from_unix(1_787_400_000);
//!
//! // 1. Write the template of every read once.
//! let options = GetOptions::new();
//! let mut template = vec![0; 512];
//! let reads =
//!     azure::prepare_get(&container, token, AzureNamespace::Unknown, &options, &mut template)?;
//!
//! // 2. Write each read into your own buffer, then send it.
//! let get = GetItem::new("directory/object.txt");
//! let mut buffer = vec![0; 1024];
//! let request = azure::encode_get(&reads, &template, &get, &now, &mut buffer)?;
//! assert_eq!(request.method(), Method::Get);
//! for (name, value) in request.headers() {
//!     // your_client.header(name, value);
//! }
//!
//! // 3. Read the response head that your client returned.
//! let head = ResponseHead::from_headers(200, [("Content-Length", b"8".as_slice())]);
//! match azure::accept_get_head(get.shape(&options), head)? {
//!     GetHeadOutcome::Body { meta, body, .. } => {
//!         assert_eq!(meta.size, Some(8));
//!         assert_eq!(body.object_offset, 0);
//!     }
//!     other => panic!("unexpected outcome: {other:?}"),
//! }
//!
//! // A listing reads its result out of the response body.
//! let options = ListOptions {
//!     delimiter: Some("/"),
//!     max_results: Some(2),
//!     ..ListOptions::default()
//! };
//! let mut template = vec![0; 512];
//! let pages =
//!     azure::prepare_list(&container, token, AzureNamespace::Unknown, &options, &mut template)?;
//! let mut buffer = vec![0; 1024];
//! let request =
//!     azure::encode_list(&pages, &template, &ListItem::new("directory/"), &now, &mut buffer)?;
//! assert_eq!(
//!     request.url(),
//!     "https://account.blob.core.windows.net/objects\
//!      ?restype=container&comp=list&prefix=directory%2F&delimiter=%2F&maxresults=2"
//! );
//!
//! match azure::accept_list_head(ResponseHead::new(200))? {
//!     ListHeadOutcome::Page { .. } => {}
//!     other => panic!("unexpected outcome: {other:?}"),
//! }
//!
//! // The body is yours: read it whole, then read the entries out of it.
//! let mut body = Vec::from(
//!     b"<EnumerationResults><Blobs><Blob><Name>directory/a.txt</Name>\
//!       <Properties><Content-Length>8</Content-Length></Properties></Blob>\
//!       </Blobs><NextMarker/></EnumerationResults>"
//!         .as_slice(),
//! );
//! let mut entries = vec![ListEntry::default(); 2];
//! let page = azure::fill_listing(&mut body, &mut entries)?;
//! assert_eq!(page.filled, 1);
//! assert_eq!(entries[0].key, "directory/a.txt");
//! assert_eq!(page.next_marker, None);
//!
//! // A write follows the same three steps.
//! let options = PutOptions::default();
//! let mut template = vec![0; 512];
//! let writes =
//!     azure::prepare_put(&container, token, AzureNamespace::Unknown, &options, &mut template)?;
//! let put = PutItem::new("directory/object.txt", Payload::Slice(b"contents"));
//! let mut buffer = vec![0; 1024];
//! let request = azure::encode_put(&writes, &template, &put, &now, &mut buffer)?;
//! assert_eq!(request.method(), Method::Put);
//! assert_eq!(request.payload().bytes(), Some(b"contents".as_slice()));
//!
//! let head = ResponseHead::from_headers(201, [("ETag", b"\"tag\"".as_slice())]);
//! match azure::accept_put_head(put.shape(&options), head)? {
//!     PutHeadOutcome::Created { meta, .. } => {
//!         assert_eq!(meta.e_tag, Some(b"\"tag\"".as_slice()))
//!     }
//!     other => panic!("unexpected outcome: {other:?}"),
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Metadata and checksums
//!
//! To store metadata pairs with an object, put them in
//! [`PutOptions::metadata`], [`PhysicalCommit::metadata`] or, on S3,
//! [`s3::PhysicalCreateUpload::metadata`].
//! To read them back from a listing, put [`ListInclude::METADATA`] in the
//! plan and call [`ListEntry::metadata`] on each entry. To read them back
//! from a head read, pass each response header name to
//! [`azure::metadata_name`] or [`s3::metadata_name`]. On S3, pass each
//! value to [`s3::metadata_value`].
//!
//! To have Azure check the content of a write, name an MD5 or a CRC64 in
//! [`PutOptions::checksum`], and pass the checksum of each write in
//! [`PutItem::checksum`], as base64 text or as the [`Digest`] you computed.
//! S3 takes those two too, and a CRC32, a CRC32C, a SHA-1 or a SHA-256. This
//! crate implements no checksum. The [`checksum`] module says where to get
//! one.
//!
//! A write stores the content properties of an object, its tags and its
//! storage class from [`WriteOptions`]. A read returns the properties in
//! [`ObjectMeta`].
//!
//! # Sizing the buffer
//!
//! The functions that write a template or a request refuse a buffer that is
//! too small, and state the exact number of bytes that they need. Pass an
//! empty buffer to learn it. The methods of [`Blobs`] and [`s3::Objects`]
//! need header slots as well: call a `*_requirements` function of
//! [`layered`] first. The functions for an S3 client are in
//! [`layered::s3`].
//!
//! A listing needs a second buffer for the response body, which
//! [`ListHeadOutcome::Page`] sizes.
//!
//! A `*_requirements` function is a dry run: it encodes the request into an
//! empty buffer and reads the capacities from the refusal. It allocates
//! nothing, and it reports a plan error that the encoding method reports
//! again.
//!
//! # Object keys
//!
//! A key is the name of an object within its container or bucket, before
//! percent-encoding. Each function that writes a request refuses a key that
//! the service refuses, or that would name another object than the one asked
//! for:
//!
//! - An empty key is [`InvalidPlan::EmptyKey`].
//! - A `.` or `..` segment is [`InvalidPlan::KeyWouldBeNormalized`],
//!   because a host resolves it out of the URL before it sends the request.
//!   On Azure, so is any segment that ends in `.`, because Azure stores the
//!   name without that dot.
//! - On Azure, a control character is [`InvalidPlan::KeyControlCharacter`],
//!   and more than 255 segments is [`InvalidPlan::KeyTooManySegments`].
//! - On an Azure account of [`AzureNamespace::Flat`], more than 1024 UTF-16
//!   code units is [`InvalidPlan::KeyTooLong`], where a character outside the
//!   basic plane counts twice. A template for [`AzureNamespace::Unknown`]
//!   sends such a key.
//! - On S3, more than [`s3::MAX_KEY_LEN`] bytes is
//!   [`InvalidPlan::KeyTooLong`]. S3 takes every other key.
//!
//! # Staying within the service's limits
//!
//! The encoding methods refuse a plan that the service would refuse, with an
//! [`InvalidPlan`] that names the rule. See [Object keys](#object-keys) for
//! the rules on a key. On Azure, the numeric limits are
//! [`azure::MAX_URL_LEN`] for the whole URL, [`azure::MAX_PUT_LEN`] for one
//! write, and [`azure::MAX_STAGE_LEN`] for one block. On S3, they are
//! [`s3::MAX_KEY_LEN`] for a key, [`s3::MAX_PUT_LEN`] for one write,
//! [`s3::MAX_PART_LEN`] for one part, [`s3::MAX_PARTS`] for the parts of an
//! upload to AWS, and [`s3::MAX_METADATA_LEN`] for the metadata of a write
//! to AWS.
//!
//! # Host requirements
//!
//! Your HTTP client must not decompress the response body. See
//! [`BodyWindow`] for the reason.
//!
//! Hand back the response head and the response body as two values. Every
//! outcome borrows the [`ResponseHead`] that you passed in, and a
//! `NeedErrorBody` outcome then asks for the body. A value that lends the
//! head from `&self` and reads the body from `&mut self` cannot do both.
//!
//! The [ureq host](https://github.com/borink-org/object-storage/tree/master/hosts/ureq)
//! is a complete example. It reads every operation the same way, so copy its
//! shape for your own.
//!
//! # Reading a failure
//!
//! A `NeedErrorBody` outcome carries a [`Failure`] whose `request_id`
//! borrows the head. Read the body, and pass it with the failure to the
//! `accept_*_error_body` method of the same operation. That method returns
//! the same outcome type again, with the error that the body named.
//!
//! Every outcome type is `#[non_exhaustive]`. Treat a variant that your
//! `match` does not name as a failure of the service, and report the status.
//!
//! This crate never retries a request. [`Failure::class`] says whether a
//! retry can succeed. When to retry, and how often, is your decision.

#![no_std]
#![forbid(unsafe_code)]

pub mod azure;
pub mod checksum;
mod common;
mod encoding;
mod error;
mod head;
mod http;
mod http_message;
pub mod layered;
mod multipart;
mod outcome;
mod request;
pub mod s3;
pub mod sigv4;
mod template;
mod time;
mod types;
mod url;
mod xml;

pub use azure::{AzureNamespace, AzureRejection, Blobs, Container, VERSION, classify_error};
pub use checksum::{ChecksumKind, ChecksumProvider, ChecksumState, Digest};
pub use error::{CapacityError, Error, ErrorCode, InvalidPlan, ResponseFault, Result};
pub use head::ResponseHead;
pub use outcome::{
    BodyWindow, Classification, CommitHeadOutcome, CopyHeadOutcome, DeleteHeadOutcome,
    DeleteManyHeadOutcome, Failure, FailureClass, GetHeadOutcome, ListHeadOutcome,
    ListPartsHeadOutcome, Listing, ObjectMeta, PutHeadOutcome, RestoreHeadOutcome,
    ServiceErrorKind, StageHeadOutcome, TagsHeadOutcome, UpdateHeadOutcome,
};
pub use request::{HeaderSpan, LegacyRequest, Method, RequestSize, Span};
pub use template::WireRequest;
pub use time::Timestamps;
pub use types::{
    BlobProperty, ChecksumValue, CommitShape, Condition, ConditionKind, ConditionValue,
    ContentProperties, CopyShape, CopySource, DeleteItem, DeleteKind, DeleteOptions, DeleteShape,
    DeleteTarget, EntryKind, GetItem, GetKind, GetOptions, GetShape, ListEntry, ListInclude,
    ListItem, ListMarker, ListOptions, Metadata, MetadataPair, Payload, PhysicalCommit,
    PhysicalCopy, PhysicalDeleteMany, PhysicalRestore, PhysicalSetTags, Properties, PropertySet,
    PropertyValues, PutItem, PutOptions, PutShape, RangeForm, Ranging, RequestedRange,
    RestorePriority, Revision, RevisionKind, Tag, TransactionalChecksum, WriteOptions,
};
