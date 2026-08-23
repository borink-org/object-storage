use core::fmt;

// TODO(doc-review): Public API rustdoc is an initial scaffold for manual review.

/// A contiguous byte extent owned, and optionally grown, by the host.
pub trait Extent: Send + Sync {
    /// Returns the currently available bytes.
    fn as_slice(&self) -> &[u8];
    /// Returns the currently available mutable bytes.
    fn as_mut_slice(&mut self) -> &mut [u8];
    /// Attempts contents-preserving growth to at least `required` bytes.
    ///
    /// Refusing growth by returning `false` is always valid.
    fn try_reserve(&mut self, required: usize) -> bool;
}

impl Extent for [u8] {
    fn as_slice(&self) -> &[u8] {
        self
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        self
    }

    fn try_reserve(&mut self, required: usize) -> bool {
        required <= self.len()
    }
}

/// A growable extent backed by [`alloc::vec::Vec`].
#[cfg(feature = "alloc")]
pub type VecExtent = alloc::vec::Vec<u8>;

#[cfg(feature = "alloc")]
impl Extent for VecExtent {
    fn as_slice(&self) -> &[u8] {
        self.as_slice()
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }

    fn try_reserve(&mut self, required: usize) -> bool {
        let additional = required.saturating_sub(self.len());
        if alloc::vec::Vec::try_reserve(self, additional).is_err() {
            return false;
        }
        self.resize(required, 0);
        true
    }
}

/// Identifies the caller-provided extent that was too small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkspaceExtent {
    /// Storage containing the URL and header values of a request.
    Packed,
}

impl fmt::Display for WorkspaceExtent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Packed => f.write_str("packed request"),
        }
    }
}

/// The exact capacity needed from one caller-provided extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityError {
    /// The extent that was too small.
    pub extent: WorkspaceExtent,
    /// The minimum capacity required by the attempted operation.
    pub required: usize,
    /// The capacity available during the attempted operation.
    pub available: usize,
}

impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} needs {} bytes but has {}",
            self.extent, self.required, self.available
        )
    }
}

impl core::error::Error for CapacityError {}

/// Extent sizes required to construct a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestRequirements {
    /// Bytes required for the packed request extent.
    pub packed: usize,
}
