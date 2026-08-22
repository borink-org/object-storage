use core::fmt;

pub trait Extent: Send + Sync {
    fn as_slice(&self) -> &[u8];
    fn as_mut_slice(&mut self) -> &mut [u8];
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkspaceExtent {
    Packed,
}

impl fmt::Display for WorkspaceExtent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Packed => f.write_str("packed request"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityError {
    pub extent: WorkspaceExtent,
    pub required: usize,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestRequirements {
    pub packed: usize,
}
