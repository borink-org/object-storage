// S3 templates: what an S3 request is built from, and the steps that sign
// a request written from one.
//
// An operation describes its request as a `Describe`: the method, whether
// the path ends in a key, the query, the fields that each request fills,
// each in its slot in the order of their names, and the fields that every
// request sends alike. `layout` writes the request from it, once as the
// template's pass and once as each request's: see `crate::template::Pass`.
// The template holds the request's fixed text twice where SigV4 needs it
// twice: once as the head sends it, and once as the canonical request signs
// it, with the signed fields in the order of their names.

use core::cmp::Ordering;

use super::{
    Bucket, METADATA_PREFIX, Service, encodes, sorted_metadata, write_canonical_value,
    write_metadata_value,
};
use crate::common::write_tags;
use crate::encoding;
use crate::request::{ByteSink, Writer};
use crate::sigv4::{ALGORITHM, Credentials, EMPTY_SHA256, MAX_REGION_LEN, MAX_SECRET_LEN};
use crate::template::{
    self, Body, Builder, Fill, Hole, Param, ParamValue, Pass, Section, Table, Values, WireRequest,
    Written,
};
use crate::{Error, InvalidRequest, MetadataPair, Method, Payload, Result, Span, Tag, Timestamps};

/// The operation that a template writes requests for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Operation {
    /// A GET or a HEAD of an object.
    Get,
    /// A PutObject, which writes a whole object.
    Put,
    /// A DeleteObject, which removes an object or one of its versions.
    Delete,
    /// A ListObjectsV2, or a ListObjectVersions, which reads one page of a
    /// listing.
    List,
    /// A GetObjectTagging, which reads the tags of an object.
    GetTagging,
    /// A PutObjectTagging, which replaces the tags of an object.
    PutTagging,
    /// A RestoreObject, which restores an archived object for some days.
    Restore,
    /// A DeleteObjects, which removes many objects at once.
    DeleteMany,
    /// A CreateSession, which asks a directory bucket for a session.
    CreateSession,
    /// A CreateMultipartUpload, which starts an upload in parts.
    CreateUpload,
    /// An UploadPart, which stages one part of an upload.
    StagePart,
    /// A CompleteMultipartUpload, which commits an object from its parts.
    CommitParts,
    /// An AbortMultipartUpload, which ends an upload without an object.
    AbortUpload,
    /// A ListParts, which reads the parts of an upload.
    ListParts,
    /// A CopyObject, which copies an object.
    Copy,
    /// An UploadPartCopy, which stages a part from the bytes of an object.
    StagePartCopy,
}

/// The part of every S3 request that is the same for each object, written
/// once into a buffer you own.
///
/// A template holds no borrow. Keep it beside the bytes it was written
/// into, and pass both to each encoding function of its operation.
///
/// # Bytes
///
/// Encoding trusts that the bytes are the template's own, and checks
/// nothing of them that would cost each request time. Bytes that are not
/// write a wrong request or are refused, but never make an encoder read
/// outside them or panic. Where bytes could be another template's, such as
/// bytes read back from storage, check them once with [`Template::check`].
#[derive(Debug, Clone, Copy)]
pub struct Template {
    pub(super) table: Table,
    pub(super) operation: Operation,
    pub(super) method: Method,
    pub(super) service: Service,
    // The scope of a signature after its date:
    // `/<region>/<service>/aws4_request`.
    pub(super) scope: Span,
    pub(super) detail: Detail,
    // Where the template holds the key of its object, for a template of
    // one object.
    pub(super) object: Option<Span>,
}

// What an operation keeps in its template to check each request against.
#[derive(Debug, Clone, Copy)]
pub(super) enum Detail {
    Get(super::GetOptions),
    Put {
        condition: crate::ConditionKind,
        checksum: Option<crate::checksum::ChecksumKind>,
        signing: super::PayloadSigning,
    },
    // A request that names one object, or none: the condition it carries,
    // the kind of revision it names, and the checksum of its body.
    Object {
        condition: crate::ConditionKind,
        revision: crate::RevisionKind,
        checksum: Option<crate::checksum::ChecksumKind>,
    },
    // A listing: what it includes, and whether its query names a delimiter
    // and a size of page, which come after a parameter that a page may
    // leave out.
    List {
        include: crate::ListInclude,
        delimiter: bool,
        max_keys: bool,
    },
    Stage {
        checksum: Option<crate::checksum::ChecksumKind>,
        signing: super::PayloadSigning,
    },
    Commit {
        condition: crate::ConditionKind,
        checksum: Option<crate::checksum::ChecksumKind>,
        sized: bool,
    },
    // A request that names one upload of one object.
    Upload,
    Copy {
        condition: crate::ConditionKind,
        source_revision: crate::RevisionKind,
        source_condition: crate::ConditionKind,
        source_ranging: crate::Ranging,
    },
    DeleteMany(Option<crate::checksum::ChecksumKind>),
}

impl Template {
    /// Returns the shape of the read `item` from this template, which
    /// [`s3::accept_get_head`](super::accept_get_head) reads its response against.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] for a template of another
    /// operation.
    pub const fn get_shape(&self, item: &crate::GetItem<'_>) -> Result<crate::GetShape> {
        match self.detail {
            Detail::Get(options) => Ok(item.shape(&options)),
            _ => Err(crate::Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Returns the shape of every whole-object write of this template.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] for a template of another
    /// operation.
    pub const fn put_shape(&self) -> Result<crate::PutShape> {
        match self.detail {
            Detail::Put { condition, .. } => Ok(crate::PutShape { condition }),
            _ => Err(crate::Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Returns the shape of every removal of this template.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] for a template of another
    /// operation.
    pub const fn delete_shape(&self) -> Result<crate::DeleteShape> {
        match (self.operation, self.detail) {
            (Operation::Delete, Detail::Object { condition, .. }) => Ok(crate::DeleteShape {
                kind: crate::DeleteKind::Object,
                condition,
            }),
            _ => Err(crate::Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Returns the shape of every commit of this template.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] for a template of another
    /// operation.
    pub const fn commit_shape(&self) -> Result<crate::CommitShape> {
        match self.detail {
            Detail::Commit { condition, .. } => Ok(crate::CommitShape { condition }),
            _ => Err(crate::Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Returns the shape of every copy of this template.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] for a template of another
    /// operation.
    pub const fn copy_shape(&self) -> Result<crate::CopyShape> {
        match self.detail {
            Detail::Copy {
                condition,
                source_condition,
                ..
            } => Ok(crate::CopyShape {
                condition,
                source_condition,
            }),
            _ => Err(crate::Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Checks that `bytes` are the bytes that this template was written
    /// into, as its `prepare_*` function wrote them. See [Bytes](Self#bytes).
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if they are not.
    pub fn check(&self, bytes: &[u8]) -> Result<()> {
        self.table.check(bytes)
    }

    /// Returns the operation this template writes requests for.
    pub const fn operation(&self) -> Operation {
        self.operation
    }

    /// Returns the service that answers the requests.
    pub const fn service(&self) -> Service {
        self.service
    }

    /// Returns how many bytes at the start of its buffer the template
    /// takes. Those are its bytes, which each request is written from.
    pub const fn bytes_len(&self) -> usize {
        self.table.len()
    }
}

// The value of one field that every request sends alike.
#[derive(Clone, Copy)]
pub(super) enum Value<'p> {
    // Text from the options or the item, which SigV4 signs with its spaces folded.
    Bytes(&'p [u8]),
    // Text that this crate formats, which holds no space to fold.
    Formatted(&'p [u8]),
    // The tags of a write, as `x-amz-tagging` writes them.
    Tags(&'p [Tag<'p>]),
}

// The fields that every request of a template sends alike, gathered on the
// stack.
pub(super) struct FieldList<'p> {
    fields: [Field<'p>; MAX_SIGNED],
    count: usize,
}

impl<'p> FieldList<'p> {
    pub(super) const fn new() -> Self {
        Self {
            fields: [Field::new("", Value::Formatted(b"")); MAX_SIGNED],
            count: 0,
        }
    }

    pub(super) fn push(&mut self, field: Field<'p>) {
        self.fields[self.count] = field;
        self.count += 1;
    }

    pub(super) fn as_slice(&self) -> &[Field<'p>] {
        &self.fields[..self.count]
    }
}

#[derive(Clone, Copy)]
pub(super) struct Field<'p> {
    pub(super) name: &'p str,
    pub(super) value: Value<'p>,
}

impl<'p> Field<'p> {
    pub(super) const fn new(name: &'p str, value: Value<'p>) -> Self {
        Self { name, value }
    }
}

// The source of a copy, `/<bucket>/<key>`, with its version: the bucket,
// which only the template's text holds, and the kind of revision each
// request names.
#[derive(Clone, Copy)]
pub(super) struct CopyFrom<'p> {
    pub(super) bucket: &'p str,
    pub(super) revision: crate::RevisionKind,
}

// The fields that each request fills, each in its slot. The slots of the
// signed fields are in the order of the names that they can hold, so the
// head and the canonical request write them alike: `if-…`, `range`,
// `x-amz-checksum-…`, `x-amz-content-sha256`, `x-amz-copy-source`,
// `x-amz-copy-source-if-…`, `x-amz-copy-source-range`, `x-amz-date`, which
// every request sends, and `x-amz-mp-object-size`. Two fields S3 does not
// sign follow them in the head. Each is there or not by the template's
// options alone.
#[derive(Clone, Copy, Default)]
pub(super) struct Holes<'p> {
    // The field of a condition on the object.
    pub(super) condition: Option<&'static str>,
    pub(super) range: bool,
    // The field of a checksum, and the hole that fills it: of the content,
    // or of a body that this crate writes.
    pub(super) checksum: Option<(&'static str, Hole)>,
    // The SHA-256 of the content or of the body, where each request fills
    // it.
    pub(super) content_sha256: Option<Hole>,
    pub(super) copy_source: Option<CopyFrom<'p>>,
    // The field of a condition on the source.
    pub(super) source_condition: Option<&'static str>,
    pub(super) source_range: bool,
    pub(super) object_size: bool,
    // Not signed: the length, and an MD5, of the content or of the body.
    pub(super) content_length: Option<Hole>,
    pub(super) content_md5: Option<Hole>,
}

impl Holes<'_> {
    // The names of the signed fields that each request fills, in order.
    fn names(&self) -> impl Iterator<Item = &'static str> + use<> {
        [
            self.condition,
            self.range.then_some("range"),
            self.checksum.map(|(name, _)| name),
            self.content_sha256.map(|_| "x-amz-content-sha256"),
            self.copy_source.map(|_| "x-amz-copy-source"),
            self.source_condition,
            self.source_range.then_some("x-amz-copy-source-range"),
            Some("x-amz-date"),
            self.object_size.then_some("x-amz-mp-object-size"),
        ]
        .into_iter()
        .flatten()
    }
}

// One S3 request, described for its template and for each request.
//
// The method, the key, the query, the holes and the kind of body are the
// structure, which the template's options decide, so that a request
// rebuilds the same. The fixed fields, the metadata, the fixed values of
// the query and the text of a fixed body are what only the template's pass
// writes: a request's describes none.
pub(super) struct Describe<'p, const Q: usize> {
    pub(super) operation: Operation,
    pub(super) method: Method,
    // Whether the path ends in an object's key, or names the bucket.
    pub(super) key: bool,
    // The query, in the order of the parameters' names.
    pub(super) query: [Param<'p>; Q],
    pub(super) holes: Holes<'p>,
    // The value of `x-amz-content-sha256` that every request sends alike,
    // where no hole fills it.
    pub(super) content_sha256: &'static [u8],
    // The signed fields that every request sends alike, other than `host`,
    // `x-amz-content-sha256`, the token and the metadata, in any order.
    pub(super) signed: &'p [Field<'p>],
    // The fields that every request sends alike and S3 does not sign.
    pub(super) unsigned: &'p [Field<'p>],
    pub(super) metadata: &'p [MetadataPair<'p>],
    pub(super) body: Body<'p>,
    pub(super) detail: Detail,
}

impl<'p, const Q: usize> Describe<'p, Q> {
    // A request of `operation` that names one object, with no parameter, no
    // hole of its own and no body.
    pub(super) const fn new(operation: Operation, method: Method, detail: Detail) -> Self {
        Self {
            operation,
            method,
            key: true,
            query: [None; Q],
            holes: Holes {
                condition: None,
                range: false,
                checksum: None,
                content_sha256: None,
                copy_source: None,
                source_condition: None,
                source_range: false,
                object_size: false,
                content_length: None,
                content_md5: None,
            },
            content_sha256: EMPTY_SHA256.as_bytes(),
            signed: &[],
            unsigned: &[],
            metadata: &[],
            body: Body::None,
            detail,
        }
    }

    // Adds the condition of the kind `condition` on the object, which each
    // request fills.
    pub(super) fn condition(&mut self, condition: crate::ConditionKind) {
        self.holes.condition = super::condition_header(condition);
    }

    // Adds the field of a checksum of `kind` that `hole` fills. S3 reads an
    // MD5 from `Content-MD5`, which it does not sign, and every other kind
    // from its own `x-amz-checksum-` field, which it signs.
    pub(super) fn checksum(&mut self, kind: crate::checksum::ChecksumKind, hole: Hole) {
        use crate::checksum::ChecksumKind;
        match kind {
            ChecksumKind::Md5 => self.holes.content_md5 = Some(hole),
            ChecksumKind::Crc64 => self.holes.checksum = Some((super::CRC64_HEADER, hole)),
            kind => self.holes.checksum = Some((kind.header(), hole)),
        }
    }

    // Gives the request a body that this crate writes: its length, its
    // SHA-256, which the request signs, and a checksum of `checksum`'s kind.
    pub(super) fn with_body(
        &mut self,
        body: Body<'p>,
        checksum: Option<crate::checksum::ChecksumKind>,
    ) {
        self.body = body;
        self.holes.content_sha256 = Some(Hole::BodyHash);
        self.holes.content_length = Some(Hole::BodyLength);
        if let Some(kind) = checksum {
            self.checksum(kind, Hole::BodyChecksum);
        }
    }
}

// The most signed fields one request has, metadata aside.
const MAX_SIGNED: usize = 24;

// One signed field that every request sends alike, as it is sorted into
// the canonical request.
#[derive(Clone, Copy)]
enum Signed<'p> {
    Host,
    Field(Field<'p>),
    Meta(&'p MetadataPair<'p>),
}

impl Signed<'_> {
    fn write_name(&self, out: &mut dyn ByteSink) {
        match self {
            Self::Host => out.push(b"host"),
            Self::Field(field) => out.push(field.name.as_bytes()),
            Self::Meta(pair) => {
                out.push(METADATA_PREFIX.as_bytes());
                for byte in pair.name.bytes() {
                    out.push(&[byte.to_ascii_lowercase()]);
                }
            }
        }
    }

    // How its name sorts against `name`.
    fn cmp_name(&self, name: &str) -> Ordering {
        match self {
            Self::Host => "host".cmp(name),
            Self::Field(field) => field.name.cmp(name),
            Self::Meta(pair) => METADATA_PREFIX
                .bytes()
                .chain(pair.name.bytes().map(|byte| byte.to_ascii_lowercase()))
                .cmp(name.bytes()),
        }
    }
}

// The signed fields that every request sends alike, in the order of their
// names.
struct Sorted<'p> {
    fields: [Signed<'p>; MAX_SIGNED],
    count: usize,
    metadata: &'p [MetadataPair<'p>],
}

impl<'p> Sorted<'p> {
    fn of(
        signed: &[Field<'p>],
        extra: &[Field<'p>],
        metadata: &'p [MetadataPair<'p>],
    ) -> Result<Self> {
        let mut fields = [Signed::Host; MAX_SIGNED];
        let mut count = 1;
        for field in signed.iter().chain(extra) {
            if count == MAX_SIGNED {
                return Err(InvalidRequest::RequestTooLarge.into());
            }
            let mut at = count;
            while at > 1
                && matches!(fields[at - 1], Signed::Field(previous) if previous.name > field.name)
            {
                fields[at] = fields[at - 1];
                at -= 1;
            }
            fields[at] = Signed::Field(*field);
            count += 1;
        }
        Ok(Self {
            fields,
            count,
            metadata,
        })
    }

    // Each signed field in the order of its name. A metadata name starts
    // `x-amz-meta-`, so the pairs sort among the other fields.
    fn iter(&self) -> impl Iterator<Item = Signed<'p>> + '_ {
        let mut fixed = self.fields[..self.count].iter().copied().peekable();
        let mut metadata = sorted_metadata(self.metadata).peekable();
        core::iter::from_fn(move || match (fixed.peek(), metadata.peek()) {
            (None, None) => None,
            (Some(_), None) => fixed.next(),
            (None, Some(_)) => metadata.next().map(Signed::Meta),
            (Some(field), Some(pair)) => {
                if Signed::Meta(pair).cmp_name(match field {
                    Signed::Host => "host",
                    Signed::Field(field) => field.name,
                    Signed::Meta(_) => unreachable!("the fixed list holds no metadata"),
                }) == Ordering::Greater
                {
                    fixed.next()
                } else {
                    metadata.next().map(Signed::Meta)
                }
            }
        })
    }
}

// What a template's text is written from, which a request's pass does not
// read.
struct Fixed<'p> {
    bucket: &'p Bucket<'p>,
    credentials: &'p Credentials<'p>,
    sorted: Sorted<'p>,
    // The key of the template's object, when it names one.
    object: Option<&'p str>,
}

// Writes a fixed value as the head sends it.
fn write_sent(out: &mut Writer<'_>, value: Value<'_>) {
    match value {
        Value::Bytes(bytes) | Value::Formatted(bytes) => out.push(bytes),
        Value::Tags(tags) => write_tags(out, tags),
    }
}

// Writes a fixed value as SigV4 signs it.
fn write_signed(out: &mut Writer<'_>, value: Value<'_>) {
    match value {
        Value::Bytes(bytes) => write_canonical_value(out, bytes),
        // Encoded tags hold no space.
        Value::Formatted(_) | Value::Tags(_) => write_sent(out, value),
    }
}

// Writes a fixed signed field as the canonical request signs it.
fn write_canonical_field(out: &mut Writer<'_>, signed: Signed<'_>, fixed: &Fixed<'_>) {
    let service = fixed.bucket.service;
    signed.write_name(out);
    out.push(b":");
    match signed {
        Signed::Host => fixed.bucket.write_host(out),
        Signed::Field(field) => write_signed(out, field.value),
        // An encoded word holds no space, and a value sent as it is is
        // signed folded.
        Signed::Meta(pair) if encodes(pair.value, service) => {
            write_metadata_value(out, pair.value, service);
        }
        Signed::Meta(pair) => write_canonical_value(out, pair.value.as_bytes()),
    }
    out.push(b"\n");
}

// Whether every parameter of `query` is the same for each request.
fn fixed_query(query: &[Param<'_>]) -> bool {
    query
        .iter()
        .flatten()
        .all(|(_, value)| matches!(value, ParamValue::Fixed(_)))
}

// Writes the path of the URL, with the template's object when it names one.
fn write_path(out: &mut Writer<'_>, fixed: &Fixed<'_>, key: bool) {
    let bucket = fixed.bucket;
    out.push(b"/");
    if bucket.addressing == super::Addressing::Path {
        out.push(bucket.name.as_bytes());
        if key {
            out.push(b"/");
        }
    }
    if let (true, Some(object)) = (key, fixed.object) {
        for part in crate::url::encode_object_key(object) {
            out.push(part);
        }
    }
}

// Writes a query whose every parameter is fixed, with its `?` or without.
fn write_fixed_query(out: &mut Writer<'_>, query: &[Param<'_>], mark: bool) {
    for (index, (name, value)) in query.iter().flatten().enumerate() {
        if index != 0 {
            out.push(b"&");
        } else if mark {
            out.push(b"?");
        }
        out.push(name.as_bytes());
        out.push(b"=");
        if let ParamValue::Fixed(value) = value {
            crate::url::write_query_value(out, *value);
        }
    }
}

// The source of a copy, which is sent and signed alike.
#[inline(always)]
fn copy_source(p: &mut impl Pass, source: CopyFrom<'_>) {
    p.text(|out| {
        out.push(b"/");
        out.push(source.bucket.as_bytes());
        out.push(b"/");
    });
    p.hole(Hole::SourceKey);
    if source.revision != crate::RevisionKind::Current {
        p.bytes(b"?versionId=");
        p.hole(Hole::SourceRevision);
    }
}

// The signed fields that each request fills: as the head sends them, or,
// for `signed`, as the canonical request signs them. There `before` writes
// the fixed fields whose names sort before each.
#[inline(always)]
fn holes<P: Pass>(
    p: &mut P,
    holes: &Holes<'_>,
    signed: bool,
    mut before: impl FnMut(&mut P, &str),
) {
    let (separator, end): (&'static [u8], &'static [u8]) = match signed {
        true => (b":", b"\n"),
        false => (b": ", b"\r\n"),
    };
    let mut start = |p: &mut P, name: &str| {
        before(p, name);
        p.text(|out| {
            out.push(name.as_bytes());
            out.push(separator);
        });
    };
    if let Some(name) = holes.condition {
        start(p, name);
        p.hole(match signed {
            true => Hole::SignedCondition,
            false => Hole::Condition,
        });
        p.bytes(end);
    }
    if holes.range {
        start(p, "range");
        p.hole(Hole::Range);
        p.bytes(end);
    }
    if let Some((name, hole)) = holes.checksum {
        start(p, name);
        p.hole(hole);
        p.bytes(end);
    }
    if let Some(hole) = holes.content_sha256 {
        start(p, "x-amz-content-sha256");
        p.hole(hole);
        p.bytes(end);
    }
    if let Some(source) = holes.copy_source {
        start(p, "x-amz-copy-source");
        copy_source(p, source);
        p.bytes(end);
    }
    if let Some(name) = holes.source_condition {
        start(p, name);
        p.hole(match signed {
            true => Hole::SignedSourceCondition,
            false => Hole::SourceCondition,
        });
        p.bytes(end);
    }
    if holes.source_range {
        start(p, "x-amz-copy-source-range");
        p.hole(Hole::SourceRange);
        p.bytes(end);
    }
    start(p, "x-amz-date");
    p.hole(Hole::IsoTime);
    p.bytes(end);
    if holes.object_size {
        start(p, "x-amz-mp-object-size");
        p.hole(Hole::ObjectSize);
        p.bytes(end);
    }
}

/// The layout of every S3 request, run as the template's pass with
/// `fixed`, and as a request's without. `object` says whether the template
/// names its object, whose key it then holds.
#[inline(always)]
fn layout<P: Pass, const Q: usize>(
    p: &mut P,
    d: &Describe<'_, Q>,
    fixed: Option<&Fixed<'_>>,
    object: bool,
) {
    p.text(|out| {
        if let Some(fixed) = fixed {
            out.push(fixed.bucket.scheme.as_str().as_bytes());
            out.push(b"://");
            fixed.bucket.write_host(out);
        }
    });
    p.section(Section::Path);
    p.text(|out| {
        if let Some(fixed) = fixed {
            write_path(out, fixed, d.key);
        }
    });
    if d.key && !object {
        p.hole(Hole::Key);
    }

    p.section(Section::Query);
    for (name, value) in d.query.iter().flatten() {
        p.param(name, *value);
    }

    // A URL that no request writes a value into is the template's text, in
    // the request line and in the canonical request.
    let url_fixed = (object || !d.key) && fixed_query(&d.query);
    p.section(Section::Head);
    p.text(|out| {
        out.push(d.method.as_str().as_bytes());
        out.push(b" ");
    });
    if url_fixed {
        p.text(|out| {
            if let Some(fixed) = fixed {
                write_path(out, fixed, d.key);
                write_fixed_query(out, &d.query, true);
            }
        });
    } else {
        p.hole(Hole::Target);
    }
    p.text(|out| {
        if let Some(fixed) = fixed {
            out.push(b" HTTP/1.1\r\nhost: ");
            fixed.bucket.write_host(out);
            out.push(b"\r\n");
        }
    });
    p.fields();
    // The fixed fields first, as one run of text, then `authorization`.
    p.text(|out| {
        let Some(fixed) = fixed else {
            return;
        };
        let service = fixed.bucket.service;
        for signed in fixed.sorted.iter() {
            match signed {
                Signed::Host => {}
                Signed::Field(field) => {
                    out.push(field.name.as_bytes());
                    out.push(b": ");
                    write_sent(out, field.value);
                    out.push(b"\r\n");
                }
                Signed::Meta(pair) => {
                    signed.write_name(out);
                    out.push(b": ");
                    write_metadata_value(out, pair.value, service);
                    out.push(b"\r\n");
                }
            }
        }
        for field in d.unsigned {
            out.push(field.name.as_bytes());
            out.push(b": ");
            write_sent(out, field.value);
            out.push(b"\r\n");
        }
        out.push(b"authorization: ");
        out.push(ALGORITHM.as_bytes());
        out.push(b" Credential=");
        out.push(fixed.credentials.key_id().as_bytes());
        out.push(b"/");
    });
    p.hole(Hole::Day);
    p.text(|out| {
        let Some(fixed) = fixed else {
            return;
        };
        write_scope(out, fixed.bucket);
        out.push(b", SignedHeaders=");
        write_signed_names(out, fixed, &d.holes);
        out.push(b", Signature=");
    });
    p.hole(Hole::Signature);
    p.bytes(b"\r\n");
    holes(p, &d.holes, false, |_, _| {});
    if let Some(hole) = d.holes.content_length {
        p.bytes(b"content-length: ");
        p.hole(hole);
        p.bytes(b"\r\n");
    }
    if let Some(hole) = d.holes.content_md5 {
        p.bytes(b"content-md5: ");
        p.hole(hole);
        p.bytes(b"\r\n");
    }
    p.bytes(b"\r\n");

    p.section(Section::Canonical);
    p.text(|out| {
        out.push(d.method.as_str().as_bytes());
        out.push(b"\n");
    });
    if url_fixed {
        p.text(|out| {
            if let Some(fixed) = fixed {
                write_path(out, fixed, d.key);
                out.push(b"\n");
                write_fixed_query(out, &d.query, false);
            }
        });
    } else {
        p.hole(Hole::Path);
        p.bytes(b"\n");
        p.hole(Hole::Query);
    }
    p.bytes(b"\n");
    // The signed fields in the order of their names: the fixed ones that
    // sort before each that a request fills, then the rest.
    let mut sorted = fixed.map(|fixed| fixed.sorted.iter().peekable());
    let mut write_before = |p: &mut P, name: Option<&str>| {
        p.text(|out| {
            let (Some(fixed), Some(sorted)) = (fixed, sorted.as_mut()) else {
                return;
            };
            while let Some(signed) = sorted
                .next_if(|signed| name.is_none_or(|name| signed.cmp_name(name) == Ordering::Less))
            {
                write_canonical_field(out, signed, fixed);
            }
        });
    };
    holes(p, &d.holes, true, |p, name| write_before(p, Some(name)));
    write_before(p, None);
    p.text(|out| {
        if let Some(fixed) = fixed {
            out.push(b"\n");
            write_signed_names(out, fixed, &d.holes);
            out.push(b"\n");
        }
    });
    match d.holes.content_sha256 {
        Some(hole) => p.hole(hole),
        None => p.text(|out| out.push(d.content_sha256)),
    }
    p.body(d.body);
}

// Writes the scope of a signature after its date:
// `/<region>/<service>/aws4_request`.
fn write_scope(out: &mut dyn ByteSink, bucket: &Bucket<'_>) {
    out.push(b"/");
    out.push(bucket.region.as_bytes());
    out.push(b"/");
    out.push(bucket.signing_service().as_bytes());
    out.push(b"/aws4_request");
}

// Writes the names of every signed field, in order, separated by `;`.
fn write_signed_names(out: &mut Writer<'_>, fixed: &Fixed<'_>, holes: &Holes<'_>) {
    let mut sorted = fixed.sorted.iter().peekable();
    let mut first = true;
    let mut separate = |out: &mut Writer<'_>| {
        if !first {
            out.push(b";");
        }
        first = false;
    };
    for name in holes.names() {
        while let Some(signed) = sorted.next_if(|signed| signed.cmp_name(name) == Ordering::Less) {
            separate(out);
            signed.write_name(out);
        }
        separate(out);
        out.push(name.as_bytes());
    }
    for signed in sorted {
        separate(out);
        signed.write_name(out);
    }
}

/// Writes the template of `describe` into `buf`.
///
/// A bucket that names an object makes a template of that object, whose text
/// holds the key. An operation that names no object refuses such a bucket
/// with [`InvalidRequest::Option`].
pub(super) fn prepare<const Q: usize>(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    describe: &Describe<'_, Q>,
    buf: &mut [u8],
) -> Result<Template> {
    let object = match bucket.object {
        Some(key) if describe.key => {
            super::validate_key(key)?;
            Some(key)
        }
        Some(_) => return Err(InvalidRequest::Option.into()),
        None => None,
    };
    let token = credentials.token().header();
    let mut extra = [Field::new("", Value::Formatted(b"")); 2];
    let mut count = 0;
    if describe.holes.content_sha256.is_none() {
        extra[count] = Field::new(
            "x-amz-content-sha256",
            Value::Formatted(describe.content_sha256),
        );
        count += 1;
    }
    if let Some((name, value)) = token {
        extra[count] = Field::new(name, Value::Bytes(value.as_bytes()));
        count += 1;
    }
    let fixed = Fixed {
        bucket,
        credentials,
        sorted: Sorted::of(describe.signed, &extra[..count], describe.metadata)?,
        object,
    };
    let mut b = Builder::new(buf);
    layout(&mut b, describe, Some(&fixed), object.is_some());
    let scope = b.stash(|out| write_scope(out, bucket));
    let object = object.map(|key| b.stash(|out| out.push(key.as_bytes())));
    let table = b.finish()?;
    Ok(Template {
        table,
        operation: describe.operation,
        method: describe.method,
        service: bucket.service,
        scope,
        detail: describe.detail,
        object,
    })
}

/// An S3 request written but not yet signed: where its parts are in the
/// buffer it was written into.
///
/// Hash [`Self::canonical`] with SHA-256, compute the HMAC-SHA256 of the
/// string to sign under the day's signing key, and pass it to [`set_signature`].
///
/// Pass every function that takes a buffer the one that the request was
/// written into. Another buffer gives a wrong request, but never a panic.
#[derive(Debug, Clone, Copy)]
pub struct Unsigned<'c> {
    method: Method,
    written: Written,
    payload: Payload<'c>,
    // The kind of checksum of a body that this crate wrote, if the request
    // sends one.
    body_checksum: Option<crate::checksum::ChecksumKind>,
}

impl<'c> Unsigned<'c> {
    /// Returns the canonical request, which SigV4 signs, from the buffer
    /// that the request was written into.
    pub fn canonical<'b>(&self, buf: &'b [u8]) -> &'b [u8] {
        let span = self.written.canonical;
        buf.get(span.start..span.start + span.len)
            .unwrap_or_default()
    }

    /// Returns the length of the request in the buffer it was written into:
    /// the head and the canonical request.
    pub fn len(&self) -> usize {
        self.written.end
    }

    /// Returns `true` if the request takes no bytes, which never happens.
    pub fn is_empty(&self) -> bool {
        self.written.end == 0
    }

    /// Returns the body that this crate wrote into the buffer, or [`None`]
    /// for a request whose content you supply.
    pub fn body<'b>(&self, buf: &'b [u8]) -> Option<&'b [u8]> {
        let span = self.written.body?;
        buf.get(span.start..span.start + span.len)
    }

    /// Returns whether the request signs the SHA-256 of a body that this
    /// crate wrote, which [`set_body_hash`] writes in. Call it before you
    /// hash [`Self::canonical`].
    pub fn signs_body(&self) -> bool {
        self.written.body_hash[0].is_some()
    }

    /// Returns the kind of checksum of a body that this crate wrote, if the
    /// request sends one. Compute it over [`Self::body`] and write it in with
    /// [`set_body_checksum`] before you hash [`Self::canonical`].
    pub fn body_checksum(&self) -> Option<crate::checksum::ChecksumKind> {
        self.body_checksum
            .filter(|_| self.written.body_checksum[0].is_some())
    }
}

/// Writes the SHA-256 of the body of `unsigned` into each place of `buf`
/// that holds it: the field `x-amz-content-sha256` and the canonical
/// request. Does nothing for a request that signs no body of its own.
pub fn set_body_hash(unsigned: &Unsigned<'_>, buf: &mut [u8], sha256: &[u8; 32]) {
    let hex = crate::encoding::hex(sha256);
    for span in unsigned.written.body_hash.iter().flatten() {
        put(buf, *span, &hex);
    }
}

// Writes `text` over the placeholder at `span` of `buf`, if `buf` holds it
// whole and it is as long: a buffer that is not the request's is left as it
// is.
fn put(buf: &mut [u8], span: Span, text: &[u8]) {
    if let Some(slot) = buf.get_mut(span.start..span.start.saturating_add(span.len))
        && slot.len() == text.len()
    {
        slot.copy_from_slice(text);
    }
}

/// Writes `digest`, the checksum of the body of `unsigned`, into each place
/// of `buf` that holds it: its field, and the canonical request when S3
/// signs that field. Does nothing for a request that sends no checksum of a
/// body of its own.
///
/// # Errors
///
/// Returns [`InvalidRequest::Checksum`] for a digest of another kind than the
/// template's.
pub fn set_body_checksum(
    unsigned: &Unsigned<'_>,
    buf: &mut [u8],
    digest: &crate::Digest,
) -> Result<()> {
    if unsigned.written.body_checksum[0].is_none() {
        return Ok(());
    }
    if unsigned.body_checksum != Some(digest.kind()) {
        return Err(InvalidRequest::Checksum.into());
    }
    let mut text = [0; crate::checksum::BASE64_LEN];
    // S3 reads a CRC64 in big-endian order.
    let text = digest.base64_for(&mut text, true).as_bytes();
    for span in unsigned.written.body_checksum.iter().flatten() {
        put(buf, *span, text);
    }
    Ok(())
}

/// Writes a request from `template` into `buf`, with the values that `fill`
/// writes. `describe` is the template's, without its text. Inlined into
/// each operation's encoder, which so runs its own layout.
#[inline(always)]
pub(super) fn encode<'c, F: Fill, const Q: usize>(
    template: &Template,
    bytes: &[u8],
    fill: &F,
    payload: Payload<'c>,
    buf: &mut [u8],
    describe: &Describe<'_, Q>,
) -> Result<Unsigned<'c>> {
    let mut out = Writer::new(buf);
    #[cfg(not(feature = "compact"))]
    let written = {
        let mut state = template::State::default();
        let mut pass = template::Encode::new(&template.table, bytes, fill, &mut out, &mut state);
        layout(&mut pass, describe, None, template.object.is_some());
        pass.finish()?
    };
    #[cfg(feature = "compact")]
    let written = {
        let _ = describe;
        template::walk_request(&template.table, bytes, fill, &mut out)?
    };
    let written = template::fitted(&template.table, bytes, fill, &out, written)?;
    let body_checksum = match template.detail {
        Detail::Object { checksum, .. }
        | Detail::Commit { checksum, .. }
        | Detail::DeleteMany(checksum) => checksum,
        _ => None,
    };
    Ok(Unsigned {
        method: template.method,
        written,
        payload,
        body_checksum,
    })
}

impl Template {
    // The key that a request names: `key`, checked, or `None` for a
    // template of one object, which writes its own. A request of such a
    // template passes that key, or an empty one.
    pub(super) fn request_key<'k>(&self, bytes: &[u8], key: &'k str) -> Result<Option<&'k str>> {
        match self.object {
            None => {
                super::validate_key(key)?;
                Ok(Some(key))
            }
            Some(span)
                if key.is_empty()
                    || bytes.get(span.start..span.start + span.len) == Some(key.as_bytes()) =>
            {
                Ok(None)
            }
            Some(_) => Err(InvalidRequest::Template.into()),
        }
    }
}

/// The most bytes [`string_to_sign_prefix`] writes.
pub const STRING_TO_SIGN_PREFIX_LEN: usize = 17 + 16 + 1 + 8 + 1 + MAX_REGION_LEN + 23 + 1;

/// Writes what the string to sign of every request at `now` starts with,
/// and returns it.
///
/// The string to sign is this, followed by the lowercase hexadecimal of the
/// SHA-256 of the canonical request. With a region of four bytes, such as
/// `auto`, it is 64 bytes: one block of SHA-256, which an HMAC can absorb
/// once for every request signed at that instant.
pub fn string_to_sign_prefix<'o>(
    template: &Template,
    bytes: &[u8],
    now: &Timestamps,
    out: &'o mut [u8; STRING_TO_SIGN_PREFIX_LEN],
) -> Result<&'o [u8]> {
    let scope = template.scope;
    let scope = bytes
        .get(scope.start..scope.start + scope.len)
        .ok_or(InvalidRequest::Template)?;
    let mut at = 0;
    for piece in [
        ALGORITHM.as_bytes(),
        b"\n",
        now.iso8601_bytes(),
        b"\n",
        now.date(),
        scope,
        b"\n",
    ] {
        let end = at + piece.len();
        out.get_mut(at..end)
            .ok_or(InvalidRequest::Template)?
            .copy_from_slice(piece);
        at = end;
    }
    Ok(&out[..at])
}

/// Writes the signature into a request and returns the request.
///
/// `mac` is the HMAC-SHA256 of the string to sign: the prefix that
/// [`string_to_sign_prefix`] writes, followed by the lowercase hexadecimal
/// of the SHA-256 of [`Unsigned::canonical`].
pub fn set_signature<'r>(
    unsigned: &Unsigned<'r>,
    buf: &'r mut [u8],
    mac: &[u8; 32],
) -> WireRequest<'r> {
    if let Some(span) = unsigned.written.signature {
        put(buf, span, &encoding::hex(mac));
    }
    let bytes: &'r [u8] = buf;
    WireRequest::new(bytes, unsigned.method, &unsigned.written, unsigned.payload)
}

/// Computes the key that signs every request of the day of `now`.
///
/// `hmac` returns the HMAC-SHA256 of its second argument under the key in
/// its first. The `borink-object-storage-crypto` crate has one.
///
/// # Errors
///
/// Returns [`Error::InvalidCredentials`] if the secret of `credentials` is
/// longer than [`MAX_SECRET_LEN`] bytes, and [`InvalidRequest::Template`] if
/// `bytes` are not the template's.
pub fn signing_key(
    template: &Template,
    bytes: &[u8],
    credentials: &Credentials<'_>,
    now: &Timestamps,
    mut hmac: impl FnMut(&[u8], &[u8]) -> [u8; 32],
) -> Result<[u8; 32]> {
    let scope = template.scope;
    // The scope after its date and the `/` that follows it.
    let scope = bytes
        .get(scope.start + 1..scope.start + scope.len)
        .ok_or(InvalidRequest::Template)?;
    let wipe = credentials.wipe();
    let secret = credentials.secret().as_bytes();
    if secret.len() > MAX_SECRET_LEN {
        return Err(Error::InvalidCredentials);
    }
    let mut seed = [0u8; 4 + MAX_SECRET_LEN];
    let len = 4 + secret.len();
    seed[..4].copy_from_slice(b"AWS4");
    seed[4..len].copy_from_slice(secret);
    let mut key = hmac(&seed[..len], now.date());
    wipe(&mut seed);
    for part in scope.split(|&byte| byte == b'/') {
        let mut next = hmac(&key, part);
        core::mem::swap(&mut key, &mut next);
        wipe(&mut next);
    }
    Ok(key)
}

/// The values of one S3 request, with the canonical form of a condition.
pub(super) struct S3Values<'i>(pub(super) Values<'i>);

impl Fill for S3Values<'_> {
    fn values(&self) -> &Values<'_> {
        &self.0
    }

    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        if hole == Hole::SignedSourceCondition {
            return match self.0.source_condition {
                Some(crate::ConditionValue::ETag(tag)) => {
                    write_canonical_value(out, tag);
                    true
                }
                Some(value) => {
                    use crate::request::HeaderValue;
                    value.write_to(out);
                    true
                }
                None => false,
            };
        }
        match (hole, self.0.condition) {
            (Hole::SignedCondition, Some(crate::ConditionValue::ETag(tag))) => {
                write_canonical_value(out, tag);
                true
            }
            (Hole::SignedCondition, Some(value)) => {
                use crate::request::HeaderValue;
                value.write_to(out);
                true
            }
            _ => false,
        }
    }
}
