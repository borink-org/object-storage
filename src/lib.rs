//! Object storage in a sans-I/O, externally buffered style.
//!
//! See the synchronous [ureq host](https://github.com/borink-org/object-storage/tree/master/hosts/ureq).

#![no_std]
#![forbid(unsafe_code)]

mod azure;
mod error;
mod http;
pub mod layered;
mod path;
mod request;
mod response;
mod time;
mod types;

pub use azure::{Blobs, Container, VERSION};
pub use error::{CapacityError, Error, InvalidPlan, Result};
pub use request::WireRequest;
pub use response::Response;
pub use time::Timestamps;
pub use types::{ConditionKind, GetKind, GetShape, ObjectMeta, PhysicalGet, RequestedRange};
