/// What a plan asks the service to return.
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

/// The part of a plan that holds no borrows.
///
/// [`PhysicalGet::shape`] returns this, and [`PhysicalGet::from_shape`] takes
/// it back. Between those two calls you can store it: it is [`Copy`] and has
/// no lifetime, so it outlives the key and ETag bytes that the plan borrows.
///
/// [`Blobs::accept_get_head`](crate::Blobs::accept_get_head) needs only this
/// part, so you can read a response without rebuilding the whole plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GetShape {
    /// Whether the plan asks for bytes or for metadata.
    pub kind: GetKind,
    /// The byte range that the plan requests.
    pub range: RequestedRange,
    /// The precondition that the plan carries.
    pub condition: ConditionKind,
}

/// A complete plan for one read.
///
/// Build this immediately before each call and let it go afterwards. It
/// borrows the key and the ETag, so it cannot be stored across a request. To
/// keep a plan while a request is in flight, store [`PhysicalGet::shape`] and
/// your own copy of the bytes.
///
/// Because the fields are public and unchecked,
/// [`Blobs::encode_get`](crate::Blobs::encode_get) validates the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalGet<'h> {
    /// The object key, before percent-encoding.
    pub key: &'h str,
    /// Whether the plan asks for bytes or for metadata.
    pub kind: GetKind,
    /// The byte range that the plan requests.
    pub range: RequestedRange,
    /// The precondition that the plan carries.
    pub condition: ConditionKind,
    /// The ETag that the precondition compares against.
    ///
    /// This must be present if `condition` is not [`ConditionKind::None`], and
    /// absent if it is.
    pub condition_value: Option<&'h [u8]>,
}

impl<'h> PhysicalGet<'h> {
    /// Creates a plan that reads every byte of `key` with no precondition.
    pub fn new(key: &'h str) -> Self {
        Self {
            key,
            kind: GetKind::default(),
            range: RequestedRange::default(),
            condition: ConditionKind::default(),
            condition_value: None,
        }
    }

    /// Rebuilds a plan from a stored [`GetShape`] and the bytes it needs.
    pub fn from_shape(shape: GetShape, key: &'h str, condition_value: Option<&'h [u8]>) -> Self {
        Self {
            key,
            kind: shape.kind,
            range: shape.range,
            condition: shape.condition,
            condition_value,
        }
    }

    /// Returns the part of this plan that you can store.
    ///
    /// Pass the result to
    /// [`Blobs::accept_get_head`](crate::Blobs::accept_get_head) when the
    /// response arrives.
    pub fn shape(&self) -> GetShape {
        GetShape {
            kind: self.kind,
            range: self.range,
            condition: self.condition,
        }
    }
}
