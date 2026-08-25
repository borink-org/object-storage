//! Object storage in a sans-I/O, externally buffered style.
//!
//! See the synchronous [ureq host](https://github.com/borink-org/object-storage/tree/master/hosts/ureq).

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

pub use azure::{Blobs, Container, VERSION};
pub use error::{CapacityError, Error, InvalidPlan, Result};
pub use head::GetHead;
pub use outcome::{BodyWindow, FailureClass, GetHeadOutcome, ObjectMeta};
pub use request::WireRequest;
pub use time::Timestamps;
pub use types::{ConditionKind, GetKind, GetShape, PhysicalGet, RequestedRange};
