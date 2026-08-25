//! Helpers written over the crate's public surface.
//!
//! Nothing here needs private access. They are provided because every host
//! wants them, not because the core cannot be used without them.

use crate::{Blobs, Error, PhysicalGet, Result, Timestamps};

/// Measures the buffer [`Blobs::encode_get`] needs for this plan.
///
/// Encoding into an empty buffer turns the capacity refusal into the
/// requirement, so requirements-first and grow-on-refusal hosts share one core
/// entry point. An invalid plan propagates unchanged.
pub fn requirements(blobs: &Blobs<'_>, get: &PhysicalGet<'_>, now: &Timestamps) -> Result<usize> {
    match blobs.encode_get(&mut [], get, now) {
        Ok(_) => Ok(0),
        Err(Error::Capacity(error)) => Ok(error.required),
        Err(error) => Err(error),
    }
}
