/// What a read asks the service to return.
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
    /// A caller that carries a value across a language boundary sends each
    /// value as its number, and refuses a number that names nothing here.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Bytes,
            2 => Self::Head,
            _ => return None,
        })
    }
}

/// Which form of byte range a read requests, without its offsets.
///
/// [`RequestedRange`] carries the offsets as well, which a number cannot. This
/// is the part of it that is one value. Pair it with the offsets in
/// [`RequestedRange::from_parts`] to carry a range across a language boundary.
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

/// The byte range that a read requests.
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
    /// [`InvalidRequest::Range`](crate::InvalidRequest::Range), so you need no check
    /// of your own: S3 would answer one with the whole object.
    Bounded {
        /// The first byte that the read requests.
        start: u64,
        /// The byte after the last byte that the read requests.
        end: u64,
    },
    /// Every byte from this offset to the end of the object.
    Offset(u64),
    /// The last `n` bytes, written `Range: bytes=-N`.
    ///
    /// Azure Blob Storage does not accept this form.
    /// [`azure::encode_get`](crate::azure::encode_get) refuses it.
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

/// The precondition that every request of a template carries.
///
/// Each request's item holds what the precondition compares against, in its
/// `condition_value`: a [`ConditionValue::ETag`] for the first two, and a
/// [`ConditionValue::Time`] for the last two.
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

/// What a precondition compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConditionValue<'h> {
    /// An entity tag, or `*` for any, as `If-Match` and `If-None-Match`
    /// carry it. A client refuses an empty one, and one that is not ASCII
    /// or holds a control character, with
    /// [`InvalidRequest::Condition`](crate::InvalidRequest::Condition).
    ETag(&'h [u8]),
    /// An instant, in seconds since the Unix epoch, which the request writes
    /// as an HTTP date. HTTP dates count whole seconds and end at the year
    /// 9999: a client refuses a later instant with
    /// [`InvalidRequest::Condition`](crate::InvalidRequest::Condition).
    ///
    /// To compare against a `Last-Modified` that the service sent, read it
    /// with [`http_date_ms`](crate::http_date_ms) and
    /// divide by 1000.
    Time(u64),
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

/// The part of a read that holds no borrows.
///
/// [`GetItem::shape`] returns this. Keep it while the request is in flight:
/// it is [`Copy`] and has no lifetime. The `accept_get_head` of each service
/// needs only this part to read the response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GetShape {
    /// Whether the read asks for bytes or for metadata.
    pub kind: GetKind,
    /// The byte range that the read requests.
    pub range: RequestedRange,
    /// The precondition that the read carries.
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

/// Whether every request of a template reads a byte range, or none does.
///
/// A template fixes which fields its requests send, so a read with a range
/// and a read without one come from two templates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u16)]
pub enum Ranging {
    /// No request sends a range.
    #[default]
    Whole = 1,
    /// Every request sends a range.
    Ranged = 2,
}

impl Ranging {
    /// Returns the value with this discriminant.
    ///
    /// Returns [`None`](Option::None) for a discriminant that this version
    /// does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Whole,
            2 => Self::Ranged,
            _ => return None,
        })
    }
}

/// Whether every request of a template names an earlier state of its
/// object, and which kind.
///
/// Each request names its own snapshot or version: see
/// [`GetItem::revision`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u16)]
pub enum RevisionKind {
    /// Every request names the object as it is now.
    #[default]
    Current = 1,
    /// Every request names a snapshot, sent as `snapshot`. Azure only.
    Snapshot = 2,
    /// Every request names a version, sent as `versionid` on Azure and
    /// `versionId` on S3.
    Version = 3,
}

impl RevisionKind {
    /// Returns the value with this discriminant.
    ///
    /// Returns [`None`](Option::None) for a discriminant that this version
    /// does not define.
    pub const fn from_discriminant(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::Current,
            2 => Self::Snapshot,
            3 => Self::Version,
            _ => return None,
        })
    }
}

/// What every read of a template does alike.
///
/// Pass it to `prepare_get` of [`azure`](crate::azure) or
/// [`s3`](crate::s3) to write the template, and describe each read with a
/// [`GetItem`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GetOptions {
    /// Whether the reads ask for bytes or for metadata.
    pub kind: GetKind,
    /// Whether every read sends a range.
    pub ranging: Ranging,
    /// The precondition every read carries. Each read names what it
    /// compares against.
    pub condition: ConditionKind,
    /// Whether every read names a snapshot or a version.
    pub revision: RevisionKind,
}

impl GetOptions {
    /// Creates options for reads of every byte of the object as it is now,
    /// with no precondition.
    pub const fn new() -> Self {
        Self {
            kind: GetKind::Bytes,
            ranging: Ranging::Whole,
            condition: ConditionKind::None,
            revision: RevisionKind::Current,
        }
    }

    /// Creates options for reads of the properties and the metadata of an
    /// object, without its bytes.
    pub const fn head() -> Self {
        Self {
            kind: GetKind::Head,
            ..Self::new()
        }
    }

    /// Returns these options for reads that each send a range.
    pub const fn ranged(mut self) -> Self {
        self.ranging = Ranging::Ranged;
        self
    }
}

/// What one read of a template reads.
///
/// Build this for each request and let it go afterwards: it borrows the key
/// and the values it names. To read the response, keep [`Self::shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetItem<'i> {
    /// The object key, before percent-encoding.
    ///
    /// Each service refuses some keys: see
    /// [Object keys](crate#object-keys).
    pub key: &'i str,
    /// The bytes to read: [`RequestedRange::Whole`] exactly when the
    /// template's [`GetOptions::ranging`] is [`Ranging::Whole`]. An empty
    /// range is refused: see [`RequestedRange::Bounded`].
    pub range: RequestedRange,
    /// What the template's precondition compares against: see
    /// [`ConditionKind`]. Present exactly when the template carries one.
    pub condition_value: Option<ConditionValue<'i>>,
    /// The snapshot or version to read, as the service wrote it. Present
    /// exactly when the template's [`GetOptions::revision`] names one, and
    /// never empty.
    pub revision: Option<&'i str>,
}

impl<'i> GetItem<'i> {
    /// Creates a read of every byte of `key`, as it is now.
    pub const fn new(key: &'i str) -> Self {
        Self {
            key,
            range: RequestedRange::Whole,
            condition_value: None,
            revision: None,
        }
    }

    /// Returns this read of the bytes in `range`.
    pub const fn with_range(mut self, range: RequestedRange) -> Self {
        self.range = range;
        self
    }

    /// Returns the part of this read that holds no borrows, for a template
    /// with `options`.
    ///
    /// Keep it while the request is in flight, and pass it to the service's
    /// `accept_get_head` when the response arrives.
    pub const fn shape(&self, options: &GetOptions) -> GetShape {
        GetShape {
            kind: options.kind,
            range: self.range,
            condition: options.condition,
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

/// The properties that describe an object's content, which a write stores
/// and a read returns in the response head.
///
/// Each is one HTTP header value: ASCII, with no control character and no
/// space at either end. A write refuses any other value with
/// [`InvalidRequest::ContentProperty`](crate::InvalidRequest::ContentProperty),
/// except where the service returns UTF-8 as it got it: a `Content-Type` on
/// an Azure account of [`AzureNamespace::Flat`](crate::azure::AzureNamespace::Flat),
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
/// with the same key, with [`InvalidRequest::Tag`](crate::InvalidRequest::Tag). A
/// client of [`s3::Service::Compatible`](crate::s3::Service::Compatible)
/// refuses only an empty key and a repeated one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tag<'h> {
    /// The key.
    pub key: &'h str,
    /// The value.
    pub value: &'h str,
}

/// One object of a removal of several: its key, and the snapshot or version
/// to remove.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteTarget<'h> {
    /// The object key, under the rules of [Object keys](crate#object-keys).
    pub key: &'h str,
    /// The snapshot or version to remove, or [`None`] for the object as it
    /// is now. Azure sends it in the path of the object's Delete Blob, and
    /// S3 as the object's `VersionId`. S3 removes no snapshot: a client
    /// refuses one with [`InvalidRequest::Revision`](crate::InvalidRequest::Revision).
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

/// The options of a write.
///
/// [`PutOptions::options`], [`CopyOptions::options`],
/// [`azure::CommitOptions::options`](crate::azure::CommitOptions::options)
/// and
/// [`s3::CreateUploadOptions::options`](crate::s3::CreateUploadOptions::options)
/// each hold one. Build it with `..Default::default()` or [`Self::new`], so
/// that a field added later does not break your code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOptions<'h> {
    /// The content properties to store with the object.
    ///
    /// A write of a whole object takes them, and so does the write that
    /// names the object of a write in parts: an Azure commit, or an S3
    /// CreateMultipartUpload. Any other write refuses them with
    /// [`InvalidRequest::Option`](crate::InvalidRequest::Option).
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
/// [`ListOptions::include`]. An empty set asks for nothing beyond the
/// object's own properties. Each constant says which service writes it, and
/// a client refuses a listing that asks the other service for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ListInclude(u32);

impl ListInclude {
    /// The metadata pairs of each object, as a `Metadata` element. Azure
    /// only.
    ///
    /// Read them with [`ListEntry::metadata`], or in the same pass as the
    /// rest of the page with [`BlobProperty::Metadata`](crate::azure::BlobProperty::Metadata).
    pub const METADATA: Self = Self(1 << 0);

    /// The owner of each object, as an `Owner` element. S3 only. Read it as
    /// [`s3::ObjectProperty::Owner`](crate::s3::ObjectProperty::Owner).
    pub const OWNER: Self = Self(1 << 1);

    /// An entry for each snapshot of an object, beside the object's own,
    /// each with a `Snapshot` element. Azure only. Read it as
    /// [`BlobProperty::Snapshot`](crate::azure::BlobProperty::Snapshot).
    pub const SNAPSHOTS: Self = Self(1 << 2);

    /// An entry for each version of an object, with its version ID and
    /// whether it is the current one: read both with [`ListEntry::version`]
    /// and [`ListEntry::is_current_version`].
    ///
    /// Azure writes `VersionId` and, on the current version,
    /// `IsCurrentVersion` in a List Blobs. An S3 client sends a
    /// ListObjectVersions instead of a ListObjectsV2, whose entries carry
    /// `VersionId` and `IsLatest`, and which reports each delete marker as
    /// an [`EntryKind::DeleteMarker`]. It pages with a key and a version: see
    /// [`ListMarker::Version`]. A directory bucket keeps no versions, so a
    /// client refuses this flag for one.
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

/// What every write of a whole object from a template does alike.
///
/// Pass it to `prepare_put` of [`azure`](crate::azure) or [`s3`](crate::s3)
/// to write the template, and describe each write with a [`PutItem`].
///
/// # Writing only if the object is absent
///
/// Set [`Self::condition`] to [`ConditionKind::IfNoneMatch`] and give each
/// write the value `*`. The service then refuses a write that would replace
/// an object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PutOptions<'o> {
    /// The precondition every write carries. Each write names what it
    /// compares against.
    pub condition: ConditionKind,
    /// The metadata pairs to store with each object.
    ///
    /// The object then holds these pairs and no others: a write replaces the
    /// whole set.
    pub metadata: &'o [MetadataPair<'o>],
    /// The content properties, the tags and the storage class to store with
    /// each object.
    pub options: WriteOptions<'o>,
    /// The kind of checksum every write sends, or [`None`] for none. Each
    /// write names its value: see [`PutItem::checksum`].
    ///
    /// Azure takes an MD5 and a CRC64. S3 takes every kind.
    pub checksum: Option<crate::checksum::ChecksumKind>,
}

/// The value of one checksum: its base64 as the service takes it, or its
/// digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumValue<'i> {
    /// The base64 text, sent as it is.
    ///
    /// A CRC64 is the base64 of eight bytes in little-endian order for
    /// Azure, and in big-endian order for S3.
    Base64(&'i str),
    /// The digest, which the encoder writes in base64 in the byte order of
    /// the service.
    Digest(crate::checksum::Digest),
}

/// What one write of a whole object from a template writes.
///
/// Build this for each request and let it go afterwards: it borrows the key,
/// the content and the values it names. To read the response, keep
/// [`Self::shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PutItem<'i> {
    /// The object key, before percent-encoding: see
    /// [Object keys](crate#object-keys).
    pub key: &'i str,
    /// The content. The head states its length. A request lends a
    /// [`Payload::Slice`] to your HTTP client, and leaves a
    /// [`Payload::Streamed`] for you to send.
    pub content: Payload<'i>,
    /// What the template's precondition compares against: see
    /// [`ConditionKind`]. Present exactly when the template carries one.
    pub condition_value: Option<ConditionValue<'i>>,
    /// The checksum of the content, of the template's kind. Present exactly
    /// when the template sends one.
    pub checksum: Option<ChecksumValue<'i>>,
}

impl<'i> PutItem<'i> {
    /// Creates a write of `content` to `key`.
    pub const fn new(key: &'i str, content: Payload<'i>) -> Self {
        Self {
            key,
            content,
            condition_value: None,
            checksum: None,
        }
    }

    /// Returns the part of this write that holds no borrows, for a template
    /// with `options`.
    pub const fn shape(&self, options: &PutOptions<'_>) -> PutShape {
        PutShape {
            condition: options.condition,
        }
    }
}

/// The part of a write that holds no borrows.
///
/// [`PutItem::shape`] returns this. It is [`Copy`] and has no lifetime, so
/// you can keep it while the request is in flight, and pass it to the
/// service's `accept_put_head`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PutShape {
    /// The condition that the write carries.
    pub condition: ConditionKind,
}

/// What reading the answer to a commit needs to know of its template.
///
/// [`azure::Template::commit_shape`](crate::azure::Template::commit_shape)
/// and [`s3::Template::commit_shape`](crate::s3::Template::commit_shape)
/// return it. Pass it to the function that reads the response, such as
/// [`azure::accept_commit_blocks_head`](crate::azure::accept_commit_blocks_head).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitShape {
    /// The precondition on the object being committed.
    pub condition: ConditionKind,
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

/// The part of a removal that holds no borrows.
///
/// This is [`Copy`] and has no lifetime, so you can store it. Pass it to
/// the service's `accept_delete_head` to read the response that answers
/// the removal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteShape {
    /// What the removal takes with it.
    pub kind: DeleteKind,
    /// The condition that the removal carries.
    pub condition: ConditionKind,
}

/// What every removal of a template does alike.
///
/// Pass it to `prepare_delete` of [`azure`](crate::azure) or
/// [`s3`](crate::s3) to write the template, and describe each removal with
/// an [`ObjectItem`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteOptions {
    /// What each removal takes with it. Azure refuses to remove an object
    /// with snapshots unless this names them, and S3 keeps none.
    pub kind: DeleteKind,
    /// The precondition every removal carries. Each removal names what it
    /// compares against.
    pub condition: ConditionKind,
    /// Whether every removal names a snapshot or a version, which takes
    /// [`DeleteKind::Object`].
    pub revision: RevisionKind,
}

impl DeleteOptions {
    /// Creates options for removals of the object alone, as it is now, with
    /// no precondition.
    pub const fn new() -> Self {
        Self {
            kind: DeleteKind::Object,
            condition: ConditionKind::None,
            revision: RevisionKind::Current,
        }
    }

    /// Returns the part of every removal of these options that a response
    /// is read against.
    pub const fn shape(&self) -> DeleteShape {
        DeleteShape {
            kind: self.kind,
            condition: self.condition,
        }
    }
}

/// One request that names one object: a removal, or a change to what the
/// service stores about the object.
///
/// Build this for each request and let it go afterwards: it borrows the key
/// and the values it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectItem<'i> {
    /// The object key, before percent-encoding.
    ///
    /// Each service refuses some keys: see
    /// [Object keys](crate#object-keys).
    pub key: &'i str,
    /// What the template's precondition compares against: see
    /// [`ConditionKind`]. Present exactly when the template carries one.
    pub condition_value: Option<ConditionValue<'i>>,
    /// The snapshot or version that the request names, as the service wrote
    /// it. Present exactly when the template names one, and never empty.
    pub revision: Option<&'i str>,
}

impl<'i> ObjectItem<'i> {
    /// Creates a request that names `key`, as it is now.
    pub const fn new(key: &'i str) -> Self {
        Self {
            key,
            condition_value: None,
            revision: None,
        }
    }
}

/// What every staged part of a template does alike: an Azure block, or a
/// part of an S3 upload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageOptions {
    /// The checksum of each part that the service checks, or [`None`].
    /// Azure takes an MD5 or a CRC64, and S3 any kind.
    pub checksum: Option<crate::checksum::ChecksumKind>,
}

/// What every restore of a template does alike: an S3 RestoreObject, or the
/// rehydration of an Azure blob out of the `Archive` tier.
///
/// Describe each restore with an [`ObjectItem`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestoreOptions<'o> {
    /// How soon the service restores each object. Azure takes no
    /// [`RestorePriority::Bulk`].
    pub priority: RestorePriority,
    /// How many days S3 keeps each restored copy, at least one, or
    /// [`None`] for a storage class that S3 restores for good. Azure
    /// rehydrates for good, and takes none.
    pub days: Option<u32>,
    /// The online tier that Azure rehydrates each blob to, such as `Hot`.
    /// S3 restores a copy in the object's own class, and takes none.
    pub tier: Option<&'o str>,
    /// Whether each restore names a snapshot or a version of its object.
    pub revision: RevisionKind,
    /// The checksum of the body of each S3 request. Azure sends no body,
    /// and takes none.
    pub checksum: Option<crate::checksum::ChecksumKind>,
}

/// Where the sources of a template's copies are, and what every copy does
/// alike.
///
/// Pass it to the `prepare_*` function of a copy, and describe each copy
/// with a [`CopyItem`]. A copy that stages a part reads only the source
/// fields, and refuses any other that is not the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyOptions<'o> {
    /// The origin of another Azure account that holds the sources, such as
    /// `https://other.blob.core.windows.net`, or [`None`] for the template's
    /// own. S3 names a bucket of any account by its name alone, and refuses
    /// one with [`InvalidRequest::CopySource`](crate::InvalidRequest::CopySource).
    pub source_endpoint: Option<&'o str>,
    /// The container or bucket that holds the sources, or [`None`] for the
    /// template's own.
    pub source_container: Option<&'o str>,
    /// Whether each copy names a snapshot or a version of its source.
    pub source_revision: RevisionKind,
    /// The condition on each source. Each copy names what it compares
    /// against.
    pub source_condition: ConditionKind,
    /// Whether each copy that stages a part names the bytes of the source it
    /// copies. A whole copy takes [`Ranging::Whole`].
    pub source_ranging: Ranging,
    /// The condition on each target.
    pub condition: ConditionKind,
    /// The metadata pairs of each copy, or [`None`] to keep the source's.
    pub metadata: Option<&'o [MetadataPair<'o>]>,
    /// The tags of each copy, or [`None`] to keep the source's.
    pub tags: Option<&'o [Tag<'o>]>,
    /// What each copy stores beside its metadata and tags. A copy takes no
    /// checksum, and names its tags in [`Self::tags`].
    pub options: WriteOptions<'o>,
}

impl CopyOptions<'_> {
    /// Returns the part of every copy of these options that a response is
    /// read against.
    pub const fn shape(&self) -> CopyShape {
        CopyShape {
            condition: self.condition,
            source_condition: self.source_condition,
        }
    }
}

/// One copy of a template, from the object `source_key` to the object `key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyItem<'i> {
    /// The key of the target, under the rules of
    /// [Object keys](crate#object-keys).
    pub key: &'i str,
    /// The key of the source, under the same rules.
    pub source_key: &'i str,
    /// The snapshot or version of the source. Present exactly when the
    /// template's [`CopyOptions::source_revision`] names one.
    pub source_revision: Option<&'i str>,
    /// What the template's condition on the target compares against.
    pub condition_value: Option<ConditionValue<'i>>,
    /// What the template's condition on the source compares against.
    pub source_condition_value: Option<ConditionValue<'i>>,
    /// The bytes of the source that a staged part copies: a bounded range
    /// exactly when the template's [`CopyOptions::source_ranging`] is
    /// [`Ranging::Ranged`], and [`RequestedRange::Whole`] otherwise.
    pub source_range: RequestedRange,
}

impl<'i> CopyItem<'i> {
    /// Creates a copy of all of `source_key`, as it is now, to `key`.
    pub const fn new(key: &'i str, source_key: &'i str) -> Self {
        Self {
            key,
            source_key,
            source_revision: None,
            condition_value: None,
            source_condition_value: None,
            source_range: RequestedRange::Whole,
        }
    }
}

/// What reading the answer to a copy needs to know of its template.
///
/// [`azure::Template::copy_shape`](crate::azure::Template::copy_shape) and
/// [`s3::Template::copy_shape`](crate::s3::Template::copy_shape) return it.
/// Pass it to the function that reads the response of the copy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyShape {
    /// The condition on the target.
    pub condition: ConditionKind,
    /// The condition on the source.
    pub source_condition: ConditionKind,
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

/// Where a listing continues: what the previous page handed out.
///
/// Take it from [`Listing::next_marker`](crate::Listing::next_marker), copy
/// its text into your own storage before the next page overwrites the body,
/// and pass it back in the item of the next listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ListMarker<'a> {
    /// The text of a listing that continues from one marker: Azure's
    /// `NextMarker`, an S3 continuation token, or the part number marker
    /// of an S3 ListParts.
    Text(&'a str),
    /// Where an S3 listing of versions continues: after the key `key`, or
    /// with `version` after that version of it. S3 hands both out as
    /// `NextKeyMarker` and `NextVersionIdMarker`.
    Version {
        /// The key that the next page starts after, or at.
        key: &'a str,
        /// The version of `key` that the next page starts after.
        version: Option<&'a str>,
    },
}

impl<'a> ListMarker<'a> {
    /// Returns the text of a [`Self::Text`] marker, or [`None`] for a
    /// [`Self::Version`] one.
    pub const fn text(self) -> Option<&'a str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Version { .. } => None,
        }
    }
}

/// What every page of a template's listings reports alike.
///
/// Pass it to `prepare_list` of [`azure`](crate::azure) or
/// [`s3`](crate::s3) to write the template, and describe each page with a
/// [`ListItem`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListOptions<'o> {
    /// The text at which to group the keys after the prefix, usually `/`, or
    /// [`None`] to report every key.
    ///
    /// A delimited listing reports each group once, as an
    /// [`EntryKind::Prefix`] entry, instead of reporting every key in it. This
    /// is how a listing walks one level of a hierarchy at a time.
    pub delimiter: Option<&'o str>,
    /// The most entries that one page reports.
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

/// One page of a listing, which is one request.
///
/// The response names where the next page starts: describe that page with
/// the same prefix and that marker. Build this for each request and let it
/// go afterwards: it borrows the text it names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListItem<'i> {
    /// The keys to list under. An empty prefix lists the whole container.
    ///
    /// The prefix is matched byte for byte and is not a path: this crate adds
    /// no `/` to it. To list one directory of a delimited listing, end the
    /// prefix with the delimiter yourself.
    ///
    /// A prefix may be longer than a name.
    pub prefix: &'i str,
    /// Where the previous page ended.
    ///
    /// Pass the [`Listing::next_marker`](crate::Listing::next_marker) that the
    /// previous page reported. The first page carries [`None`]. The text is
    /// the service's, and means nothing to this crate.
    ///
    /// A listing continues from the kind of marker that its pages hand out:
    /// [`ListMarker::Version`] for an S3 listing of
    /// [`ListInclude::VERSIONS`], and [`ListMarker::Text`] for every other.
    /// The other kind, and an empty text, are refused with
    /// [`InvalidRequest::Marker`](crate::InvalidRequest::Marker).
    pub marker: Option<ListMarker<'i>>,
    /// The text after which the listing starts. S3 only.
    ///
    /// The listing reports only the keys and groups of keys that sort after
    /// this text, which need not be a key. An empty text is the same as
    /// [`None`]. A later page starts at its marker instead.
    pub start_after: Option<&'i str>,
}

impl<'i> ListItem<'i> {
    /// Creates the first page of a listing of the keys under `prefix`.
    pub const fn new(prefix: &'i str) -> Self {
        Self {
            prefix,
            marker: None,
            start_after: None,
        }
    }

    /// Returns this page, continued from `marker`.
    pub const fn after(mut self, marker: ListMarker<'i>) -> Self {
        self.marker = Some(marker);
        self
    }
}

/// One entry of a listing page.
///
/// Every slice points into the body that
/// [`azure::fill_listing`](crate::azure::fill_listing) or
/// [`s3::fill_listing`](crate::s3::fill_listing) read, and
/// stays valid until you reuse that buffer.
///
/// The fields hold the text that the service wrote, decoded.
///
/// Azure version and snapshot fields are available through
/// `entry.property("VersionId")`, `entry.property("IsCurrentVersion")` and
/// `entry.property("Snapshot")`. To select these while reading the page, use
/// [`azure::fill_listing_with`](crate::azure::fill_listing_with) and
/// [`BlobProperty`](crate::azure::BlobProperty).
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
    /// [`quoted_etag`](crate::quoted_etag). S3 lists it
    /// with its quotes.
    pub e_tag: Option<&'b str>,
    /// The value that the listing gave for the last modification.
    ///
    /// Azure writes it in the form that the `Last-Modified` header uses:
    /// read it with [`http_date_ms`](crate::http_date_ms).
    /// S3 writes it in ISO 8601: read it with
    /// [`iso8601_ms`](crate::iso8601_ms).
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
    /// [`decode_property`](crate::decode_property).
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
    /// writes only when the listing asked for [`ListInclude::METADATA`]. This
    /// method reads the entry again. To read the pairs in the same pass as
    /// the rest of the page, select
    /// [`BlobProperty::Metadata`](crate::azure::BlobProperty::Metadata).
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
/// [`decode_property`](crate::decode_property) to resolve `&amp;`
/// and the other references.
#[derive(Debug, Clone, Copy)]
pub struct Metadata<'b> {
    rest: &'b [u8],
}

impl<'b> Metadata<'b> {
    /// Creates an iterator over the pairs in `bytes`, which are the bytes
    /// between the tags of one metadata element, as
    /// [`BlobProperty::Metadata`](crate::azure::BlobProperty::Metadata) reports them.
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

        // 0 is the twins' "absent", so no value may claim it.
        assert_eq!(GetKind::from_discriminant(0), None);
        assert_eq!(ConditionKind::from_discriminant(0), None);
        assert_eq!(DeleteKind::from_discriminant(0), None);
        assert_eq!(RangeForm::from_discriminant(0), None);
        assert_eq!(EntryKind::from_discriminant(0), None);
    }
}
