/// What a GET plan asks Azure to return.
///
/// Stable scheduler intent: the provider decides the lowering, which is a HEAD
/// request for [`GetKind::Metadata`] on Azure Blob Storage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GetKind {
    /// Object bytes.
    #[default]
    Bytes,
    /// Object metadata without a body.
    Metadata,
}

/// The byte range a plan requests, in stored bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RequestedRange {
    /// Every stored byte.
    #[default]
    Whole,
    /// A half-open interval whose end is excluded.
    Bounded {
        /// First requested byte.
        start: u64,
        /// One past the last requested byte.
        end: u64,
    },
    /// All bytes beginning at this offset.
    Offset(u64),
    /// The final number of bytes (`Range: bytes=-N`), which Azure does not support.
    Suffix(u64),
}

/// Which ETag precondition a plan carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConditionKind {
    /// The request is unconditional.
    #[default]
    None,
    /// The request succeeds only when the current ETag matches.
    IfMatch,
    /// The request succeeds only when the current ETag differs.
    IfNoneMatch,
}

/// The scalar fetch plan: protocol facts only, no policy and no borrows.
///
/// A scheduler stores this beside its own state and hands it back both to
/// [`Blobs::encode_get`](crate::Blobs::encode_get) and to
/// [`Blobs::accept_get_head`](crate::Blobs::accept_get_head), which is what binds a
/// response to the request that produced it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GetShape {
    /// Whether the plan wants bytes or metadata.
    pub kind: GetKind,
    /// The requested byte range.
    pub range: RequestedRange,
    /// Which precondition the plan carries, if any.
    pub condition_kind: ConditionKind,
}

/// The byte-bearing plan view, reconstructed by the host and never stored.
///
/// The fields are public because schedulers build these from their own tables;
/// [`Blobs::encode_get`](crate::Blobs::encode_get) is therefore the point where
/// a plan is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalGet<'h> {
    /// The object key, unencoded.
    pub key: &'h str,
    /// The ETag the precondition compares against.
    pub condition_value: Option<&'h [u8]>,
    /// The scalar part of the same plan.
    pub shape: GetShape,
}

impl<'h> PhysicalGet<'h> {
    /// Plans an unconditional read of every byte of `key`.
    pub fn new(key: &'h str) -> Self {
        Self {
            key,
            condition_value: None,
            shape: GetShape::default(),
        }
    }
}
