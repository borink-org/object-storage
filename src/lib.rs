//! Azure Blob GET requests built in caller-provided memory.
//!
//! See `examples/ureq_get.rs` for a complete synchronous host.

#![no_std]
#![forbid(unsafe_code)]

mod azure;
mod error;
mod memory;
mod request;
mod response;

pub use azure::{Blobs, Container, VERSION};
pub use error::{Error, Result};
pub use memory::{CapacityError, Extent, RequestRequirements, WorkspaceExtent};
pub use request::{Request, RequestWorkspace};
pub use response::Response;
