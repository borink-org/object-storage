use core::ops::Range;

/// Byte range requested from Azure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetRange {
    /// Half-open interval whose end is excluded.
    Bounded(Range<u64>),
    /// All bytes beginning at this offset.
    Offset(u64),
    /// The final number of bytes (`Range: bytes=-N`), which Azure does not support.
    Suffix(u64),
}

/// Options applied to a GET or HEAD request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GetOptions<'a> {
    /// Request succeeds only when the current ETag matches.
    pub if_match: Option<&'a str>,
    /// Request succeeds only when the current ETag differs.
    pub if_none_match: Option<&'a str>,
    /// Optional byte range.
    pub range: Option<GetRange>,
    /// Uses HEAD instead of GET when true.
    pub head: bool,
}

/// Metadata borrowed from a successful Azure response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectMeta<'a> {
    /// Total object size, not merely the returned range length.
    pub size: u64,
    /// Entity tag when Azure returned one.
    pub e_tag: Option<&'a str>,
    /// Azure blob version identifier when returned.
    pub version: Option<&'a str>,
}
