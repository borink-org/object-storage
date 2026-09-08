//! Azure Blob Storage reads for callers that own their memory and their I/O.
//!
//! This crate builds HTTP requests and reads HTTP responses. It never opens a
//! socket, never reads the clock, and never allocates. You supply the buffer,
//! the current time and the HTTP client.
//!
//! # How a read works
//!
//! A read has three steps.
//!
//! 1. Describe the read as a plan: a [`GetShape`] and a [`PhysicalGet`].
//! 2. Call [`Blobs::encode_get`] to write the request head into your buffer,
//!    and send the [`WireRequest`] with your HTTP client. Every byte of the
//!    head is in that buffer, so [`WireRequest`] can also name each part of it
//!    by offset and length: see [`WireRequest::url_span`].
//! 3. Put the response headers into a [`ResponseHead`] and call
//!    [`Blobs::accept_get_head`]. It returns a [`GetHeadOutcome`] that tells
//!    you what to do with the body.
//!
//! Pass the same [`GetShape`] to steps 2 and 3. The second call checks the
//! response against the plan, so you never restate what the plan already
//! holds.
//!
//! A removal has the same three steps, with [`PhysicalDelete`],
//! [`Blobs::encode_delete`] and [`Blobs::accept_delete_head`].
//!
//! A write has the same three steps, with [`PhysicalPut`],
//! [`Blobs::encode_put`] and [`Blobs::accept_put_head`]. The content stays
//! where you put it: [`Blobs::encode_put`] states its length in the head, and
//! the [`WireRequest`] borrows the bytes or leaves them to you. Describe the
//! content with a [`Payload`], which names a length whether or not you hold
//! the bytes, so a write can stream from a file or a socket.
//!
//! An object can also be written in blocks. Stage each block with
//! [`azure::PhysicalStageBlock`], publish an ordered list of them with
//! [`azure::PhysicalCommitBlocks`], and read what is staged with
//! [`azure::PhysicalListBlocks`]. These are Azure's own operations, under the
//! [`azure`] module.
//!
//! # Example
//!
//! ```
//! use borink_object_storage_proto::{
//!     Blobs, Container, GetHeadOutcome, HeaderSpan, ListEntry, ListHeadOutcome,
//!     Method, Payload, PhysicalGet, PhysicalList, PhysicalPut, PutHeadOutcome,
//!     ResponseHead, Timestamps,
//!     layered,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let container = Container::new("https://account.blob.core.windows.net", "objects")?;
//! let blobs = Blobs::new(container, "access-token")?;
//! let now = Timestamps::from_unix(1_787_400_000);
//!
//! // 1. Plan the read.
//! let get = PhysicalGet::new("directory/object.txt");
//!
//! // 2. Encode the request head into your own buffer, then send it.
//! let size = layered::get_requirements(&blobs, &get, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request = blobs.encode_get(&mut buffer, &mut headers, &get, &now)?;
//! assert_eq!(request.method(), Method::Get);
//! for (name, value) in request.headers() {
//!     // your_client.header(name, value);
//! }
//!
//! // 3. Read the response head that your client returned.
//! let head = ResponseHead::from_headers(200, [("Content-Length", b"8".as_slice())]);
//! match blobs.accept_get_head(get.shape(), head)? {
//!     GetHeadOutcome::Body { meta, body, .. } => {
//!         assert_eq!(meta.size, Some(8));
//!         assert_eq!(body.object_offset, 0);
//!     }
//!     other => panic!("unexpected outcome: {other:?}"),
//! }
//!
//! // A listing reads its result out of the response body.
//! let list = PhysicalList {
//!     delimited: true,
//!     max_results: Some(2),
//!     ..PhysicalList::new("directory/")
//! };
//! let size = layered::list_requirements(&blobs, &list, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request = blobs.encode_list(&mut buffer, &mut headers, &list, &now)?;
//! assert_eq!(
//!     request.url(),
//!     "https://account.blob.core.windows.net/objects\
//!      ?restype=container&comp=list&prefix=directory%2F&delimiter=%2F&maxresults=2"
//! );
//!
//! match blobs.accept_list_head(ResponseHead::new(200))? {
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
//! let page = blobs.fill_listing(&mut body, &mut entries)?;
//! assert_eq!(page.filled, 1);
//! assert_eq!(entries[0].key, "directory/a.txt");
//! assert_eq!(page.next_marker, None);
//!
//! // A write follows the same three steps.
//! let put = PhysicalPut::new("directory/object.txt");
//! let content = Payload::Slice(b"contents");
//! let size = layered::put_requirements(&blobs, &put, content, &now)?;
//! let mut buffer = vec![0; size.bytes];
//! let mut headers = vec![HeaderSpan::default(); size.headers];
//! let request = blobs.encode_put(
//!     &mut buffer, &mut headers, &put, content, &now,
//! )?;
//! assert_eq!(request.method(), Method::Put);
//! assert_eq!(request.payload().bytes(), Some(b"contents".as_slice()));
//!
//! let head = ResponseHead::from_headers(201, [("ETag", b"\"tag\"".as_slice())]);
//! match blobs.accept_put_head(put.shape(), head)? {
//!     PutHeadOutcome::Created { meta, .. } => {
//!         assert_eq!(meta.e_tag, Some(b"\"tag\"".as_slice()))
//!     }
//!     other => panic!("unexpected outcome: {other:?}"),
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Sizing the buffer
//!
//! The encoding methods refuse a buffer that is too small and state the exact
//! numbers of bytes and header slots that they need. Grow both buffers and
//! call again, or
//! call [`layered::get_requirements`], [`layered::put_requirements`],
//! [`layered::list_requirements`] or their block-operation siblings first, as
//! the example does.
//!
//! A listing needs a second buffer for the response body, which
//! [`ListHeadOutcome::Page`] sizes.
//!
//! A `*_requirements` function is a dry run: it encodes the request into an
//! empty buffer and reads the capacities from the refusal. It allocates
//! nothing, and it reports a plan error that the encoding method reports
//! again.
//!
//! # Staying within Azure's limits
//!
//! The encoding methods refuse a plan that Azure would refuse, with an
//! [`InvalidPlan`] that names the rule. The rules on a key are documented at
//! [`PhysicalGet::key`]. The numeric limits are [`azure::MAX_URL_LEN`] for
//! the whole URL, [`azure::MAX_PUT_LEN`] for one write, and
//! [`azure::MAX_STAGE_LEN`] for one block.
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
//! borrows the head. Copy what you need out of it, read the body, and call
//! the `accept_*_error_body` method of the same operation. That method
//! returns the same outcome type again, with the error that the body named.
//!
//! Every outcome type is `#[non_exhaustive]`. Treat a variant that your
//! `match` does not name as a failure of the service, and report the status.
//!
//! This crate never retries a request. [`Failure::class`] says whether a
//! retry can succeed. When to retry, and how often, is your decision.

#![no_std]
#![forbid(unsafe_code)]

pub mod azure;
mod error;
mod head;
mod http;
pub mod layered;
mod outcome;
mod path;
mod request;
mod time;
mod types;
mod xml;

pub use azure::{AzureNamespace, AzureRejection, Blobs, Container, VERSION, classify_error};
pub use error::{CapacityError, Error, ErrorCode, InvalidPlan, ResponseFault, Result};
pub use head::ResponseHead;
pub use outcome::{
    BodyWindow, Classification, CommitBlocksHeadOutcome, DeleteHeadOutcome, Failure, FailureClass,
    GetHeadOutcome, ListBlocksHeadOutcome, ListHeadOutcome, Listing, ObjectMeta, PutHeadOutcome,
    ServiceErrorKind, StageBlockHeadOutcome,
};
pub use request::{HeaderSpan, Method, RequestSize, Span, WireRequest};
pub use time::Timestamps;
pub use types::{
    BlobProperty, CommitBlocksShape, ConditionKind, DeleteKind, DeleteShape, EntryKind, GetKind,
    GetShape, ListEntry, ListShape, Payload, PhysicalDelete, PhysicalGet, PhysicalList,
    PhysicalPut, Properties, PropertySet, PropertyValues, PutShape, RangeForm, RequestedRange,
};
