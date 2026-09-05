/// What a plan asks the service to return.
///
/// The provider chooses the request that delivers it. Azure Blob Storage sends
/// a HEAD request for [`GetKind::Metadata`] and a GET request for
/// [`GetKind::Bytes`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum GetKind {
    /// The bytes of the object.
    #[default]
    Bytes = 1,
    /// The metadata of the object, without its bytes.
    Metadata = 2,
}

impl GetKind {
    /// Returns the kind with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    /// A caller that carries a plan across a language boundary sends each
    /// value as its number, and refuses a number that names nothing here.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Bytes,
            2 => Self::Metadata,
            _ => return None,
        })
    }
}

/// Which form of byte range a plan requests, without its offsets.
///
/// [`RequestedRange`] carries the offsets as well, which a number cannot. This
/// is the part of it that is one value. Pair it with the offsets in
/// [`RequestedRange::from_parts`] to carry a plan across a language boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum RangeForm {
    /// [`RequestedRange::Whole`].
    #[default]
    Whole = 1,
    /// [`RequestedRange::Bounded`].
    Bounded = 2,
    /// [`RequestedRange::Offset`].
    Offset = 3,
    /// [`RequestedRange::Suffix`].
    Suffix = 4,
}

impl RangeForm {
    /// Returns the form with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Whole,
            2 => Self::Bounded,
            3 => Self::Offset,
            4 => Self::Suffix,
            _ => return None,
        })
    }
}

/// The byte range that a plan requests.
///
/// The offsets count the stored bytes of the object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
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

impl RequestedRange {
    /// Returns which form of range this is, without its offsets.
    pub const fn form(self) -> RangeForm {
        match self {
            Self::Whole => RangeForm::Whole,
            Self::Bounded { .. } => RangeForm::Bounded,
            Self::Offset(_) => RangeForm::Offset,
            Self::Suffix(_) => RangeForm::Suffix,
        }
    }

    /// Rebuilds a range from its form and its two offsets.
    ///
    /// `start` and `end` are the fields of [`Self::Bounded`], which is the
    /// only form that reads both. [`RangeForm::Offset`] and
    /// [`RangeForm::Suffix`] read `start` and **ignore `end`**;
    /// [`RangeForm::Whole`] ignores both. A value in an ignored offset is
    /// dropped rather than refused, because the form alone says which offsets
    /// the range has.
    ///
    /// [`Self::form`] and this method are inverses: a range taken apart by one
    /// and rebuilt by the other is the range it started as.
    pub const fn from_parts(form: RangeForm, start: u64, end: u64) -> Self {
        match form {
            RangeForm::Whole => Self::Whole,
            RangeForm::Bounded => Self::Bounded { start, end },
            RangeForm::Offset => Self::Offset(start),
            RangeForm::Suffix => Self::Suffix(start),
        }
    }
}

/// The ETag precondition that a plan carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum ConditionKind {
    /// The request carries no precondition.
    #[default]
    None = 1,
    /// The request succeeds only if the current ETag matches.
    IfMatch = 2,
    /// The request succeeds only if the current ETag differs.
    IfNoneMatch = 3,
}

impl ConditionKind {
    /// Returns the precondition with this discriminant.
    ///
    /// Returns [`None`](Option::None) for a discriminant that this version
    /// does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::None,
            2 => Self::IfMatch,
            3 => Self::IfNoneMatch,
            _ => return None,
        })
    }
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
    ///
    /// At most 1024 UTF-16 code units, so a character outside the basic plane
    /// counts twice, and no control character: Azure refuses those.
    ///
    /// A segment that ends in `.` is refused as well, because Azure stores the
    /// name without that dot, and so is a `.` or `..` segment, because a host
    /// resolves those out of the URL before it sends the request. Each would
    /// name an object other than the one asked for.
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

/// The part of a write plan that holds no borrows.
///
/// This is [`Copy`] and has no lifetime, so you can store it. Pass it to
/// [`Blobs::accept_put_head`](crate::Blobs::accept_put_head) to read the
/// response that answers the write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PutShape {
    /// The condition that the write carries.
    pub condition: ConditionKind,
}

/// One write of one object.
///
/// The write sends the whole object in one request. Pass the content to
/// [`Blobs::encode_put`](crate::Blobs::encode_put), which states its length in
/// the request head and borrows the bytes.
///
/// # Writing only if the object is absent
///
/// Set `condition` to [`ConditionKind::IfNoneMatch`] and `condition_value` to
/// `*`. Azure then refuses a write that would replace an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalPut<'h> {
    /// The object key, within the container.
    ///
    /// At most 1024 UTF-16 code units, so a character outside the basic plane
    /// counts twice, and no control character: Azure refuses those.
    ///
    /// A segment that ends in `.` is refused as well, because Azure stores the
    /// name without that dot, and so is a `.` or `..` segment, because a host
    /// resolves those out of the URL before it sends the request. Each would
    /// name an object other than the one asked for.
    pub key: &'h str,
    /// The condition that the write carries.
    pub condition: ConditionKind,
    /// The entity tag that `condition` compares against, or `*`.
    pub condition_value: Option<&'h [u8]>,
}

impl<'h> PhysicalPut<'h> {
    /// Creates a plan that writes this object with no condition.
    pub fn new(key: &'h str) -> Self {
        Self {
            key,
            condition: ConditionKind::None,
            condition_value: None,
        }
    }

    /// Creates a plan from a stored shape and the bytes that it needs.
    pub fn from_shape(shape: PutShape, key: &'h str, condition_value: Option<&'h [u8]>) -> Self {
        Self {
            key,
            condition: shape.condition,
            condition_value,
        }
    }

    /// Returns the part of this plan that holds no borrows.
    pub fn shape(&self) -> PutShape {
        PutShape {
            condition: self.condition,
        }
    }
}

/// The content of a write, and where it comes from.
///
/// A write states how long its content is, so this always names a length.
/// Azure refuses a write whose head does not state it, so content of an
/// unknown length cannot be written in one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Payload<'b> {
    /// Content that you hold. The request lends these bytes and copies none.
    Slice(&'b [u8]),
    /// Content that you send yourself, of the length that you state.
    ///
    /// Use this to write from a file, a socket, or anything else that you do
    /// not hold in memory. The request then carries no content, and you give
    /// your HTTP client the same number of bytes that you state here.
    Streamed {
        /// The number of bytes that you will send.
        len: u64,
    },
}

impl<'b> Payload<'b> {
    /// Returns the number of bytes of content.
    pub fn len(&self) -> u64 {
        match *self {
            Self::Slice(bytes) => bytes.len() as u64,
            Self::Streamed { len } => len,
        }
    }

    /// Returns `true` if the content is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the content, if you passed it as [`Self::Slice`].
    ///
    /// Returns [`None`] for [`Self::Streamed`], where you hold the content.
    pub fn bytes(&self) -> Option<&'b [u8]> {
        match *self {
            Self::Slice(bytes) => Some(bytes),
            Self::Streamed { .. } => None,
        }
    }
}

impl<'b> From<&'b [u8]> for Payload<'b> {
    fn from(bytes: &'b [u8]) -> Self {
        Self::Slice(bytes)
    }
}

/// What a removal takes with it.
///
/// Azure keeps an object's snapshots separately from the object, and refuses
/// to remove an object whose snapshots would be left behind. Say here what you
/// mean, so a removal never takes more than you asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum DeleteKind {
    /// Remove the object alone.
    ///
    /// Azure refuses this if the object has snapshots.
    #[default]
    Object = 1,
    /// Remove the object and its snapshots.
    ObjectAndSnapshots = 2,
    /// Remove the snapshots and keep the object.
    SnapshotsOnly = 3,
}

impl DeleteKind {
    /// Returns the kind with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Object,
            2 => Self::ObjectAndSnapshots,
            3 => Self::SnapshotsOnly,
            _ => return None,
        })
    }
}

/// The part of a removal plan that holds no borrows.
///
/// This is [`Copy`] and has no lifetime, so you can store it. Pass it to
/// [`Blobs::accept_delete_head`](crate::Blobs::accept_delete_head) to read the
/// response that answers the removal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteShape {
    /// What the removal takes with it.
    pub kind: DeleteKind,
    /// The condition that the removal carries.
    pub condition: ConditionKind,
}

/// One removal of one object.
///
/// A removal takes only what [`DeleteKind`] names. The default takes the
/// object alone, and Azure refuses it if that would leave snapshots behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalDelete<'h> {
    /// The object key, within the container.
    ///
    /// At most 1024 UTF-16 code units, so a character outside the basic plane
    /// counts twice, and no control character: Azure refuses those.
    ///
    /// A segment that ends in `.` is refused as well, because Azure stores the
    /// name without that dot, and so is a `.` or `..` segment, because a host
    /// resolves those out of the URL before it sends the request. Each would
    /// name an object other than the one asked for.
    pub key: &'h str,
    /// What the removal takes with it.
    pub kind: DeleteKind,
    /// The condition that the removal carries.
    pub condition: ConditionKind,
    /// The entity tag that `condition` compares against.
    pub condition_value: Option<&'h [u8]>,
}

impl<'h> PhysicalDelete<'h> {
    /// Creates a plan that removes this object alone, with no condition.
    pub fn new(key: &'h str) -> Self {
        Self {
            key,
            kind: DeleteKind::Object,
            condition: ConditionKind::None,
            condition_value: None,
        }
    }

    /// Creates a plan from a stored shape and the bytes that it needs.
    pub fn from_shape(shape: DeleteShape, key: &'h str, condition_value: Option<&'h [u8]>) -> Self {
        Self {
            key,
            kind: shape.kind,
            condition: shape.condition,
            condition_value,
        }
    }

    /// Returns the part of this plan that holds no borrows.
    pub fn shape(&self) -> DeleteShape {
        DeleteShape {
            kind: self.kind,
            condition: self.condition,
        }
    }
}

/// How a listing groups the keys it reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum EntryKind {
    /// One object.
    #[default]
    Object = 1,
    /// A group of keys that a delimited listing did not report one by one.
    ///
    /// The listing reports the shared start of those keys once, and you list
    /// again with it as the prefix to see what is under it.
    Prefix = 2,
    /// A directory that the service keeps as its own entry.
    ///
    /// Only an Azure account with a hierarchical namespace reports these. A
    /// flat account reports a group of keys as [`Self::Prefix`] instead.
    Directory = 3,
}

impl EntryKind {
    /// Returns the kind with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Object,
            2 => Self::Prefix,
            3 => Self::Directory,
            _ => return None,
        })
    }
}

/// The part of a listing plan that holds no borrows.
///
/// Store it while the request is in flight, then pass it back to
/// [`PhysicalList::from_shape`] with the prefix and the marker to plan the
/// next page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListShape {
    /// Whether the listing groups keys at each `/` after the prefix.
    pub delimited: bool,
    /// The most entries that one page reports.
    pub max_results: Option<u32>,
}

/// One page of a listing.
///
/// A page is one request. The response names where the next page starts, and
/// you plan that page with the same shape and that marker.
///
/// Because the fields are public and unchecked,
/// [`Blobs::encode_list`](crate::Blobs::encode_list) validates the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalList<'h> {
    /// The keys to list under. An empty prefix lists the whole container.
    ///
    /// The prefix is matched byte for byte and is not a path: this crate adds
    /// no `/` to it. To list one directory of a delimited listing, end the
    /// prefix with the delimiter yourself.
    pub prefix: &'h str,
    /// Where the previous page ended.
    ///
    /// Pass the [`Listing::next_marker`](crate::Listing::next_marker) that the
    /// previous page reported. The first page carries [`None`]. The text is
    /// the service's, and means nothing to this crate.
    pub marker: Option<&'h str>,
    /// Whether to group the keys at each `/` after the prefix.
    ///
    /// A delimited listing reports each group once, as an
    /// [`EntryKind::Prefix`] entry, instead of reporting every key in it. This
    /// is how a listing walks one level of a hierarchy at a time.
    pub delimited: bool,
    /// The most entries that this page reports.
    ///
    /// [`None`] asks for the service's maximum, which Azure also applies to
    /// any larger number. The service may report fewer entries than this and
    /// still name a next page.
    pub max_results: Option<u32>,
}

impl<'h> PhysicalList<'h> {
    /// Creates a plan for the first page of an undelimited listing.
    pub fn new(prefix: &'h str) -> Self {
        Self {
            prefix,
            marker: None,
            delimited: false,
            max_results: None,
        }
    }

    /// Creates a plan from a stored shape and the text that it needs.
    pub fn from_shape(shape: ListShape, prefix: &'h str, marker: Option<&'h str>) -> Self {
        Self {
            prefix,
            marker,
            delimited: shape.delimited,
            max_results: shape.max_results,
        }
    }

    /// Returns the part of this plan that holds no borrows.
    pub fn shape(&self) -> ListShape {
        ListShape {
            delimited: self.delimited,
            max_results: self.max_results,
        }
    }
}

/// One entry of a listing page.
///
/// Every slice points into the body that
/// [`Blobs::fill_listing`](crate::Blobs::fill_listing) read, and stays valid
/// until you reuse that buffer.
///
/// The fields hold the text that the service wrote, which is what
/// [`ObjectMeta`] holds as bytes. Read `last_modified` with
/// [`layered::http_date_ms`](crate::layered::http_date_ms).
///
/// [`ObjectMeta`]: crate::ObjectMeta
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListEntry<'b> {
    /// Whether this entry is an object, a group of keys, or a directory.
    pub kind: EntryKind,
    /// The object key, the shared start of the group, or the directory path.
    pub key: &'b str,
    /// The size of the object. [`None`] for a group and for a directory.
    pub size: Option<u64>,
    /// The entity tag, as the listing wrote it.
    ///
    /// Azure lists an entity tag without the quotes that the `ETag` header
    /// carries, and conditions a request on either form. To write the one that
    /// HTTP defines, quote it with
    /// [`layered::quoted_etag`](crate::layered::quoted_etag).
    pub e_tag: Option<&'b str>,
    /// The value that the listing gave for the last modification, in the form
    /// that the `Last-Modified` header uses.
    pub last_modified: Option<&'b str>,
    /// This entry as the service wrote it, from its opening tag to its closing
    /// one.
    ///
    /// Read a value that the fields above do not carry with [`Self::property`]
    /// or [`Self::properties`], which read these bytes.
    ///
    /// Reading the page decoded the key, the entity tag and the date in place
    /// and set the bytes each no longer needed to zero.
    pub raw: &'b [u8],
}

impl<'b> ListEntry<'b> {
    /// Returns the value that this entry gave for one property.
    ///
    /// The name is matched exactly, against the elements of the entry and of
    /// its properties element: `AccessTier` and `Creation-Time` on Azure,
    /// `StorageClass` on S3. Returns [`None`] if the entry gave no such
    /// property.
    ///
    /// Each call reads the entry again, so read more than one or two with
    /// [`Self::properties`], which reads it once.
    ///
    /// The value is the bytes between the two tags, as the service wrote them.
    /// A value that holds `&amp;` or another reference is decoded by
    /// [`layered::decode_into`](crate::layered::decode_into).
    ///
    /// The key, the entity tag and the date were decoded when the page was
    /// read, so for those three elements this reports the decoded text and
    /// not what the service wrote. Read them from `key`, `e_tag` and
    /// `last_modified` instead.
    pub fn property(&self, name: &str) -> Option<&'b [u8]> {
        self.properties()
            .find(|(found, _)| *found == name.as_bytes())
            .map(|(_, value)| value)
    }

    /// Returns every property of this entry, in the order it wrote them.
    ///
    /// One walk reads the entry once, whatever it holds. The values are the
    /// bytes between the tags, under the rules that [`Self::property`] states.
    /// An element that holds other elements, such as the metadata of a blob,
    /// reports those bytes as its value.
    pub fn properties(&self) -> Properties<'b> {
        Properties::new(self.raw)
    }
}

/// The properties of one entry, as the service wrote them.
///
/// [`ListEntry::properties`] hands this out. It reads the entry as it goes and
/// keeps only where it stopped. One walk is therefore one pass over the entry,
/// and copying this value starts a second walk from the same place.
#[derive(Debug, Clone, Copy)]
pub struct Properties<'b> {
    // What is left of the entry: the next element, or its closing tag.
    rest: &'b [u8],
    // Whether the walk stands inside the properties element.
    within: bool,
}

impl<'b> Properties<'b> {
    /// Creates a walk over the bytes of one entry.
    ///
    /// Pass [`ListEntry::raw`]. The walk starts after the entry's own opening
    /// tag, so it reports what the entry holds and not the entry itself.
    pub fn new(raw: &'b [u8]) -> Self {
        Self {
            rest: crate::xml::after_opening_tag(raw).unwrap_or_default(),
            within: false,
        }
    }

    /// Creates a walk from the values that [`Self::remaining`] and
    /// [`Self::within`] returned.
    ///
    /// Use it to rebuild a walk that you kept as those two values, over the
    /// entry they came from. A walk built from other bytes reports whatever
    /// elements they hold, and one over another entry reports that entry.
    pub const fn from_parts(remaining: &'b [u8], within: bool) -> Self {
        Self {
            rest: remaining,
            within,
        }
    }

    /// Returns the bytes of the entry that this walk has not read.
    pub const fn remaining(self) -> &'b [u8] {
        self.rest
    }

    /// Returns whether the walk stands inside the properties element.
    pub const fn within(self) -> bool {
        self.within
    }
}

impl<'b> Iterator for Properties<'b> {
    type Item = (&'b [u8], &'b [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        crate::xml::next_property(&mut self.rest, &mut self.within)
    }
}

#[cfg(test)]
mod tests {
    use super::{ConditionKind, DeleteKind, EntryKind, GetKind, RangeForm, RequestedRange};

    // The tables are hand-written, so a number that names the wrong value is
    // the bug worth checking for.
    #[test]
    fn each_number_names_the_value_it_projects_from() {
        for (form, range) in [
            (RangeForm::Whole, RequestedRange::Whole),
            (
                RangeForm::Bounded,
                RequestedRange::Bounded { start: 2, end: 6 },
            ),
            (RangeForm::Offset, RequestedRange::Offset(2)),
            (RangeForm::Suffix, RequestedRange::Suffix(2)),
        ] {
            assert_eq!(range.form(), form);
            assert_eq!(RangeForm::from_discriminant(form as u16), Some(form));
            assert_eq!(RequestedRange::from_parts(form, 2, 6), range);
        }
        for kind in [GetKind::Bytes, GetKind::Metadata] {
            assert_eq!(GetKind::from_discriminant(kind as u16), Some(kind));
        }
        for condition in [
            ConditionKind::None,
            ConditionKind::IfMatch,
            ConditionKind::IfNoneMatch,
        ] {
            assert_eq!(
                ConditionKind::from_discriminant(condition as u16),
                Some(condition)
            );
        }
        for kind in [
            DeleteKind::Object,
            DeleteKind::ObjectAndSnapshots,
            DeleteKind::SnapshotsOnly,
        ] {
            assert_eq!(DeleteKind::from_discriminant(kind as u16), Some(kind));
        }

        for kind in [EntryKind::Object, EntryKind::Prefix, EntryKind::Directory] {
            assert_eq!(EntryKind::from_discriminant(kind as u16), Some(kind));
        }

        // 0 is the twins' "absent", so no plan value may claim it.
        assert_eq!(GetKind::from_discriminant(0), None);
        assert_eq!(ConditionKind::from_discriminant(0), None);
        assert_eq!(DeleteKind::from_discriminant(0), None);
        assert_eq!(RangeForm::from_discriminant(0), None);
        assert_eq!(EntryKind::from_discriminant(0), None);
    }
}

/// An element that a listing writes for a blob, other than the four that
/// every [`ListEntry`] carries.
///
/// Name the ones you want in a [`PropertySet`] and read a page with
/// [`Blobs::fill_listing_with`](crate::Blobs::fill_listing_with), which
/// hands you their values as it goes. Most are written under the properties
/// element; the ones marked otherwise stand beside it. Read anything that is
/// not listed here with [`ListEntry::properties`].
///
/// The page reader matches each of these by its whole start tag, in
/// `xml/azure.rs`. A property added here is added to that match too, and a
/// test there checks that every one of these is matched.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlobProperty {
    /// The access tier: `Hot`, `Cool`, `Cold` or `Archive`.
    AccessTier,
    /// Whether the tier was inferred rather than set.
    AccessTierInferred,
    /// When the tier was last changed.
    AccessTierChangeTime,
    /// The progress of a rehydration out of the archive tier.
    ArchiveStatus,
    /// The access control list, on a hierarchical account listing with permissions.
    Acl,
    /// `BlockBlob`, `PageBlob` or `AppendBlob`.
    BlobType,
    /// When the blob was created, in the form of the `Last-Modified` header.
    CreationTime,
    /// The media type, as stored with the blob.
    ContentType,
    /// The content encoding, as stored with the blob.
    ContentEncoding,
    /// The content language, as stored with the blob.
    ContentLanguage,
    /// The CRC64 of the content, if the service holds one.
    ContentCrc64,
    /// The MD5 of the content, base64, if the service holds one.
    ContentMd5,
    /// The cache control directives, as stored with the blob.
    CacheControl,
    /// The content disposition, as stored with the blob.
    ContentDisposition,
    /// The identifier of the last copy operation onto this blob.
    CopyId,
    /// The state of that copy: `pending`, `success`, `aborted` or `failed`.
    CopyStatus,
    /// The URL that copy read from.
    CopySource,
    /// The bytes copied so far and the total, as `copied/total`.
    CopyProgress,
    /// When that copy finished.
    CopyCompletionTime,
    /// Why that copy failed or was aborted.
    CopyStatusDescription,
    /// When a soft-deleted blob was deleted.
    DeletedTime,
    /// Whether the entry is a soft-deleted blob. Written beside the properties element.
    Deleted,
    /// The encryption scope the blob is stored under.
    EncryptionScope,
    /// When the blob expires, on a hierarchical account.
    ExpiryTime,
    /// The owning group, on a hierarchical account listing with permissions.
    Group,
    /// Whether this version is the current one. Written beside the properties element.
    IsCurrentVersion,
    /// Whether the blob is an incremental copy of a page blob snapshot.
    IncrementalCopy,
    /// Until when the immutability policy holds.
    ImmutabilityPolicyUntilDate,
    /// The immutability policy: `unlocked` or `locked`.
    ImmutabilityPolicyMode,
    /// Whether the blob is leased: `locked` or `unlocked`.
    LeaseStatus,
    /// The state of the lease: `available`, `leased`, `expired`, `breaking` or `broken`.
    LeaseState,
    /// Whether the lease is `infinite` or `fixed`.
    LeaseDuration,
    /// Whether a legal hold is set.
    LegalHold,
    /// The owner, on a hierarchical account listing with permissions.
    Owner,
    /// The POSIX permissions, on a hierarchical account listing with permissions.
    Permissions,
    /// How many days a soft-deleted blob is kept.
    RemainingRetentionDays,
    /// The priority of a rehydration out of the archive tier.
    RehydratePriority,
    /// Whether the blob is encrypted at rest.
    ServerEncrypted,
    /// The snapshot's timestamp, on an entry that names a snapshot. Written beside the properties element.
    Snapshot,
    /// How many tags the blob has.
    TagCount,
    /// The version's identifier, on an account that keeps versions. Written beside the properties element.
    VersionId,
    /// The sequence number of a page blob.
    BlobSequenceNumber,
}

impl BlobProperty {
    /// Every property, in the order of their numbers.
    pub const ALL: &[Self] = &[
        Self::AccessTier,
        Self::AccessTierInferred,
        Self::AccessTierChangeTime,
        Self::ArchiveStatus,
        Self::Acl,
        Self::BlobType,
        Self::CreationTime,
        Self::ContentType,
        Self::ContentEncoding,
        Self::ContentLanguage,
        Self::ContentCrc64,
        Self::ContentMd5,
        Self::CacheControl,
        Self::ContentDisposition,
        Self::CopyId,
        Self::CopyStatus,
        Self::CopySource,
        Self::CopyProgress,
        Self::CopyCompletionTime,
        Self::CopyStatusDescription,
        Self::DeletedTime,
        Self::Deleted,
        Self::EncryptionScope,
        Self::ExpiryTime,
        Self::Group,
        Self::IsCurrentVersion,
        Self::IncrementalCopy,
        Self::ImmutabilityPolicyUntilDate,
        Self::ImmutabilityPolicyMode,
        Self::LeaseStatus,
        Self::LeaseState,
        Self::LeaseDuration,
        Self::LegalHold,
        Self::Owner,
        Self::Permissions,
        Self::RemainingRetentionDays,
        Self::RehydratePriority,
        Self::ServerEncrypted,
        Self::Snapshot,
        Self::TagCount,
        Self::VersionId,
        Self::BlobSequenceNumber,
    ];

    /// The element name, as the service writes it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::AccessTier => "AccessTier",
            Self::AccessTierInferred => "AccessTierInferred",
            Self::AccessTierChangeTime => "AccessTierChangeTime",
            Self::ArchiveStatus => "ArchiveStatus",
            Self::Acl => "Acl",
            Self::BlobType => "BlobType",
            Self::CreationTime => "Creation-Time",
            Self::ContentType => "Content-Type",
            Self::ContentEncoding => "Content-Encoding",
            Self::ContentLanguage => "Content-Language",
            Self::ContentCrc64 => "Content-CRC64",
            Self::ContentMd5 => "Content-MD5",
            Self::CacheControl => "Cache-Control",
            Self::ContentDisposition => "Content-Disposition",
            Self::CopyId => "CopyId",
            Self::CopyStatus => "CopyStatus",
            Self::CopySource => "CopySource",
            Self::CopyProgress => "CopyProgress",
            Self::CopyCompletionTime => "CopyCompletionTime",
            Self::CopyStatusDescription => "CopyStatusDescription",
            Self::DeletedTime => "DeletedTime",
            Self::Deleted => "Deleted",
            Self::EncryptionScope => "EncryptionScope",
            Self::ExpiryTime => "Expiry-Time",
            Self::Group => "Group",
            Self::IsCurrentVersion => "IsCurrentVersion",
            Self::IncrementalCopy => "IncrementalCopy",
            Self::ImmutabilityPolicyUntilDate => "ImmutabilityPolicyUntilDate",
            Self::ImmutabilityPolicyMode => "ImmutabilityPolicyMode",
            Self::LeaseStatus => "LeaseStatus",
            Self::LeaseState => "LeaseState",
            Self::LeaseDuration => "LeaseDuration",
            Self::LegalHold => "LegalHold",
            Self::Owner => "Owner",
            Self::Permissions => "Permissions",
            Self::RemainingRetentionDays => "RemainingRetentionDays",
            Self::RehydratePriority => "RehydratePriority",
            Self::ServerEncrypted => "ServerEncrypted",
            Self::Snapshot => "Snapshot",
            Self::TagCount => "TagCount",
            Self::VersionId => "VersionId",
            Self::BlobSequenceNumber => "x-ms-blob-sequence-number",
        }
    }
}

// A set holds one bit per property.
const _: () = assert!(BlobProperty::ALL.len() <= 64);

impl BlobProperty {
    /// How many properties there are, which is the most a set can hold.
    pub const COUNT: usize = Self::ALL.len();

    // The property that an element name stands for, if it is one of these.
    // This is the slow way to find out and is used only where a tag was not
    // matched whole: an unusual spelling, or an element that is none of them.
    pub(crate) fn identify(name: &[u8]) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|property| property.name().as_bytes() == name)
    }

    const fn bit(self) -> u64 {
        1 << (self as u8)
    }
}

/// The properties that one page read is asked for.
///
/// Build one with [`Self::of`] and pass it to
/// [`Blobs::fill_listing_with`](crate::Blobs::fill_listing_with). The values
/// come back in the order that [`BlobProperty`] lists them, whatever order
/// the set was built in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PropertySet(u64);

impl PropertySet {
    /// A set of these properties. Naming one twice is the same as once.
    pub const fn of(properties: &[BlobProperty]) -> Self {
        let mut mask = 0;
        let mut i = 0;
        while i < properties.len() {
            mask |= properties[i].bit();
            i += 1;
        }
        Self(mask)
    }

    /// A set from its bits, one per property in the order [`BlobProperty`]
    /// numbers them. A bit that names no property is dropped.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits & ((1 << BlobProperty::COUNT) - 1))
    }

    /// The set's bits, as [`Self::from_bits`] reads them.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether the set holds this property.
    pub const fn contains(self, property: BlobProperty) -> bool {
        self.0 & property.bit() != 0
    }

    /// How many properties the set holds, which is how many values a read
    /// reports for each entry.
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether the set holds nothing.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Where a property's value stands among the values of an entry: its
    /// rank among the set's members, in the order [`BlobProperty`] lists
    /// them. Meaningful only for a property the set holds.
    pub const fn slot(self, property: BlobProperty) -> usize {
        (self.0 & (property.bit() - 1)).count_ones() as usize
    }
}

/// The values that one entry gave for the properties of a set.
///
/// [`Blobs::fill_listing_with`](crate::Blobs::fill_listing_with) hands one
/// to the closure that builds each entry. Each value is the bytes between
/// the element's tags, as the service wrote them, under the rules that
/// [`ListEntry::property`] states. A group of keys gives no values.
#[derive(Clone, Copy, Debug)]
pub struct PropertyValues<'x, 'b> {
    set: PropertySet,
    values: &'x [Option<&'b [u8]>],
}

impl<'x, 'b> PropertyValues<'x, 'b> {
    pub(crate) fn new(set: PropertySet, values: &'x [Option<&'b [u8]>]) -> Self {
        Self { set, values }
    }

    /// The set that the page was read with.
    pub const fn set(&self) -> PropertySet {
        self.set
    }

    /// The value the entry gave for one property.
    ///
    /// [`None`] if the property is not in the set or the entry wrote no such
    /// element; an empty slice if it wrote the element empty.
    pub fn get(&self, property: BlobProperty) -> Option<&'b [u8]> {
        if !self.set.contains(property) {
            return None;
        }
        self.values[self.set.slot(property)]
    }

    /// Every value, one per member of the set, in the order [`BlobProperty`]
    /// lists them.
    pub const fn all(&self) -> &'x [Option<&'b [u8]>] {
        self.values
    }
}

#[cfg(test)]
mod property_tests {
    use super::{BlobProperty, PropertySet};

    #[test]
    fn every_property_is_found_by_its_name_and_numbered_in_order() {
        for (index, property) in BlobProperty::ALL.iter().enumerate() {
            assert_eq!(*property as usize, index);
            assert_eq!(
                BlobProperty::identify(property.name().as_bytes()),
                Some(*property)
            );
        }
        assert_eq!(BlobProperty::identify(b"Name"), None);
        assert_eq!(BlobProperty::identify(b""), None);
    }

    #[test]
    fn a_set_numbers_its_members_in_the_order_the_enum_does() {
        let set = PropertySet::of(&[
            BlobProperty::ServerEncrypted,
            BlobProperty::AccessTier,
            BlobProperty::AccessTier,
            BlobProperty::CreationTime,
        ]);
        assert_eq!(set.len(), 3);
        assert_eq!(set.slot(BlobProperty::AccessTier), 0);
        assert_eq!(set.slot(BlobProperty::CreationTime), 1);
        assert_eq!(set.slot(BlobProperty::ServerEncrypted), 2);
        assert!(!set.contains(BlobProperty::BlobType));
        assert!(PropertySet::default().is_empty());
    }
}
