// S3 templates: what an S3 request is built from, and the steps that sign
// a request written from one.
//
// An operation describes its request as a `Describe`: the method, whether
// the path ends in a key, the query, and the fields, each of which is fixed
// text or a hole. `prepare` writes the template from it. The template holds
// the request's fixed text twice where SigV4 needs it twice: once as the
// head sends it, and once as the canonical request signs it, with the
// signed fields already in the order of their names.

use core::cmp::Ordering;

use super::{
    Bucket, METADATA_PREFIX, Service, encodes, sorted_metadata, write_canonical_value,
    write_metadata_value,
};
use crate::common::write_tags;
use crate::encoding;
use crate::request::{ByteSink, Writer};
use crate::sigv4::{ALGORITHM, Credentials, MAX_REGION_LEN, MAX_SECRET_LEN};
use crate::template::{
    self, Body, Builder, Fill, Hole, Param, Section, Table, Values, WireRequest, Written,
};
use crate::{Error, InvalidPlan, MetadataPair, Method, Payload, Result, Span, Tag, Timestamps};

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
    List(crate::ListInclude),
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
    /// Returns the operation this template writes requests for.
    pub const fn operation(&self) -> Operation {
        self.operation
    }

    /// Returns the service that answers the requests.
    pub const fn service(&self) -> Service {
        self.service
    }
}

// The value of one field.
#[derive(Clone, Copy)]
pub(super) enum Value<'p> {
    // Text from the plan, which SigV4 signs with its spaces folded.
    Bytes(&'p [u8]),
    // Text that this crate formats, which holds no space to fold.
    Formatted(&'p [u8]),
    // The tags of a write, as `x-amz-tagging` writes them.
    Tags(&'p [Tag<'p>]),
    // A value that each request fills: the hole it is sent as, and the hole
    // it is signed as.
    Hole(Hole, Hole),
    // The source of a copy, `/<bucket>/<key>`, with its version: the bucket
    // fixed, and the key and the version of each request.
    CopySource {
        bucket: &'p str,
        revision: crate::RevisionKind,
    },
}

// Whether each request fills some of the value.
const fn filled(value: Value<'_>) -> bool {
    matches!(value, Value::Hole(..) | Value::CopySource { .. })
}

// Writes the source of a copy, which is sent and signed alike.
fn copy_source(b: &mut Builder<'_>, bucket: &str, revision: crate::RevisionKind) {
    b.with(|out| {
        out.push(b"/");
        out.push(bucket.as_bytes());
        out.push(b"/");
    });
    b.hole(Hole::SourceKey);
    if revision != crate::RevisionKind::Current {
        b.text(b"?versionId=");
        b.hole(Hole::SourceRevision);
    }
}

// The fields of a request, gathered on the stack.
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

// One S3 request, described for its template.
pub(super) struct Describe<'p> {
    pub(super) method: Method,
    // Whether the path ends in the request's key, or names the bucket.
    pub(super) key: bool,
    // The query, in the order of the parameters' names.
    pub(super) query: &'p [Param<'p>],
    // The signed fields other than `host`, `x-amz-content-sha256`,
    // `x-amz-date`, the token and the metadata, in any order.
    pub(super) signed: &'p [Field<'p>],
    pub(super) metadata: &'p [MetadataPair<'p>],
    pub(super) content_sha256: Value<'p>,
    // Fields that are sent and not signed, such as `content-length`.
    pub(super) unsigned: &'p [Field<'p>],
    pub(super) body: Body<'p>,
}

// The most signed fields one request has, metadata aside.
const MAX_SIGNED: usize = 24;

// One signed field, as it is sorted into the canonical request.
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
}

// The signed fields in the order of their names.
struct Sorted<'p> {
    fields: [Signed<'p>; MAX_SIGNED],
    count: usize,
    metadata: &'p [MetadataPair<'p>],
}

impl<'p> Sorted<'p> {
    fn of(describe: &Describe<'p>, extra: &[Field<'p>]) -> Result<Self> {
        let mut fields = [Signed::Host; MAX_SIGNED];
        let mut count = 1;
        for field in describe.signed.iter().chain(extra) {
            if count == MAX_SIGNED {
                return Err(InvalidPlan::RequestTooLarge.into());
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
            metadata: describe.metadata,
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
                let name = match field {
                    Signed::Host => "host",
                    Signed::Field(field) => field.name,
                    Signed::Meta(_) => unreachable!("the fixed list holds no metadata"),
                };
                let pair_name = METADATA_PREFIX
                    .bytes()
                    .chain(pair.name.bytes().map(|byte| byte.to_ascii_lowercase()));
                if name.bytes().cmp(pair_name) == Ordering::Less {
                    fixed.next()
                } else {
                    metadata.next().map(Signed::Meta)
                }
            }
        })
    }
}

// Writes a fixed value as the head sends it.
fn write_sent(out: &mut Writer<'_>, value: Value<'_>) {
    match value {
        Value::Bytes(bytes) | Value::Formatted(bytes) => out.push(bytes),
        Value::Tags(tags) => write_tags(out, tags),
        Value::Hole(..) | Value::CopySource { .. } => {}
    }
}

// Writes a fixed value as SigV4 signs it.
fn write_signed(out: &mut Writer<'_>, value: Value<'_>) {
    match value {
        Value::Bytes(bytes) => write_canonical_value(out, bytes),
        // Encoded tags hold no space.
        Value::Formatted(_) | Value::Tags(_) => write_sent(out, value),
        Value::Hole(..) | Value::CopySource { .. } => {}
    }
}

/// Writes the template of `describe` into `buf`.
pub(super) fn prepare(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    describe: &Describe<'_>,
    operation: Operation,
    detail: Detail,
    buf: &mut [u8],
) -> Result<Template> {
    let token = credentials.token().header();
    let mut fixed = [Field::new("", Value::Formatted(b"")); 3];
    let mut extra = 0;
    fixed[extra] = Field::new("x-amz-content-sha256", describe.content_sha256);
    extra += 1;
    fixed[extra] = Field::new("x-amz-date", Value::Hole(Hole::IsoTime, Hole::IsoTime));
    extra += 1;
    if let Some((name, value)) = token {
        fixed[extra] = Field::new(name, Value::Bytes(value.as_bytes()));
        extra += 1;
    }
    let sorted = Sorted::of(describe, &fixed[..extra])?;
    let service = bucket.service;

    let mut b = Builder::new(buf);
    b.with(|out| {
        out.push(bucket.scheme.as_str().as_bytes());
        out.push(b"://");
        bucket.write_host(out);
    });
    b.section(Section::Path);
    b.with(|out| {
        out.push(b"/");
        if bucket.addressing == super::Addressing::Path {
            out.push(bucket.name.as_bytes());
            if describe.key {
                out.push(b"/");
            }
        }
    });
    if describe.key {
        b.hole(Hole::Key);
    }

    b.query(describe.query);

    b.section(Section::Head);
    b.text(describe.method.as_str().as_bytes());
    b.text(b" ");
    b.hole(Hole::Target);
    b.with(|out| {
        out.push(b" HTTP/1.1\r\nhost: ");
        bucket.write_host(out);
        out.push(b"\r\n");
    });
    b.hole(Hole::Fields);
    // The fixed fields first, as one run of text.
    for signed in sorted.iter() {
        match signed {
            Signed::Host => {}
            Signed::Field(field) if filled(field.value) => {}
            Signed::Field(field) => b.with(|out| {
                out.push(field.name.as_bytes());
                out.push(b": ");
                write_sent(out, field.value);
                out.push(b"\r\n");
            }),
            Signed::Meta(pair) => b.with(|out| {
                signed.write_name(out);
                out.push(b": ");
                write_metadata_value(out, pair.value, service);
                out.push(b"\r\n");
            }),
        }
    }
    for field in describe.unsigned {
        if !filled(field.value) {
            b.with(|out| {
                out.push(field.name.as_bytes());
                out.push(b": ");
                write_sent(out, field.value);
                out.push(b"\r\n");
            });
        }
    }
    // Then the fields that each request fills, `authorization` first.
    b.with(|out| {
        out.push(b"authorization: ");
        out.push(ALGORITHM.as_bytes());
        out.push(b" Credential=");
        out.push(credentials.key_id().as_bytes());
        out.push(b"/");
    });
    b.hole(Hole::Day);
    let mut scope = Span::default();
    b.with(|out| {
        scope = out.part(|out| {
            out.push(b"/");
            out.push(bucket.region.as_bytes());
            out.push(b"/");
            out.push(bucket.signing_service().as_bytes());
            out.push(b"/aws4_request");
        });
        out.push(b", SignedHeaders=");
        for (index, signed) in sorted.iter().enumerate() {
            if index != 0 {
                out.push(b";");
            }
            signed.write_name(out);
        }
        out.push(b", Signature=");
    });
    b.hole(Hole::Signature);
    b.text(b"\r\n");
    for field in sorted
        .iter()
        .filter_map(|signed| match signed {
            Signed::Field(field) => Some(field),
            _ => None,
        })
        .chain(describe.unsigned.iter().copied())
    {
        if filled(field.value) {
            b.with(|out| {
                out.push(field.name.as_bytes());
                out.push(b": ");
            });
            match field.value {
                Value::Hole(sent, _) => b.hole(sent),
                Value::CopySource { bucket, revision } => copy_source(&mut b, bucket, revision),
                _ => {}
            }
            b.text(b"\r\n");
        }
    }
    b.text(b"\r\n");

    b.section(Section::Canonical);
    b.text(describe.method.as_str().as_bytes());
    b.text(b"\n");
    b.hole(Hole::Path);
    b.text(b"\n");
    b.hole(Hole::Query);
    b.text(b"\n");
    for signed in sorted.iter() {
        b.with(|out| {
            signed.write_name(out);
            out.push(b":");
        });
        match signed {
            Signed::Host => b.with(|out| bucket.write_host(out)),
            Signed::Field(Field {
                value: Value::Hole(_, signed),
                ..
            }) => b.hole(signed),
            Signed::Field(Field {
                value: Value::CopySource { bucket, revision },
                ..
            }) => copy_source(&mut b, bucket, revision),
            Signed::Field(field) => b.with(|out| write_signed(out, field.value)),
            // An encoded word holds no space, and a value sent as it is
            // is signed folded.
            Signed::Meta(pair) if encodes(pair.value, service) => {
                b.with(|out| write_metadata_value(out, pair.value, service));
            }
            Signed::Meta(pair) => b.with(|out| write_canonical_value(out, pair.value.as_bytes())),
        }
        b.text(b"\n");
    }
    b.with(|out| {
        out.push(b"\n");
        for (index, signed) in sorted.iter().enumerate() {
            if index != 0 {
                out.push(b";");
            }
            signed.write_name(out);
        }
        out.push(b"\n");
    });
    match describe.content_sha256 {
        Value::Hole(_, signed) => b.hole(signed),
        value => b.with(|out| write_signed(out, value)),
    }
    b.body(describe.body);
    let table = b.finish()?;
    Ok(Template {
        table,
        operation,
        method: describe.method,
        service,
        scope,
        detail,
    })
}

/// An S3 request written but not yet signed: where its parts are in the
/// buffer it was written into.
///
/// Hash [`Self::canonical`] with SHA-256, compute the HMAC-SHA256 of the
/// string to sign under the day's signing key, and pass it to [`finish`].
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
        &buf[span.start..span.start + span.len]
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
///
/// # Panics
///
/// Panics if `buf` is not the buffer that `unsigned` was written into.
pub fn set_body_hash(unsigned: &Unsigned<'_>, buf: &mut [u8], sha256: &[u8; 32]) {
    let hex = crate::encoding::hex(sha256);
    for span in unsigned.written.body_hash.iter().flatten() {
        buf[span.start..span.start + span.len].copy_from_slice(&hex);
    }
}

/// Writes `digest`, the checksum of the body of `unsigned`, into each place
/// of `buf` that holds it: its field, and the canonical request when S3
/// signs that field. Does nothing for a request that sends no checksum of a
/// body of its own.
///
/// # Errors
///
/// Returns [`InvalidPlan::Checksum`] for a digest of another kind than the
/// template's.
///
/// # Panics
///
/// Panics if `buf` is not the buffer that `unsigned` was written into.
pub fn set_body_checksum(
    unsigned: &Unsigned<'_>,
    buf: &mut [u8],
    digest: &crate::Digest,
) -> Result<()> {
    if unsigned.written.body_checksum[0].is_none() {
        return Ok(());
    }
    if unsigned.body_checksum != Some(digest.kind()) {
        return Err(InvalidPlan::Checksum.into());
    }
    let mut text = [0; crate::checksum::BASE64_LEN];
    // S3 reads a CRC64 in big-endian order.
    let text = digest.base64_for(&mut text, true).as_bytes();
    for span in unsigned.written.body_checksum.iter().flatten() {
        buf[span.start..span.start + span.len].copy_from_slice(text);
    }
    Ok(())
}

/// Writes a request from `template` into `buf`, with the values that `fill`
/// writes.
pub(super) fn encode<'c>(
    template: &Template,
    bytes: &[u8],
    fill: &impl Fill,
    payload: Payload<'c>,
    buf: &mut [u8],
) -> Result<Unsigned<'c>> {
    let mut out = Writer::new(buf);
    let written = template::write(&template.table, bytes, fill, &mut out)?;
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
        .ok_or(InvalidPlan::Template)?;
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
            .ok_or(InvalidPlan::Template)?
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
///
/// # Panics
///
/// Panics if `buf` is not the buffer that `unsigned` was written into, or
/// is shorter than it.
pub fn finish<'r>(unsigned: &Unsigned<'r>, buf: &'r mut [u8], mac: &[u8; 32]) -> WireRequest<'r> {
    if let Some(span) = unsigned.written.signature {
        buf[span.start..span.start + span.len].copy_from_slice(&encoding::hex(mac));
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
/// longer than [`MAX_SECRET_LEN`] bytes, and [`InvalidPlan::Template`] if
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
        .ok_or(InvalidPlan::Template)?;
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
            _ => self.0.write(hole, out),
        }
    }

    fn param(&self, hole: Hole) -> Option<Option<&[u8]>> {
        self.0.param(hole)
    }
}
