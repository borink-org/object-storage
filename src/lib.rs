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
//!    and send the [`WireRequest`] with your HTTP client.
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
//! # Example
//!
//! ```
//! use borink_object_storage::{
//!     Blobs, Container, GetHeadOutcome, Payload, PhysicalGet, PhysicalPut, PutHeadOutcome,
//!     ResponseHead, Timestamps, layered,
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
//! let mut buffer = vec![0; layered::get_requirements(&blobs, &get, &now)?];
//! let request = blobs.encode_get(&mut buffer, &get, &now)?;
//! assert_eq!(request.method(), "GET");
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
//! // A write follows the same three steps.
//! let put = PhysicalPut::new("directory/object.txt");
//! let content = Payload::Slice(b"contents");
//! let mut buffer = vec![0; layered::put_requirements(&blobs, &put, content, &now)?];
//! let request = blobs.encode_put(&mut buffer, &put, content, &now)?;
//! assert_eq!(request.method(), "PUT");
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
//! number of bytes that they need. You can grow the buffer and call again, or
//! call [`layered::get_requirements`] or [`layered::put_requirements`] first,
//! as the example does.
//!
//! # Host requirements
//!
//! Your HTTP client must not decompress the response body. See
//! [`BodyWindow`] for the reason.
//!
//! The [ureq host](https://github.com/borink-org/object-storage/tree/master/hosts/ureq)
//! is a complete example.

#![no_std]
#![forbid(unsafe_code)]

mod azure;
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

pub use azure::{Blobs, Container, VERSION, classify_error};
pub use error::{CapacityError, Error, InvalidPlan, Result};
pub use head::ResponseHead;
pub use outcome::{
    BodyWindow, Classification, DeleteHeadOutcome, FailureClass, GetHeadOutcome, ObjectMeta,
    PutHeadOutcome, ServiceErrorKind,
};
pub use request::WireRequest;
pub use time::Timestamps;
pub use types::{
    ConditionKind, DeleteKind, DeleteShape, GetKind, GetShape, Payload, PhysicalDelete,
    PhysicalGet, PhysicalPut, PutShape, RequestedRange,
};
