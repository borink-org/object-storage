/// What a plan asks the service to return.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum GetKind {
    /// The bytes of the object.
    #[default]
    Bytes = 1,
    /// The properties and the metadata of the object, without its bytes.
    ///
    /// Azure answers this with a HEAD request, as S3's `HeadObject` does.
    Head = 2,
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
            2 => Self::Head,
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
    ///
    /// The end must be after the start. The encoding methods refuse an empty
    /// or inverted range with
    /// [`InvalidPlan::Range`](crate::InvalidPlan::Range), so you need no check
    /// of your own: S3 would answer one with the whole object.
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

/// The precondition that a plan carries.
///
/// The plan's `condition_value` holds what the precondition compares
/// against: an entity tag, or `*`, for the first two, and an HTTP date for
/// the last two, as [`Timestamps::rfc1123`](crate::Timestamps::rfc1123)
/// writes it.
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
    /// The request succeeds only if the object changed after the date.
    ///
    /// A read that fails it is answered `304 Not Modified`. S3 takes it on a
    /// read alone.
    IfModifiedSince = 4,
    /// The request succeeds only if the object has not changed since the
    /// date.
    ///
    /// A request that fails it is answered `412 Precondition Failed`. S3
    /// takes it on a read alone.
    IfUnmodifiedSince = 5,
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
            4 => Self::IfModifiedSince,
            5 => Self::IfUnmodifiedSince,
            _ => return None,
        })
    }

    // Whether a read that fails the condition is answered 304, rather than
    // 412.
    pub(crate) const fn fails_as_not_modified(self) -> bool {
        matches!(self, Self::IfNoneMatch | Self::IfModifiedSince)
    }

    // Whether the condition compares a date rather than an entity tag.
    pub(crate) const fn is_date(self) -> bool {
        matches!(self, Self::IfModifiedSince | Self::IfUnmodifiedSince)
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

/// An earlier state of an object, which a read or a removal names instead
/// of the object as it is now.
///
/// Each carries the identifier as the service wrote it, and the request
/// sends it percent-encoded in the query. A client does not check its form:
/// the service refuses an identifier it does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Revision<'h> {
    /// A snapshot, by the timestamp that Azure returned in `x-ms-snapshot`
    /// when it took the snapshot, sent as `snapshot`. Azure only.
    Snapshot(&'h str),
    /// A version, by the identifier that the service returned in
    /// `x-ms-version-id` or `x-amz-version-id`, sent as `versionid` on
    /// Azure and `versionId` on S3.
    Version(&'h str),
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
    /// No control character: Azure refuses those. A flat-namespace account
    /// also refuses more than 1024 UTF-16 code units, where a character outside
    /// the basic plane counts twice; a client told it is on one refuses such a
    /// key itself, see [`Blobs::with_namespace`](crate::Blobs::with_namespace).
    ///
    /// A segment that ends in `.` is refused as well, because Azure stores the
    /// name without that dot, and so is a `.` or `..` segment, because a host
    /// resolves those out of the URL before it sends the request. Each would
    /// name an object other than the one asked for.
    ///
    /// An S3 client refuses a `.` or `..` segment for the same reason, and a
    /// key longer than [`s3::MAX_KEY_LEN`](crate::s3::MAX_KEY_LEN) bytes. S3
    /// takes every other key of UTF-8.
    pub key: &'h str,
    /// Whether the plan asks for bytes or for metadata.
    pub kind: GetKind,
    /// The byte range that the plan requests. An empty range is refused:
    /// see [`RequestedRange::Bounded`].
    pub range: RequestedRange,
    /// The precondition that the plan carries.
    pub condition: ConditionKind,
    /// What the precondition compares against: see [`ConditionKind`].
    ///
    /// This must be present if `condition` is not [`ConditionKind::None`], and
    /// absent if it is.
    pub condition_value: Option<&'h [u8]>,
    /// The snapshot or version to read, or [`None`] for the object as it
    /// is now. An empty identifier is refused with
    /// [`InvalidPlan::Revision`](crate::InvalidPlan::Revision), and so is a
    /// snapshot on S3.
    pub revision: Option<Revision<'h>>,
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
            revision: None,
        }
    }

    /// Creates a plan that reads the properties and the metadata of `key`,
    /// without its bytes, with no precondition.
    pub fn head(key: &'h str) -> Self {
        Self {
            kind: GetKind::Head,
            ..Self::new(key)
        }
    }

    /// Rebuilds a plan from a stored [`GetShape`] and the bytes it needs.
    ///
    /// The plan reads the object as it is now. Set [`Self::revision`] on the
    /// result to read a snapshot or a version.
    pub fn from_shape(shape: GetShape, key: &'h str, condition_value: Option<&'h [u8]>) -> Self {
        Self {
            key,
            kind: shape.kind,
            range: shape.range,
            condition: shape.condition,
            condition_value,
            revision: None,
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

/// One metadata pair of an object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetadataPair<'h> {
    /// The name, without the `x-ms-meta-` or `x-amz-meta-` prefix.
    ///
    /// On Azure, ASCII letters, digits and underscores, not starting with a
    /// digit. On S3, any character that an HTTP token holds: letters, digits
    /// and `` !#$%&'*+-.^_`|~ ``. Both services match a name without case.
    pub name: &'h str,
    /// The text stored under that name.
    ///
    /// On Azure, the text is sent as one HTTP header value, so it is ASCII,
    /// with no control character and no space at either end. Encode any
    /// other text yourself, as base64 or with percent escapes.
    ///
    /// An S3 client sends text outside ASCII or a control character as an
    /// RFC 2047 encoded word, which S3 decodes. Read it back with
    /// [`s3::metadata_value`](crate::s3::metadata_value). The client refuses
    /// a value that a read would not return exactly:
    ///
    /// - a value with CR or LF, which S3 stores as spaces;
    /// - a value with a space or a tab at either end, which a read drops;
    /// - a value with a space-separated word that starts with `=?` and ends
    ///   with `?=`, which S3 reads as an encoded word.
    pub value: &'h str,
}

/// A checksum of the content, which Azure compares against the bytes it
/// receives.
///
/// Azure refuses a write whose content does not match, with 400
/// `Md5Mismatch` or `Crc64Mismatch`. The text is base64: of sixteen bytes
/// for an MD5, of eight for a CRC64.
///
/// # What Azure stores
///
/// A whole-object write stores an MD5 whether or not you sent one, and
/// stores a CRC64 only if you sent one. A listing reports both, as
/// [`BlobProperty::ContentMd5`] and [`BlobProperty::ContentCrc64`]. A head
/// read reports the MD5 alone. An object written in blocks stores neither
/// unless the commit declares one: see [`WriteOptions::declared_md5`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransactionalChecksum<'h> {
    /// The base64 of the MD5 of the content, sent as `Content-MD5`.
    Md5(&'h str),
    /// The base64 of the CRC-64/NVME of the content.
    ///
    /// Azure takes it as `x-ms-content-crc64`, of the eight bytes in
    /// little-endian order, as [`Digest::crc64`](crate::Digest::crc64) holds
    /// them. S3 takes it as `x-amz-checksum-crc64nvme`, of the eight bytes
    /// in big-endian order.
    Crc64(&'h str),
    /// The base64 of the CRC32 of the content, sent as
    /// `x-amz-checksum-crc32`. S3 only.
    Crc32(&'h str),
    /// The base64 of the CRC32C of the content, sent as
    /// `x-amz-checksum-crc32c`. S3 only.
    Crc32c(&'h str),
    /// The base64 of the SHA-1 of the content, sent as
    /// `x-amz-checksum-sha1`. S3 only.
    Sha1(&'h str),
    /// The base64 of the SHA-256 of the content, sent as
    /// `x-amz-checksum-sha256`. S3 only.
    Sha256(&'h str),
    /// A checksum of this kind that the encoder computes and sends.
    ///
    /// The encoder computes it with the provider of that kind that
    /// [`Blobs::with_checksum`](crate::Blobs::with_checksum) or
    /// [`s3::Objects::with_checksum`](crate::s3::Objects::with_checksum)
    /// registered. It can only sum content that it holds: a
    /// [`Payload::Slice`], or the list of parts of a commit. It refuses the plan with
    /// [`InvalidPlan::Option`](crate::InvalidPlan::Option) if no provider of
    /// that kind is registered, or if the payload is [`Payload::Streamed`].
    /// For a streamed payload, compute the checksum yourself before you
    /// encode and pass the text.
    Compute(crate::checksum::ChecksumKind),
}

/// The properties that describe an object's content, which a write stores
/// and a read returns in the response head.
///
/// Each is one HTTP header value: ASCII, with no control character and no
/// space at either end. A write refuses any other value with
/// [`InvalidPlan::ContentProperty`](crate::InvalidPlan::ContentProperty),
/// except where the service returns UTF-8 as it got it: a `Content-Type` on
/// an Azure account of [`AzureNamespace::Flat`](crate::AzureNamespace::Flat),
/// and a `Content-Type` and a `Content-Disposition` on an S3 general purpose
/// bucket. For a file name outside ASCII elsewhere, write the RFC 6266 form
/// `attachment; filename*=UTF-8''%C3%A9.txt`, which is ASCII.
/// Azure takes them as `x-ms-blob-content-type`, `x-ms-blob-cache-control`
/// and so on, and S3 as the headers that name them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentProperties<'h> {
    /// `Content-Type`.
    pub content_type: Option<&'h str>,
    /// `Content-Encoding`, such as `gzip`. Neither service encodes the
    /// content: it stores the bytes as sent.
    pub content_encoding: Option<&'h str>,
    /// `Content-Language`.
    pub content_language: Option<&'h str>,
    /// `Content-Disposition`.
    pub content_disposition: Option<&'h str>,
    /// `Cache-Control`.
    pub cache_control: Option<&'h str>,
}

impl<'h> ContentProperties<'h> {
    /// Creates a set that stores no property.
    pub const fn new() -> Self {
        Self {
            content_type: None,
            content_encoding: None,
            content_language: None,
            content_disposition: None,
            cache_control: None,
        }
    }

    // The properties that the set holds, each with its lowercase header
    // name, in the order of those names.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'static str, &'h str)> {
        [
            ("cache-control", self.cache_control),
            ("content-disposition", self.content_disposition),
            ("content-encoding", self.content_encoding),
            ("content-language", self.content_language),
            ("content-type", self.content_type),
        ]
        .into_iter()
        .filter_map(|(name, value)| Some((name, value?)))
    }

    // Whether the set holds any property.
    pub(crate) fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }
}

/// One tag of an object: a key and a value, which the service indexes.
///
/// Both services take at most ten tags on an object, a key of 1 to 128
/// characters and a value of at most 256. Azure takes ASCII letters and
/// digits, space and `+-./:=_` in both. S3 takes any letter or digit, space
/// and `+-=._:/@`. A write refuses a tag outside those rules, and two tags
/// with the same key, with [`InvalidPlan::Tag`](crate::InvalidPlan::Tag). A
/// client of [`s3::Service::Compatible`](crate::s3::Service::Compatible)
/// refuses only an empty key and a repeated one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tag<'h> {
    /// The key.
    pub key: &'h str,
    /// The value.
    pub value: &'h str,
}

/// A request that replaces the tags of an object: an Azure Set Blob Tags, or
/// an S3 PutObjectTagging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysicalSetTags<'h> {
    /// The object key, under the rules of [`PhysicalGet::key`].
    pub key: &'h str,
    /// The tags that the object then holds, and no others. An empty list
    /// removes every tag.
    pub tags: &'h [Tag<'h>],
    /// A checksum of the request body for the encoder to compute and send,
    /// with the provider of that kind that the client registered.
    pub checksum: Option<crate::checksum::ChecksumKind>,
    /// The version whose tags to replace, or [`None`] for the object as it
    /// is now. Each version keeps its own tags. Azure also replaces the tags
    /// of a snapshot, apart from the object's, and S3, which keeps no
    /// snapshots, refuses one with
    /// [`InvalidPlan::Revision`](crate::InvalidPlan::Revision).
    pub revision: Option<Revision<'h>>,
}

impl<'h> PhysicalSetTags<'h> {
    /// Creates a plan that gives `key` these tags, with no checksum.
    pub const fn new(key: &'h str, tags: &'h [Tag<'h>]) -> Self {
        Self {
            key,
            tags,
            checksum: None,
            revision: None,
        }
    }
}

/// One object of a removal of several: its key, and the snapshot or version
/// to remove.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteTarget<'h> {
    /// The object key, under the rules of [`PhysicalGet::key`].
    pub key: &'h str,
    /// The snapshot or version to remove, or [`None`] for the object as it
    /// is now. Azure sends it in the path of the object's Delete Blob, and
    /// S3 as the object's `VersionId`. S3 removes no snapshot: a client
    /// refuses one with [`InvalidPlan::Revision`](crate::InvalidPlan::Revision).
    pub revision: Option<Revision<'h>>,
}

impl<'h> DeleteTarget<'h> {
    /// Names the object `key` as it is now.
    pub const fn new(key: &'h str) -> Self {
        Self {
            key,
            revision: None,
        }
    }
}

impl<'h> From<&'h str> for DeleteTarget<'h> {
    fn from(key: &'h str) -> Self {
        Self::new(key)
    }
}

/// A removal of several objects in one request: an Azure Blob Batch of
/// Delete Blob requests, or an S3 DeleteObjects.
///
/// The service removes each object on its own, and answers with a result for
/// each: see [`DeleteManyHeadOutcome`](crate::DeleteManyHeadOutcome).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysicalDeleteMany<'h> {
    /// The objects to remove. Azure takes at most 256 in one request, and
    /// S3 1,000.
    pub objects: &'h [DeleteTarget<'h>],
    /// A checksum of the request body for the encoder to compute and send,
    /// with the provider of that kind that the client registered. AWS takes
    /// a removal of several objects only with one.
    pub checksum: Option<crate::checksum::ChecksumKind>,
}

impl<'h> PhysicalDeleteMany<'h> {
    /// Creates a plan that removes these objects, with no checksum.
    pub const fn new(objects: &'h [DeleteTarget<'h>]) -> Self {
        Self {
            objects,
            checksum: None,
        }
    }
}

/// The options of a write.
///
/// [`PhysicalPut::options`], [`PhysicalCommit::options`],
/// [`azure::PhysicalStageBlock::options`](crate::azure::PhysicalStageBlock::options)
/// and [`s3::PhysicalStagePart::options`](crate::s3::PhysicalStagePart::options)
/// each hold one. Build it with `..Default::default()` or [`Self::new`], so
/// that a field added later does not break your code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOptions<'h> {
    /// A checksum of the content, which the service compares against the
    /// bytes it receives. Azure takes an MD5 or a CRC64, and so does a part
    /// on S3. A whole-object write to S3 takes any of the kinds.
    ///
    /// The content of a commit is its list of parts, so on a commit this is a
    /// checksum of that text.
    pub checksum: Option<TransactionalChecksum<'h>>,
    /// The base64 of an MD5 to store as the object's `Content-MD5`. Azure
    /// only.
    ///
    /// Azure stores this value without comparing it to the content. Only a
    /// commit takes it, because an object written in blocks stores no
    /// checksum unless the commit declares one. A whole-object write or a
    /// stage that sets it is refused with
    /// [`InvalidPlan::Option`](crate::InvalidPlan::Option), and so is any S3
    /// write that sets it.
    pub declared_md5: Option<&'h str>,
    /// The content properties to store with the object.
    ///
    /// A write of a whole object takes them, and so does the write that
    /// names the object of a write in parts: an Azure commit, or an S3
    /// CreateMultipartUpload. Any other write refuses them with
    /// [`InvalidPlan::Option`](crate::InvalidPlan::Option).
    pub properties: ContentProperties<'h>,
    /// The tags to store with the object, under the rules of [`Tag`]. The
    /// writes that take [`Self::properties`] take these.
    pub tags: &'h [Tag<'h>],
    /// The storage class on S3, such as `STANDARD_IA`, or the access tier on
    /// Azure, such as `Cool`. The writes that take [`Self::properties`]
    /// take this.
    ///
    /// The client sends any one header value, and the service refuses a name
    /// it does not have.
    pub storage_class: Option<&'h str>,
}

impl<'h> WriteOptions<'h> {
    /// Creates options that add nothing to the request.
    ///
    /// This is the same value as [`Default::default`], from a `const fn`.
    pub const fn new() -> Self {
        Self {
            checksum: None,
            declared_md5: None,
            properties: ContentProperties::new(),
            tags: &[],
            storage_class: None,
        }
    }
}

/// The extra elements that a listing asks the service to write for each
/// object.
///
/// Combine the constants with `|` and put the set in
/// [`PhysicalList::include`]. An empty set asks for nothing beyond the
/// object's own properties. Each constant says which service writes it, and
/// a client refuses a listing that asks the other service for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ListInclude(u32);

impl ListInclude {
    /// The metadata pairs of each object, as a `Metadata` element. Azure
    /// only.
    ///
    /// Read them with [`ListEntry::metadata`], or in the same pass as the
    /// rest of the page with [`BlobProperty::Metadata`].
    pub const METADATA: Self = Self(1 << 0);

    /// The owner of each object, as an `Owner` element. S3 only. Read it as
    /// [`s3::ObjectProperty::Owner`](crate::s3::ObjectProperty::Owner).
    pub const OWNER: Self = Self(1 << 1);

    /// An entry for each snapshot of an object, beside the object's own,
    /// each with a `Snapshot` element. Azure only. Read it as
    /// [`BlobProperty::Snapshot`].
    pub const SNAPSHOTS: Self = Self(1 << 2);

    /// An entry for each version of an object, with its version ID and
    /// whether it is the current one: read both with [`ListEntry::version`]
    /// and [`ListEntry::is_current_version`].
    ///
    /// Azure writes `VersionId` and, on the current version,
    /// `IsCurrentVersion` in a List Blobs. An S3 client sends a
    /// ListObjectVersions instead of a ListObjectsV2, whose entries carry
    /// `VersionId` and `IsLatest`, and which reports each delete marker as
    /// an [`EntryKind::DeleteMarker`]. It pages with a key marker and a
    /// version marker: see [`PhysicalList::version_marker`]. A directory
    /// bucket keeps no versions, so a client refuses this flag for one.
    pub const VERSIONS: Self = Self(1 << 3);

    /// Returns `true` if this set holds every flag of `other`.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns `true` if this set holds any flag of `other`.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Returns `true` if this set holds no flag.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for ListInclude {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
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
    /// No control character: Azure refuses those. A flat-namespace account
    /// also refuses more than 1024 UTF-16 code units, where a character outside
    /// the basic plane counts twice; a client told it is on one refuses such a
    /// key itself, see [`Blobs::with_namespace`](crate::Blobs::with_namespace).
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
    /// The metadata pairs to store with the object.
    ///
    /// The object then holds these pairs and no others: a write replaces the
    /// whole set.
    pub metadata: &'h [MetadataPair<'h>],
    /// The options of the write, such as a checksum of the content.
    ///
    /// [`WriteOptions::new`] adds nothing to the request.
    pub options: WriteOptions<'h>,
}

impl<'h> PhysicalPut<'h> {
    /// Creates a plan that writes this object with no condition, no metadata
    /// and no options.
    pub fn new(key: &'h str) -> Self {
        Self {
            key,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            options: WriteOptions::new(),
        }
    }

    /// Creates a plan from a stored shape and the bytes that it needs.
    ///
    /// The plan has no metadata and no options, because a shape holds no
    /// borrows and both of those borrow. Set those two fields after this
    /// call if the write needs them.
    pub fn from_shape(shape: PutShape, key: &'h str, condition_value: Option<&'h [u8]>) -> Self {
        Self {
            condition: shape.condition,
            condition_value,
            ..Self::new(key)
        }
    }

    /// Returns the part of this plan that holds no borrows.
    pub fn shape(&self) -> PutShape {
        PutShape {
            condition: self.condition,
        }
    }
}

/// The part of a [`PhysicalCommit`] that holds no borrows.
///
/// This is [`Copy`] and has no lifetime, so you can store it. Pass it to the
/// method that reads the response, such as
/// [`Blobs::accept_commit_blocks_head`](crate::Blobs::accept_commit_blocks_head).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitShape {
    /// The precondition on the object being committed.
    pub condition: ConditionKind,
}

/// One commit, which publishes an ordered list of staged parts as an object.
///
/// An object written in parts is written in three steps. Stage each part,
/// commit the list of them with this plan, and read what the service holds
/// with a listing of the parts. On Azure, a part is a block: see
/// [`Blobs::encode_commit_blocks`](crate::Blobs::encode_commit_blocks). On
/// S3, the parts belong to an upload, which you create first: see
/// [`s3::Objects::encode_commit_parts`](crate::s3::Objects::encode_commit_parts).
///
/// The list of parts is not in the plan. Pass it beside the plan: it may be
/// long, and a binding produces it rather than holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalCommit<'a> {
    /// The object key, as for [`PhysicalPut::key`].
    ///
    /// On S3 it is also the key the upload was created for.
    pub key: &'a str,
    /// The condition that the commit carries.
    ///
    /// The commit publishes the object only if the condition holds for the
    /// object that the key holds before. [`ConditionKind::IfNoneMatch`] with
    /// `*` publishes it only if the key holds none.
    pub condition: ConditionKind,
    /// The entity tag that `condition` compares against, or `*`.
    pub condition_value: Option<&'a [u8]>,
    /// The metadata pairs to store with the object.
    ///
    /// A commit replaces the whole set, as [`PhysicalPut::metadata`] does.
    /// Azure takes them here. S3 takes them when the upload is created, and
    /// refuses a commit that carries any with [`InvalidPlan::Option`].
    ///
    /// [`InvalidPlan::Option`]: crate::InvalidPlan::Option
    pub metadata: &'a [MetadataPair<'a>],
    /// The options of the commit.
    ///
    /// The content of a commit is its list of parts, so a checksum in
    /// [`WriteOptions::checksum`] is a checksum of that list as the request
    /// writes it.
    ///
    /// An object written in parts stores no checksum of its own on Azure
    /// unless the commit declares one in [`WriteOptions::declared_md5`]. S3
    /// computes an entity tag from the parts, and takes no declared MD5.
    pub options: WriteOptions<'a>,
    /// The length of the object that the commit publishes, if you know it.
    ///
    /// S3 refuses the commit with 400 `InvalidRequest` if the parts add up
    /// to another length, which catches a part left out of the list. Azure
    /// has no such check, and refuses a plan that sets it with
    /// [`InvalidPlan::Option`].
    ///
    /// [`InvalidPlan::Option`]: crate::InvalidPlan::Option
    pub size: Option<u64>,
}

impl<'a> PhysicalCommit<'a> {
    /// Creates a plan that commits parts to `key` with no condition, no
    /// metadata, no options and no size.
    pub const fn new(key: &'a str) -> Self {
        Self {
            key,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            options: WriteOptions::new(),
            size: None,
        }
    }

    /// Returns the part of this plan that holds no borrows.
    pub const fn shape(&self) -> CommitShape {
        CommitShape {
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
    /// No control character: Azure refuses those. A flat-namespace account
    /// also refuses more than 1024 UTF-16 code units, where a character outside
    /// the basic plane counts twice; a client told it is on one refuses such a
    /// key itself, see [`Blobs::with_namespace`](crate::Blobs::with_namespace).
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
    /// The snapshot or version to remove, or [`None`] for the object as it
    /// is now. A plan that names one takes [`DeleteKind::Object`], and a
    /// client refuses any other kind with
    /// [`InvalidPlan::Revision`](crate::InvalidPlan::Revision), as it does
    /// an empty identifier and a snapshot on S3.
    pub revision: Option<Revision<'h>>,
}

impl<'h> PhysicalDelete<'h> {
    /// Creates a plan that removes this object alone, with no condition.
    pub fn new(key: &'h str) -> Self {
        Self {
            key,
            kind: DeleteKind::Object,
            condition: ConditionKind::None,
            condition_value: None,
            revision: None,
        }
    }

    /// Creates a plan from a stored shape and the bytes that it needs.
    ///
    /// The plan removes the object as it is now. Set [`Self::revision`] on
    /// the result to remove a snapshot or a version.
    pub fn from_shape(shape: DeleteShape, key: &'h str, condition_value: Option<&'h [u8]>) -> Self {
        Self {
            key,
            kind: shape.kind,
            condition: shape.condition,
            condition_value,
            revision: None,
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

/// The object that a copy reads, in the same account or service as the
/// client.
///
/// Azure names it by URL in `x-ms-copy-source`, and S3 by bucket and key in
/// `x-amz-copy-source`. Each encodes the key as a request path does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopySource<'a> {
    /// The container or bucket that holds the source, or [`None`] for the
    /// client's own. It may not hold `/`, `?`, `#` or a control character:
    /// a client refuses it with
    /// [`InvalidPlan::CopySource`](crate::InvalidPlan::CopySource).
    pub container: Option<&'a str>,
    /// The key of the source, under the rules of [`PhysicalGet::key`].
    pub key: &'a str,
    /// The snapshot or version to copy, or [`None`] for the object as it is
    /// now. S3 copies a version, and no snapshot.
    pub revision: Option<Revision<'a>>,
    /// The condition on the source. The service copies nothing if it does
    /// not hold.
    pub condition: ConditionKind,
    /// What `condition` compares against: see [`ConditionKind`].
    pub condition_value: Option<&'a [u8]>,
}

impl<'a> CopySource<'a> {
    /// Names the object `key` in the client's own container or bucket, as
    /// it is now, with no condition.
    pub const fn new(key: &'a str) -> Self {
        Self {
            container: None,
            key,
            revision: None,
            condition: ConditionKind::None,
            condition_value: None,
        }
    }
}

/// The part of a copy plan that holds no borrows.
///
/// This is [`Copy`] and has no lifetime, so you can store it. Pass it to
/// the method that reads the response of the copy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyShape {
    /// The condition on the target.
    pub condition: ConditionKind,
    /// The condition on the source.
    pub source_condition: ConditionKind,
}

/// One copy of an object onto a key, which the service carries out without
/// sending the bytes through the client.
///
/// The target takes the source's bytes and content properties. Azure copies
/// the source's metadata, and S3 its metadata and its tags, unless the plan
/// names new ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalCopy<'a> {
    /// The key that the copy writes, under the rules of [`PhysicalGet::key`].
    pub key: &'a str,
    /// The object that the copy reads.
    pub source: CopySource<'a>,
    /// The condition on the target, as a write carries it.
    pub condition: ConditionKind,
    /// What `condition` compares against: see [`ConditionKind`].
    pub condition_value: Option<&'a [u8]>,
    /// The metadata of the target. With no pair, the target has the
    /// source's metadata; with any, it has these pairs alone. The rules of
    /// [`PhysicalPut::metadata`] apply.
    pub metadata: &'a [MetadataPair<'a>],
    /// The tags, the storage class, and on S3 the content properties of the
    /// target. A copy carries no checksum and declares no MD5: a client
    /// refuses either with [`InvalidPlan::Option`](crate::InvalidPlan::Option).
    ///
    /// On S3, new metadata or any content property replaces the source's
    /// metadata and content properties together, with
    /// `x-amz-metadata-directive: REPLACE`: name every one you want to keep.
    /// Tags replace the source's with `x-amz-tagging-directive: REPLACE`.
    pub options: WriteOptions<'a>,
}

impl<'a> PhysicalCopy<'a> {
    /// Creates a plan that copies `source` onto `key` with no condition on
    /// the target, keeping what the source holds.
    pub const fn new(key: &'a str, source: CopySource<'a>) -> Self {
        Self {
            key,
            source,
            condition: ConditionKind::None,
            condition_value: None,
            metadata: &[],
            options: WriteOptions::new(),
        }
    }

    /// Returns the part of this plan that holds no borrows.
    pub fn shape(&self) -> CopyShape {
        CopyShape {
            condition: self.condition,
            source_condition: self.source.condition,
        }
    }
}

/// How soon the service makes an archived object readable again, and at
/// what cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u16)]
pub enum RestorePriority {
    /// The slowest and cheapest: S3's `Bulk`. S3 only.
    Bulk = 1,
    /// `Standard` on both services.
    #[default]
    Standard = 2,
    /// The fastest: S3's `Expedited`, and Azure's `High`.
    High = 3,
}

impl RestorePriority {
    /// Returns the priority with this discriminant.
    ///
    /// Returns [`None`] for a discriminant that this version does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Bulk,
            2 => Self::Standard,
            3 => Self::High,
            _ => return None,
        })
    }
}

/// A request that makes an object in an archive readable again: an S3
/// RestoreObject, or an Azure Set Blob Tier out of the `Archive` tier.
///
/// The two services restore differently. S3 makes a temporary copy readable
/// for [`Self::days`], and the object stays in its archive storage class.
/// Azure rehydrates the object into [`Self::tier`] for good. Both take
/// hours, and answer at once that they started: follow the restore with a
/// HEAD, in [`ObjectMeta::restore_status`](crate::ObjectMeta::restore_status).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysicalRestore<'h> {
    /// The object key, under the rules of [`PhysicalGet::key`].
    pub key: &'h str,
    /// The version to restore, or [`None`] for the object as it is now.
    /// Azure also rehydrates a snapshot, and S3 refuses one with
    /// [`InvalidPlan::Revision`](crate::InvalidPlan::Revision).
    pub revision: Option<Revision<'h>>,
    /// How soon to restore. Azure refuses [`RestorePriority::Bulk`] with
    /// [`InvalidPlan::Option`](crate::InvalidPlan::Option).
    pub priority: RestorePriority,
    /// How many days the restored copy stays readable, at least 1. S3 only.
    ///
    /// S3 requires it for an object in Glacier Flexible Retrieval or Deep
    /// Archive, and refuses it for one in an archive tier of
    /// Intelligent-Tiering, which it restores into a tier that stays. Azure
    /// refuses it with [`InvalidPlan::Option`](crate::InvalidPlan::Option).
    pub days: Option<u32>,
    /// The tier to rehydrate into: `Hot`, `Cool` or `Cold`. Azure requires
    /// it, and S3 refuses it, each with
    /// [`InvalidPlan::Option`](crate::InvalidPlan::Option).
    pub tier: Option<&'h str>,
    /// A checksum of the request body for the encoder to compute and send,
    /// with the provider of that kind that the client registered. S3 only:
    /// an Azure request has no body.
    pub checksum: Option<crate::checksum::ChecksumKind>,
}

impl<'h> PhysicalRestore<'h> {
    /// Creates a plan that restores `key` at the standard priority.
    pub const fn new(key: &'h str) -> Self {
        Self {
            key,
            revision: None,
            priority: RestorePriority::Standard,
            days: None,
            tier: None,
            checksum: None,
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
    /// On S3, this is one of the page's common prefixes.
    ///
    /// The listing reports the shared start of those keys once, and you list
    /// again with it as the prefix to see what is under it.
    Prefix = 2,
    /// A directory that the service keeps as its own entry.
    ///
    /// Only an Azure account with a hierarchical namespace reports these. A
    /// flat account reports a group of keys as [`Self::Prefix`] instead.
    Directory = 3,
    /// A delete marker: the version that a removal wrote in place of the
    /// object, which has no bytes.
    ///
    /// Only an S3 listing of versions reports these. Azure keeps no marker:
    /// a removed object's versions remain, with none of them current.
    DeleteMarker = 4,
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
            4 => Self::DeleteMarker,
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
    /// Whether the listing groups keys at a delimiter after the prefix.
    ///
    /// The shape holds no delimiter text. [`PhysicalList::from_shape`] plans
    /// `/`.
    pub delimited: bool,
    /// The most entries that one page reports.
    pub max_results: Option<u32>,
    /// The extra elements that each page reports beside each object.
    pub include: ListInclude,
}

/// One page of a listing.
///
/// A page is one request. The response names where the next page starts, and
/// you plan that page with the same shape and that marker.
///
/// Because the fields are public and unchecked,
/// [`Blobs::encode_list`](crate::Blobs::encode_list) and
/// [`s3::Objects::encode_list`](crate::s3::Objects::encode_list) validate the
/// plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalList<'h> {
    /// The keys to list under. An empty prefix lists the whole container.
    ///
    /// The prefix is matched byte for byte and is not a path: this crate adds
    /// no `/` to it. To list one directory of a delimited listing, end the
    /// prefix with the delimiter yourself.
    ///
    /// A prefix may be longer than a name.
    pub prefix: &'h str,
    /// Where the previous page ended.
    ///
    /// Pass the [`Listing::next_marker`](crate::Listing::next_marker) that the
    /// previous page reported. The first page carries [`None`]. The text is
    /// the service's, and means nothing to this crate. On S3 it is the
    /// continuation token of a ListObjectsV2, or the key marker of a
    /// ListObjectVersions.
    pub marker: Option<&'h str>,
    /// Where in the versions of the marker's key the previous page ended.
    /// S3 only, in a listing of [`ListInclude::VERSIONS`].
    ///
    /// Pass the [`Listing::next_version_marker`](crate::Listing::next_version_marker)
    /// that the previous page reported, beside its marker. A client refuses
    /// one without a marker, or on any other listing, with
    /// [`InvalidPlan::Marker`](crate::InvalidPlan::Marker).
    pub version_marker: Option<&'h str>,
    /// The text after which the listing starts. S3 only.
    ///
    /// The listing reports only the keys and groups of keys that sort after
    /// this text, which need not be a key. An empty text is the same as
    /// [`None`]. A later page starts at its marker instead.
    pub start_after: Option<&'h str>,
    /// The text at which to group the keys after the prefix, usually `/`, or
    /// [`None`] to report every key.
    ///
    /// A delimited listing reports each group once, as an
    /// [`EntryKind::Prefix`] entry, instead of reporting every key in it. This
    /// is how a listing walks one level of a hierarchy at a time.
    pub delimiter: Option<&'h str>,
    /// The most entries that this page reports.
    ///
    /// [`None`] asks for the service's maximum, which the service also
    /// applies to any larger number: 5,000 on Azure and 1,000 on AWS. The
    /// service may report fewer entries than this and still name a next
    /// page.
    pub max_results: Option<u32>,
    /// The extra elements that each page reports beside each object. See
    /// [`ListInclude`], which S3 takes none of.
    pub include: ListInclude,
}

impl<'h> PhysicalList<'h> {
    /// Creates a plan for the first page of an undelimited listing.
    pub fn new(prefix: &'h str) -> Self {
        Self {
            prefix,
            marker: None,
            version_marker: None,
            start_after: None,
            delimiter: None,
            max_results: None,
            include: ListInclude::default(),
        }
    }

    /// Creates a plan from a stored shape and the text that it needs.
    ///
    /// The plan has no [`Self::start_after`], which a later page does not
    /// need. A delimited shape groups the keys at `/`: set
    /// [`Self::delimiter`] on the plan for another delimiter. A later page
    /// of an S3 listing of versions needs [`Self::version_marker`] as well.
    pub fn from_shape(shape: ListShape, prefix: &'h str, marker: Option<&'h str>) -> Self {
        Self {
            prefix,
            marker,
            version_marker: None,
            start_after: None,
            delimiter: shape.delimited.then_some("/"),
            max_results: shape.max_results,
            include: shape.include,
        }
    }

    /// Returns the part of this plan that holds no borrows.
    pub fn shape(&self) -> ListShape {
        ListShape {
            delimited: self.delimiter.is_some(),
            max_results: self.max_results,
            include: self.include,
        }
    }
}

/// One entry of a listing page.
///
/// Every slice points into the body that
/// [`Blobs::fill_listing`](crate::Blobs::fill_listing) or
/// [`s3::Objects::fill_listing`](crate::s3::Objects::fill_listing) read, and
/// stays valid until you reuse that buffer.
///
/// The fields hold the text that the service wrote, decoded.
///
/// Azure version and snapshot fields are available through
/// `entry.property("VersionId")`, `entry.property("IsCurrentVersion")` and
/// `entry.property("Snapshot")`. To select these while reading the page, use
/// [`Blobs::fill_listing_with`](crate::Blobs::fill_listing_with) and
/// [`BlobProperty`](crate::BlobProperty).
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
    /// [`layered::quoted_etag`](crate::layered::quoted_etag). S3 lists it
    /// with its quotes.
    pub e_tag: Option<&'b str>,
    /// The value that the listing gave for the last modification.
    ///
    /// Azure writes it in the form that the `Last-Modified` header uses:
    /// read it with [`layered::http_date_ms`](crate::layered::http_date_ms).
    /// S3 writes it in ISO 8601: read it with
    /// [`layered::iso8601_ms`](crate::layered::iso8601_ms).
    pub last_modified: Option<&'b str>,
    /// The stored media type, decoded from `Content-Type` when present.
    ///
    /// Only Azure lists it.
    pub content_type: Option<&'b str>,
    /// This entry as the service wrote it, from its opening tag to its closing
    /// one.
    ///
    /// Read a value that the fields above do not carry with [`Self::property`]
    /// or [`Self::properties`], which read these bytes.
    ///
    /// Reading the page decoded the key, entity tag, date and content type in place
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
    /// The key, entity tag, date and content type were decoded when the page was
    /// read, so for those elements this reports the decoded text and
    /// not what the service wrote. Read them from `key`, `e_tag` and
    /// `last_modified` and `content_type` instead.
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

    /// Returns the metadata pairs that this entry carries.
    ///
    /// Returns [`None`] if the entry has no metadata element, which Azure
    /// writes only when the plan asked for [`ListInclude::METADATA`]. This
    /// method reads the entry again. To read the pairs in the same pass as
    /// the rest of the page, select [`BlobProperty::Metadata`].
    pub fn metadata(&self) -> Option<Metadata<'b>> {
        self.property("Metadata").map(Metadata::new)
    }

    /// Returns the version that this entry names, in a listing of
    /// [`ListInclude::VERSIONS`], from its `VersionId` element.
    ///
    /// S3 names an object written before its bucket kept versions `null`.
    /// This method reads the entry again.
    pub fn version(&self) -> Option<&'b [u8]> {
        self.property("VersionId")
    }

    /// Returns whether this entry is the current version of its object, in a
    /// listing of [`ListInclude::VERSIONS`]: Azure's `IsCurrentVersion`,
    /// or S3's `IsLatest`.
    ///
    /// Returns `false` for an entry that says neither, as every entry of a
    /// listing without versions does, and for a delete marker that S3 does
    /// not mark as the latest. This method reads the entry again.
    pub fn is_current_version(&self) -> bool {
        self.properties().any(|(name, value)| {
            matches!(name, b"IsCurrentVersion" | b"IsLatest") && value.trim_ascii() == b"true"
        })
    }
}

/// The metadata pairs of one listed object.
///
/// Each item is a name and a value, as the bytes between the pair's tags.
/// The value is not decoded: pass it to
/// [`layered::decode_into`](crate::layered::decode_into) to resolve `&amp;`
/// and the other references.
#[derive(Debug, Clone, Copy)]
pub struct Metadata<'b> {
    rest: &'b [u8],
}

impl<'b> Metadata<'b> {
    /// Creates an iterator over the pairs in `bytes`, which are the bytes
    /// between the tags of one metadata element, as
    /// [`BlobProperty::Metadata`] reports them.
    pub const fn new(bytes: &'b [u8]) -> Self {
        Self { rest: bytes }
    }

    /// Returns the bytes that this iterator has not read yet.
    pub const fn remaining(self) -> &'b [u8] {
        self.rest
    }
}

impl<'b> Iterator for Metadata<'b> {
    type Item = (&'b [u8], &'b [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        crate::xml::next_pair(&mut self.rest)
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
        for kind in [GetKind::Bytes, GetKind::Head] {
            assert_eq!(GetKind::from_discriminant(kind as u16), Some(kind));
        }
        for condition in [
            ConditionKind::None,
            ConditionKind::IfMatch,
            ConditionKind::IfNoneMatch,
            ConditionKind::IfModifiedSince,
            ConditionKind::IfUnmodifiedSince,
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
/// `xml/azure/mod.rs`. A property added here is added to that match too, and a
/// test there checks that every one of these is matched.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
    /// The metadata pairs of the object, as the bytes between the tags of
    /// the `Metadata` element. Pass the value to [`Metadata::new`] to read
    /// the pairs. Azure writes the element only when the plan asked for
    /// [`ListInclude::METADATA`].
    Metadata,
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
        Self::Metadata,
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
            Self::Metadata => "Metadata",
        }
    }

    // Whether the element holds other elements rather than one text. The
    // page reader reads such an element to its close tag and reports
    // everything between the tags.
    pub(crate) const fn holds_elements(self) -> bool {
        matches!(self, Self::Metadata)
    }
}

// from_bits shifts by COUNT, which must be below the u64 shift width.
const _: () = assert!(BlobProperty::ALL.len() < 64);

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
        // Dense enum discriminants are below COUNT, which is checked above.
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
        // COUNT < 64 is checked above; shifting 1 leaves a nonzero value.
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
        // bit() returns a nonzero power of two, so subtracting one is safe.
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
