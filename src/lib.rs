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
//! # Example
//!
//! ```
//! use borink_object_storage::{
//!     Blobs, Container, ResponseHead, GetHeadOutcome, PhysicalGet, Timestamps, layered,
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
//! # Ok(())
//! # }
//! ```
//!
//! # Sizing the buffer
//!
//! [`Blobs::encode_get`] refuses a buffer that is too small and states the
//! exact number of bytes that it needs. You can grow the buffer and call
//! again, or call [`layered::requirements`] first, as the example does.
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
    BodyWindow, Classification, FailureClass, GetHeadOutcome, ObjectMeta, ServiceErrorKind,
};
pub use request::WireRequest;
pub use time::Timestamps;
pub use types::{ConditionKind, GetKind, GetShape, PhysicalGet, RequestedRange};
