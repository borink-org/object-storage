use core::fmt;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    InvalidEndpoint,
    InvalidContainer,
    InvalidToken,
    InvalidDate,
    InvalidKey,
    BufferTooSmall { required: usize, available: usize },
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
            Self::InvalidDate => f.write_str("invalid HTTP date"),
            Self::InvalidKey => f.write_str("invalid object key"),
            Self::BufferTooSmall {
                required,
                available,
            } => write!(
                f,
                "request buffer needs {required} bytes but has {available}"
            ),
            Self::NotFound => f.write_str("object not found"),
            Self::Unauthorized => f.write_str("Azure rejected the bearer token"),
            Self::Status(status) => write!(f, "Azure returned HTTP {status}"),
        }
    }
}

impl core::error::Error for Error {}
