//! Object storage in a sans-I/O, externally buffered style.
//!
//! // review: this would show up in docs.rs for the crate but hosts/ureq wouldn't be uploaded to crates.io so we would probably have to provide a github link or similar instead?
//! See `hosts/ureq` for a complete synchronous host.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "alloc")]
extern crate alloc;

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
#[cfg(feature = "alloc")]
pub use memory::VecExtent;
pub use memory::{CapacityError, Extent, RequestRequirements, WorkspaceExtent};
pub use request::{Request, RequestWorkspace};
pub use response::Response;
pub use time::Timestamps;
