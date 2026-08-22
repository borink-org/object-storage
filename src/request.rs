use core::str;

use crate::{CapacityError, Extent, WorkspaceExtent};

pub struct RequestWorkspace<'a> {
    packed: Backing<'a>,
}

enum Backing<'a> {
    Slice(&'a mut [u8]),
    Extent(&'a mut dyn Extent),
}

impl<'a> RequestWorkspace<'a> {
    pub fn new(bytes: &'a mut [u8]) -> Self {
        Self {
            packed: Backing::Slice(bytes),
        }
    }

    pub fn with_extent(packed: &'a mut dyn Extent) -> Self {
        Self {
            packed: Backing::Extent(packed),
        }
    }

    pub fn capacity(&self) -> usize {
        match &self.packed {
            Backing::Slice(bytes) => bytes.len(),
            Backing::Extent(extent) => extent.as_slice().len(),
        }
    }

    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        match &mut self.packed {
            Backing::Slice(bytes) => bytes,
            Backing::Extent(extent) => extent.as_mut_slice(),
        }
    }

    pub fn try_reserve(&mut self, error: CapacityError) -> bool {
        match error.extent {
            WorkspaceExtent::Packed => match &mut self.packed {
                Backing::Slice(bytes) => error.required <= bytes.len(),
                Backing::Extent(extent) => extent.try_reserve(error.required),
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    url: &'a str,
    headers: [(&'static str, &'a str); 3],
}

impl<'a> Request<'a> {
    pub(crate) fn new(
        url: &'a str,
        authorization: &'a str,
        date: &'a str,
        version: &'static str,
    ) -> Self {
        Self {
            url,
            headers: [
                ("authorization", authorization),
                ("x-ms-date", date),
                ("x-ms-version", version),
            ],
        }
    }

    pub fn method(&self) -> &'static str {
        "GET"
    }

    pub fn url(&self) -> &'a str {
        self.url
    }

    pub fn headers(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.headers.iter().copied()
    }
}

pub(crate) enum Writer<'a> {
    Counting(usize),
    Storing {
        bytes: &'a mut [u8],
        position: usize,
    },
}

impl<'a> Writer<'a> {
    pub(crate) fn counting() -> Self {
        Self::Counting(0)
    }

    pub(crate) fn storing(bytes: &'a mut [u8]) -> Self {
        Self::Storing { bytes, position: 0 }
    }

    pub(crate) fn push(&mut self, value: &str) {
        match self {
            Self::Counting(position) => *position += value.len(),
            Self::Storing { bytes, position } => {
                let end = *position + value.len();
                bytes[*position..end].copy_from_slice(value.as_bytes());
                *position = end;
            }
        }
    }

    pub(crate) fn position(&self) -> usize {
        match self {
            Self::Counting(position) | Self::Storing { position, .. } => *position,
        }
    }

    pub(crate) fn finish(self) -> Option<&'a [u8]> {
        match self {
            Self::Counting(_) => None,
            Self::Storing { bytes, position } => Some(&bytes[..position]),
        }
    }
}

pub(crate) fn text(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).expect("request construction writes UTF-8")
}
