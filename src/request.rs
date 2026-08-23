use core::str;

use crate::{CapacityError, Extent, WorkspaceExtent};

/// Caller-provided storage used while constructing a request.
pub struct RequestWorkspace<'a> {
    packed: Backing<'a>,
}

enum Backing<'a> {
    Slice(&'a mut [u8]),
    Extent(&'a mut dyn Extent),
}

impl<'a> RequestWorkspace<'a> {
    /// Uses a fixed mutable slice as packed request storage.
    pub fn new(bytes: &'a mut [u8]) -> Self {
        Self {
            packed: Backing::Slice(bytes),
        }
    }

    /// Uses a host-defined extent as packed request storage.
    pub fn with_extent(packed: &'a mut dyn Extent) -> Self {
        Self {
            packed: Backing::Extent(packed),
        }
    }

    /// Returns the current packed extent capacity.
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

    /// Asks the host extent to satisfy a capacity error.
    ///
    /// Fixed slices refuse requirements larger than their existing length.
    pub fn try_reserve(&mut self, error: CapacityError) -> bool {
        match error.extent {
            WorkspaceExtent::Packed => match &mut self.packed {
                Backing::Slice(bytes) => error.required <= bytes.len(),
                Backing::Extent(extent) => extent.try_reserve(error.required),
            },
        }
    }
}

/// A GET request borrowing its URL and header values from caller-owned memory.
///
/// The request cannot outlive either the workspace or timestamp used to build
/// it. No `'static` storage is required.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    method: &'static str,
    url: &'a str,
    headers: [(&'static str, &'a str); 6],
    header_count: usize,
}

impl<'a> Request<'a> {
    pub(crate) fn new(
        method: &'static str,
        url: &'a str,
        authorization: &'a str,
        date: &'a str,
        version: &'static str,
        range: Option<&'a str>,
        conditions: [Option<(&'static str, &'a str)>; 2],
    ) -> Self {
        let mut headers = [("", ""); 6];
        headers[..3].copy_from_slice(&[
            ("authorization", authorization),
            ("x-ms-date", date),
            ("x-ms-version", version),
        ]);
        let mut header_count = 3;
        if let Some(value) = range {
            headers[header_count] = ("range", value);
            header_count += 1;
        }
        for value in conditions.into_iter().flatten() {
            headers[header_count] = value;
            header_count += 1;
        }
        Self {
            method,
            url,
            headers,
            header_count,
        }
    }

    /// Returns the HTTP method.
    pub fn method(&self) -> &'static str {
        self.method
    }

    /// Returns the complete object URL.
    pub fn url(&self) -> &'a str {
        self.url
    }

    /// Iterates over the request headers in wire-independent order.
    pub fn headers(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.headers[..self.header_count].iter().copied()
    }
}

// Counting and storing use the same `push` calls so requirement measurement
// cannot drift from request construction.
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
                // Continue advancing after overflow to report the exact size.
                // Partial request bytes are never returned to the host.
                if end <= bytes.len() {
                    bytes[*position..end].copy_from_slice(value.as_bytes());
                }
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
            Self::Storing { bytes, position } if position <= bytes.len() => {
                Some(&bytes[..position])
            }
            Self::Counting(_) | Self::Storing { .. } => None,
        }
    }
}

pub(crate) fn text(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).expect("request construction writes UTF-8")
}

// Unlike the fixed-width date fields in `time`, range offsets need the shortest
// decimal representation. This buffer owns that representation without allocating.
pub(crate) struct U64Decimal {
    bytes: [u8; 20],
    start: usize,
}

impl U64Decimal {
    pub(crate) fn new(mut value: u64) -> Self {
        let mut bytes = [0; 20];
        let mut start = bytes.len();
        loop {
            start -= 1;
            bytes[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        Self { bytes, start }
    }

    pub(crate) fn as_str(&self) -> &str {
        text(&self.bytes[self.start..])
    }
}

#[cfg(test)]
mod tests {
    use super::{U64Decimal, Writer};

    #[test]
    fn counting_and_storing_measure_the_same_writes() {
        let mut counting = Writer::counting();
        counting.push("one");
        counting.push("é");

        let mut bytes = [0; 5];
        let mut storing = Writer::storing(&mut bytes);
        storing.push("one");
        storing.push("é");

        assert_eq!(counting.position(), storing.position());
        assert_eq!(storing.finish().unwrap(), "oneé".as_bytes());
    }

    #[test]
    fn an_undersized_writer_still_reports_the_exact_requirement() {
        let mut bytes = [0; 3];
        let mut storing = Writer::storing(&mut bytes);
        storing.push("four");
        storing.push(" more");

        assert_eq!(storing.position(), 9);
        assert!(storing.finish().is_none());
    }

    #[test]
    fn formats_the_full_u64_range() {
        assert_eq!(U64Decimal::new(0).as_str(), "0");
        assert_eq!(U64Decimal::new(u64::MAX).as_str(), "18446744073709551615");
    }
}
