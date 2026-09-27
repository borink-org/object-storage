//! A synchronous `ureq` host for `borink-object-storage`.
//!
//! [`azure`] sends Azure requests with a `Blobs` client, and [`s3`] sends S3
//! requests with an `Objects` client. Each module gets, puts, deletes and
//! lists in the same way.

pub mod azure;
pub mod s3;

// Error bodies are diagnostics, so this host caps what it will read for one.
const MAX_ERROR_BODY: u64 = 8 * 1024;

// A page is a document that this host holds whole, so it caps that too.
const MAX_PAGE: u64 = 8 * 1024 * 1024;
