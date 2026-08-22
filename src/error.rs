use core::fmt;

use crate::CapacityError;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    InvalidEndpoint,
    InvalidContainer,
    InvalidToken,
    InvalidKey,
    Capacity(CapacityError),
    NotFound,
    Unauthorized,
    Status(u16),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => f.write_str("invalid Azure Blob endpoint"),
            Self::InvalidContainer => f.write_str("invalid Azure container name"),
            Self::InvalidToken => f.write_str("invalid bearer token"),
            Self::InvalidKey => f.write_str("invalid object key"),
            Self::Capacity(error) => fmt::Display::fmt(error, f),
            Self::NotFound => f.write_str("object not found"),
            Self::Unauthorized => f.write_str("Azure rejected the bearer token"),
            Self::Status(status) => write!(f, "Azure returned HTTP {status}"),
        }
    }
}

impl core::error::Error for Error {}

impl From<CapacityError> for Error {
    fn from(value: CapacityError) -> Self {
        Self::Capacity(value)
    }
}

impl Error {
    pub fn capacity(&self) -> Option<CapacityError> {
        match *self {
            Self::Capacity(error) => Some(error),
            _ => None,
        }
    }
}
