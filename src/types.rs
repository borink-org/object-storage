/// What a plan asks Azure to return.
///
/// The provider chooses the request that delivers it. Azure Blob Storage sends
/// a HEAD request for [`GetKind::Metadata`] and a GET request for
/// [`GetKind::Bytes`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GetKind {
    /// The bytes of the object.
    #[default]
    Bytes,
    /// The metadata of the object, without its bytes.
    Metadata,
}

/// The byte range that a plan requests.
///
/// The offsets count the stored bytes of the object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RequestedRange {
    /// Every byte of the object.
    #[default]
    Whole,
    /// A half-open interval that excludes its end.
    Bounded {
        /// The first byte that the plan requests.
        start: u64,
        /// The byte after the last byte that the plan requests.
        end: u64,
    },
    /// Every byte from this offset to the end of the object.
    Offset(u64),
    /// The last `n` bytes, written `Range: bytes=-N`.
    ///
    /// Azure Blob Storage does not accept this form.
    /// [`Blobs::encode_get`](crate::Blobs::encode_get) refuses it.
    Suffix(u64),
}

/// The ETag precondition that a plan carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConditionKind {
    /// The request carries no precondition.
    #[default]
    None,
    /// The request succeeds only if the current ETag matches.
    IfMatch,
    /// The request succeeds only if the current ETag differs.
    IfNoneMatch,
}

/// The part of a plan that is copyable and holds no borrows.
///
/// Store this next to your own request state. Pass it back to
/// [`Blobs::encode_get`](crate::Blobs::encode_get) and to
/// [`Blobs::accept_get_head`](crate::Blobs::accept_get_head). The second call
/// uses it to check that the response answers the request you sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GetShape {
    /// Whether the plan asks for bytes or for metadata.
    pub kind: GetKind,
    /// The byte range that the plan requests.
    pub range: RequestedRange,
    /// The precondition that the plan carries.
    pub condition_kind: ConditionKind,
}

/// A complete plan: a [`GetShape`] and the borrowed bytes that go with it.
///
/// Build this immediately before each call and let it go afterwards. Because
/// the fields are public and unchecked,
/// [`Blobs::encode_get`](crate::Blobs::encode_get) validates the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalGet<'h> {
    /// The object key, before percent-encoding.
    pub key: &'h str,
    /// The ETag that the precondition compares against.
    pub condition_value: Option<&'h [u8]>,
    /// The copyable part of the same plan.
    pub shape: GetShape,
}

impl<'h> PhysicalGet<'h> {
    /// Creates a plan that reads every byte of `key` with no precondition.
    pub fn new(key: &'h str) -> Self {
        Self {
            key,
            condition_value: None,
            shape: GetShape::default(),
        }
    }
}
