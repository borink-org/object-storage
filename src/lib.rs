//! Azure Blob GET requests built in caller-provided memory.
//!
//! See `hosts/ureq` for a complete synchronous host.

// TODO(doc-review): Public API rustdoc is an initial scaffold for manual review.

#![no_std]
#![forbid(unsafe_code)]

mod azure;
mod error;
mod http;
mod memory;
mod path;
mod request;
mod response;
mod time;

pub use azure::{Blobs, Container, VERSION};
pub use error::{Error, Result};
pub use memory::{CapacityError, Extent, RequestRequirements, WorkspaceExtent};
pub use request::{Request, RequestWorkspace};
pub use response::Response;
pub use time::Timestamps;
