// Azure's templates. `crate::template` says what a template is and how a
// request is written from one. This module adds what is Azure's own: the
// `Template`, the request that each operation writes, and how a template is
// stored as bytes. Azure signs nothing under a bearer token, so a request is
// ready to send as soon as it is written.

use crate::azure::{
    AzureNamespace, Bearer, Container, MAX_URL_LEN, METADATA_PREFIX, VERSION, validate_key,
};
use crate::checksum::ChecksumKind;
use crate::request::{HeaderValue, Writer, write_tags_as_query};
use crate::template::{
    self, Builder, Fill, HEADER_LEN, Hole, META_LEN, Param, ParamValue, Pass, Section,
    StoredWriter, Table, TemplateBody, split_header,
};
use crate::url::{encode_object_key, write_query_value};
use crate::validate::is_header_value;
use crate::{
    CommitShape, ConditionKind, CopyShape, DeleteKind, DeleteShape, Error, GetItem, GetKind,
    GetOptions, GetShape, InvalidRequest, MetadataPair, Method, PutShape, Ranging, Result,
    RevisionKind, Span, Tag, WireRequest,
};

/// The operation that a template writes requests for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Operation {
    /// A GET or a HEAD of a blob.
    Get,
    /// A Put Blob, which writes a whole blob.
    Put,
    /// A Delete Blob, which removes a blob, its snapshots, or both.
    Delete,
    /// A Set Blob Tier, which moves a blob to another access tier.
    SetTier,
    /// A Set Blob Metadata, which replaces a blob's metadata.
    SetMetadata,
    /// A Set Blob Properties, which replaces a blob's content properties.
    SetProperties,
    /// A Set Blob Tags, which replaces a blob's tags.
    SetTags,
    /// A Get Blob Tags, which reads a blob's tags.
    GetTags,
    /// A Snapshot Blob, which takes a read-only snapshot of a blob.
    Snapshot,
    /// A Put Block, which stages one block of a blob.
    StageBlock,
    /// A Put Block List, which commits a blob from its blocks.
    CommitBlocks,
    /// A Get Block List, which reads the blocks of a blob.
    ListBlocks,
    /// A Copy Blob, which copies a blob and may finish later.
    Copy,
    /// A Copy Blob From URL, which copies a blob and answers once it has.
    CopyFromUrl,
    /// A Put Blob From URL, which writes a blob from the bytes at a URL.
    PutFromUrl,
    /// A Put Block From URL, which stages a block from the bytes at a URL.
    StageBlockFromUrl,
    /// An Abort Copy Blob, which stops a copy that is pending.
    AbortCopy,
    /// A Set Blob Tier that rehydrates a blob out of the `Archive` tier.
    Restore,
    /// A Blob Batch of Delete Blobs, which removes many blobs at once.
    DeleteMany,
    /// A List Blobs, which reads one page of a listing.
    List,
}

// The decoded header of an Azure template buffer.
#[derive(Debug, Clone, Copy)]
pub(super) struct Header {
    pub(super) table: Table,
    pub(super) operation: Operation,
    pub(super) method: Method,
    pub(super) namespace: AzureNamespace,
    pub(super) detail: Detail,
    // Where the template holds the key of its object, for a template of
    // one object.
    pub(super) object: Option<Span>,
}

// What an operation keeps in its template to check each request against.
#[derive(Debug, Clone, Copy)]
pub(super) enum Detail {
    Get(GetOptions),
    Put {
        condition: ConditionKind,
        checksum: Option<ChecksumKind>,
    },
    // A request that names one object: the condition it carries, and the
    // kind of revision it names.
    Object {
        condition: ConditionKind,
        revision: RevisionKind,
        // What a removal takes with it, and `Object` for every other
        // operation.
        delete: DeleteKind,
    },
    List,
    Stage(Option<ChecksumKind>),
    Commit {
        condition: ConditionKind,
        content_md5: bool,
    },
    // A batch: where the template holds the start of each part's path, and
    // the fields that end each part.
    Batch {
        path: Span,
        tail: Span,
    },
    Copy {
        condition: ConditionKind,
        source_revision: RevisionKind,
        source_condition: ConditionKind,
        source_ranging: Ranging,
    },
}

const MAGIC: [u8; 3] = *b"BTA";

/// A prepared Azure request template.
///
/// Wraps the bytes that an `azure::prepare_*` function wrote as an
/// unsized slice view (like [`core::ffi::CStr`] or `std::path::Path`), with
/// no extra fields and no borrow of its own. Call [`Self::from_bytes`] to
/// view a prepared byte slice as `&Template`, and [`Self::check`] once if
/// those bytes came from storage or another untrusted source.
#[repr(transparent)]
#[derive(Debug, PartialEq, Eq)]
pub struct Template([u8]);

impl Template {
    /// Views `bytes` written by an `azure::prepare_*` function as a
    /// template.
    ///
    /// Checks nothing of `bytes`; call [`Self::check`] once if they came from
    /// storage or another untrusted source.
    #[inline]
    #[allow(unsafe_code)]
    pub const fn from_bytes(bytes: &[u8]) -> &Self {
        // SAFETY: `Self` is `#[repr(transparent)]` over `[u8]`, so a shared
        // reference to `[u8]` has the exact layout and validity of `&Self`.
        unsafe { &*(bytes as *const [u8] as *const Self) }
    }

    /// Returns the template's bytes.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Checks that this template is an intact Azure template from this
    /// version of the crate.
    ///
    /// Encoding reads the template header without hashing the whole buffer.
    /// Call this once when the bytes came from storage or another untrusted
    /// source.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if the bytes are truncated,
    /// changed, or not a valid Azure template.
    pub fn check(&self) -> Result<()> {
        let bytes = self.as_bytes();
        Header::load(bytes)?.table.check(bytes)
    }

    /// Returns the operation that this template writes requests for.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if this is not a valid
    /// Azure template.
    pub fn operation(&self) -> Result<Operation> {
        Ok(Header::load(self.as_bytes())?.operation)
    }

    /// Returns the account namespace that this template was prepared for.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if this is not a valid Azure
    /// template.
    pub fn namespace(&self) -> Result<AzureNamespace> {
        Ok(Header::load(self.as_bytes())?.namespace)
    }

    /// Returns the shape of the read `item` from this template, which
    /// [`azure::accept_get_head`](crate::azure::accept_get_head)
    /// reads its response against.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if this is not a read template.
    pub fn get_shape(&self, item: &GetItem<'_>) -> Result<GetShape> {
        match Header::load(self.as_bytes())?.detail {
            Detail::Get(options) => Ok(item.shape(&options)),
            _ => Err(Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Returns the shape of every whole-object write from this template.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if this is not a write template.
    pub fn put_shape(&self) -> Result<PutShape> {
        match Header::load(self.as_bytes())?.detail {
            Detail::Put { condition, .. } => Ok(PutShape { condition }),
            _ => Err(Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Returns the shape of every removal from this template.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if this is not a delete template.
    pub fn delete_shape(&self) -> Result<DeleteShape> {
        let header = Header::load(self.as_bytes())?;
        match (header.operation, header.detail) {
            (
                Operation::Delete,
                Detail::Object {
                    condition, delete, ..
                },
            ) => Ok(DeleteShape {
                kind: delete,
                condition,
            }),
            _ => Err(Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Returns the shape of every commit from this template.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if this is not a commit template.
    pub fn commit_shape(&self) -> Result<CommitShape> {
        match Header::load(self.as_bytes())?.detail {
            Detail::Commit { condition, .. } => Ok(CommitShape { condition }),
            _ => Err(Error::InvalidRequest(InvalidRequest::Template)),
        }
    }

    /// Returns the shape of every copy from this template.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRequest::Template`] if this is not a copy template.
    pub fn copy_shape(&self) -> Result<CopyShape> {
        match Header::load(self.as_bytes())?.detail {
            Detail::Copy {
                condition,
                source_condition,
                ..
            } => Ok(CopyShape {
                condition,
                source_condition,
            }),
            _ => Err(Error::InvalidRequest(InvalidRequest::Template)),
        }
    }
}

impl AsRef<[u8]> for Template {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<Template> for Template {
    #[inline]
    fn as_ref(&self) -> &Self {
        self
    }
}

impl Header {
    #[inline(always)]
    pub(super) fn load(bytes: &[u8]) -> Result<Self> {
        let (meta, table_bytes) = split_header(bytes).ok_or(InvalidRequest::Template)?;
        let table = Table::load(table_bytes, bytes.len()).ok_or(InvalidRequest::Template)?;
        load_meta(meta, table).ok_or_else(|| InvalidRequest::Template.into())
    }
}

fn write_meta(
    into: &mut StoredWriter<'_>,
    operation: Operation,
    method: Method,
    namespace: AzureNamespace,
    detail: Detail,
    object: Option<Span>,
) {
    into.put(&[MAGIC[0], MAGIC[1], MAGIC[2], operation as u8]);
    into.put(&[method as u8, namespace as u8, u8::from(object.is_some())]);
    let object = object.unwrap_or_default();
    into.u32(object.start as u32);
    into.u32(object.len as u32);
    let detail_bytes = match detail {
        Detail::Get(options) => [
            0,
            options.kind as u8,
            options.ranging as u8,
            options.condition as u8,
            options.revision as u8,
        ],
        Detail::Put {
            condition,
            checksum,
        } => [1, condition as u8, checksum_number(checksum), 0, 0],
        Detail::Object {
            condition,
            revision,
            delete,
        } => [2, condition as u8, revision as u8, delete as u8, 0],
        Detail::List => [3, 0, 0, 0, 0],
        Detail::Stage(checksum) => [4, checksum_number(checksum), 0, 0, 0],
        Detail::Commit {
            condition,
            content_md5,
        } => [5, condition as u8, u8::from(content_md5), 0, 0],
        Detail::Batch { .. } => [6, 0, 0, 0, 0],
        Detail::Copy {
            condition,
            source_revision,
            source_condition,
            source_ranging,
        } => [
            7,
            condition as u8,
            source_revision as u8,
            source_condition as u8,
            source_ranging as u8,
        ],
    };
    into.put(&detail_bytes);
    let (path, tail) = match detail {
        Detail::Batch { path, tail } => (path, tail),
        _ => (Span::default(), Span::default()),
    };
    for span in [path, tail] {
        into.u32(span.start as u32);
        into.u32(span.len as u32);
    }
}

// The operations, at the number that `as u8` gives each.
const OPERATIONS: [Operation; 20] = [
    Operation::Get,
    Operation::Put,
    Operation::Delete,
    Operation::SetTier,
    Operation::SetMetadata,
    Operation::SetProperties,
    Operation::SetTags,
    Operation::GetTags,
    Operation::Snapshot,
    Operation::StageBlock,
    Operation::CommitBlocks,
    Operation::ListBlocks,
    Operation::Copy,
    Operation::CopyFromUrl,
    Operation::PutFromUrl,
    Operation::StageBlockFromUrl,
    Operation::AbortCopy,
    Operation::Restore,
    Operation::DeleteMany,
    Operation::List,
];

const _: () = {
    let mut at = 0;
    while at < OPERATIONS.len() {
        assert!(OPERATIONS[at] as usize == at);
        at += 1;
    }
};

// What `write_meta` stores fits inside `META_LEN`.
const _: () = assert!(4 + 3 + 8 + 5 + 16 <= META_LEN);

// A checksum kind as one byte, and none as 0.
fn checksum_number(kind: Option<ChecksumKind>) -> u8 {
    kind.map_or(0, |kind| kind as u8)
}

fn checksum_kind(number: u8) -> Option<Option<ChecksumKind>> {
    match number {
        0 => Some(None),
        number => ChecksumKind::from_discriminant(u16::from(number)).map(Some),
    }
}

// Reads what `write_meta` wrote, or `None` for a value that no template
// holds. A span must lie within the template's text region.
#[inline(always)]
fn load_meta(stored: &[u8; META_LEN], table: Table) -> Option<Header> {
    let u32_at =
        |i: usize| u32::from_le_bytes([stored[i], stored[i + 1], stored[i + 2], stored[i + 3]]);
    if [stored[0], stored[1], stored[2]] != MAGIC {
        return None;
    }
    let operation = *OPERATIONS.get(usize::from(stored[3]))?;
    let method = match stored[4] {
        1 => Method::Get,
        2 => Method::Head,
        3 => Method::Put,
        4 => Method::Delete,
        5 => Method::Post,
        _ => return None,
    };
    let namespace = match stored[5] {
        0 => AzureNamespace::Unknown,
        1 => AzureNamespace::Flat,
        2 => AzureNamespace::Hierarchical,
        _ => return None,
    };
    let within = |start: u32, len: u32| {
        let end = start.checked_add(len)?;
        ((start as usize) >= HEADER_LEN && (end as usize) <= table.text_end()).then_some(Span {
            start: start as usize,
            len: len as usize,
        })
    };
    let object = match stored[6] {
        0 => None,
        1 => Some(within(u32_at(7), u32_at(11))?),
        _ => return None,
    };
    let [tag, a, b, c, d] = [stored[15], stored[16], stored[17], stored[18], stored[19]];
    let condition = |number: u8| ConditionKind::from_discriminant(u16::from(number));
    let revision = |number: u8| RevisionKind::from_discriminant(u16::from(number));
    let ranging = |number: u8| Ranging::from_discriminant(u16::from(number));
    let detail = match tag {
        0 => Detail::Get(GetOptions {
            kind: GetKind::from_discriminant(u16::from(a))?,
            ranging: ranging(b)?,
            condition: condition(c)?,
            revision: revision(d)?,
        }),
        1 => Detail::Put {
            condition: condition(a)?,
            checksum: checksum_kind(b)?,
        },
        2 => Detail::Object {
            condition: condition(a)?,
            revision: revision(b)?,
            delete: DeleteKind::from_discriminant(u16::from(c))?,
        },
        3 => Detail::List,
        4 => Detail::Stage(checksum_kind(a)?),
        5 => Detail::Commit {
            condition: condition(a)?,
            content_md5: match b {
                0 => false,
                1 => true,
                _ => return None,
            },
        },
        6 => Detail::Batch {
            path: within(u32_at(20), u32_at(24))?,
            tail: within(u32_at(28), u32_at(32))?,
        },
        7 => Detail::Copy {
            condition: condition(a)?,
            source_revision: revision(b)?,
            source_condition: condition(c)?,
            source_ranging: ranging(d)?,
        },
        _ => return None,
    };
    Some(Header {
        table,
        operation,
        method,
        namespace,
        detail,
        object,
    })
}

// The value of one field that every request sends alike.
#[derive(Clone, Copy)]
pub(super) enum FieldValue<'p> {
    Bytes(&'p [u8]),
    Tags(&'p [Tag<'p>]),
    // `Bearer` and a token, as the field of a copy that reads its source
    // as a client would sends it.
    Bearer(&'p str),
}

#[derive(Clone, Copy)]
pub(super) struct Field<'p> {
    pub(super) name: &'p str,
    pub(super) value: FieldValue<'p>,
}

impl<'p> Field<'p> {
    pub(super) const fn new(name: &'p str, value: FieldValue<'p>) -> Self {
        Self { name, value }
    }
}

// The fields that every request of a template sends alike, gathered on the
// stack. An operation has at most a dozen; metadata goes apart.
pub(super) struct FieldList<'p> {
    fields: [Field<'p>; 16],
    count: usize,
}

impl<'p> FieldList<'p> {
    pub(super) const fn new() -> Self {
        Self {
            fields: [Field::new("", FieldValue::Bytes(b"")); 16],
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

// The URL of a copy's source: the origin and the container, which only the
// template's text holds, then the key and the revision of each request.
#[derive(Clone, Copy)]
pub(super) struct CopyFrom<'p> {
    pub(super) origin: &'p str,
    pub(super) container: &'p str,
    pub(super) revision: RevisionKind,
}

// The fields that each request fills, in the order that the head writes
// them. Each is there or not by the template's options alone.
#[derive(Clone, Copy, Default)]
pub(super) struct Holes<'p> {
    pub(super) copy_source: Option<CopyFrom<'p>>,
    pub(super) source_range: bool,
    // The field of a condition on the source.
    pub(super) source_condition: Option<&'static str>,
    pub(super) content_length: Option<Hole>,
    // The field of a checksum of the content.
    pub(super) checksum: Option<&'static str>,
    pub(super) range: bool,
    // The field of a condition on the object.
    pub(super) condition: Option<&'static str>,
}

// One Azure request, described for its template and for each request.
//
// The method, the key, the query and the holes are the structure, which the
// template's options decide, so that a request rebuilds the same. The fixed
// fields, the metadata and the text of a fixed body are what only the
// template's pass writes: a request's describes none.
pub(super) struct Describe<'p, const Q: usize> {
    pub(super) method: Method,
    // Whether the path ends in an object's key, or names the container.
    pub(super) key: bool,
    pub(super) query: [Param<'p>; Q],
    pub(super) holes: Holes<'p>,
    pub(super) fields: &'p [Field<'p>],
    pub(super) metadata: &'p [MetadataPair<'p>],
    pub(super) body: TemplateBody<'p>,
}

// What a template's text is written from, which a request's pass does not
// read.
#[derive(Clone, Copy)]
pub(super) struct Fixed<'p> {
    container: &'p Container<'p>,
    token: &'p str,
    scheme: &'p str,
    host: &'p str,
}

fn write_value(out: &mut Writer<'_>, value: FieldValue<'_>) {
    match value {
        FieldValue::Bytes(bytes) => out.push(bytes),
        FieldValue::Tags(tags) => write_tags_as_query(out, tags),
        FieldValue::Bearer(token) => Bearer(token).write_to(out),
    }
}

// Writes a query whose every parameter is fixed, as the template's pass
// writes it.
fn write_fixed_query(out: &mut Writer<'_>, query: &[Param<'_>]) {
    for (index, (name, value)) in query.iter().flatten().enumerate() {
        out.push(if index == 0 { b"?" } else { b"&" });
        out.push(name.as_bytes());
        out.push(b"=");
        if let ParamValue::Fixed(value) = value {
            write_query_value(out, *value);
        }
    }
}

// Writes the path of the URL: the container, and the key of the template's
// object when it names one.
fn write_path(out: &mut Writer<'_>, fixed: Fixed<'_>, key: bool) {
    out.push(b"/");
    out.push(fixed.container.name.as_bytes());
    if key {
        out.push(b"/");
        for part in encode_object_key(fixed.container.object.unwrap_or_default()) {
            out.push(part);
        }
    }
}

// A field that each request fills with `hole`.
#[inline(always)]
fn filled_field(p: &mut impl Pass, name: &str, hole: Hole) {
    p.text(|out| {
        out.push(name.as_bytes());
        out.push(b": ");
    });
    p.hole(hole);
    p.bytes(b"\r\n");
}

/// The layout of every Azure request, run as the template's pass with
/// `fixed`, and as a request's without. `object` says whether the template
/// names its object, whose key it then holds.
#[inline(always)]
pub(super) fn layout<P: Pass, const Q: usize>(
    p: &mut P,
    d: &Describe<'_, Q>,
    fixed: Option<Fixed<'_>>,
    object: bool,
) {
    p.text(|out| {
        if let Some(fixed) = fixed {
            out.push(fixed.scheme.as_bytes());
            out.push(b"://");
            out.push(fixed.host.as_bytes());
        }
    });
    p.section(Section::Path);
    p.text(|out| {
        if let Some(fixed) = fixed {
            out.push(b"/");
            out.push(fixed.container.name.as_bytes());
            if d.key {
                out.push(b"/");
            }
        }
    });
    if d.key {
        if object {
            p.text(|out| {
                if let Some(fixed) = fixed {
                    for part in encode_object_key(fixed.container.object.unwrap_or_default()) {
                        out.push(part);
                    }
                }
            });
        } else {
            p.hole(Hole::Key);
        }
    }

    p.section(Section::Query);
    for (name, value) in d.query.iter().flatten() {
        p.param(name, *value);
    }

    p.section(Section::Head);
    p.text(|out| {
        out.push(d.method.as_str().as_bytes());
        out.push(b" ");
    });
    // A URL that no request writes a value into is the template's text.
    if (object || !d.key) && template::fixed_query(&d.query) {
        p.text(|out| {
            if let Some(fixed) = fixed {
                write_path(out, fixed, d.key);
                write_fixed_query(out, &d.query);
            }
        });
    } else {
        p.hole(Hole::Target);
    }
    p.text(|out| {
        if let Some(fixed) = fixed {
            out.push(b" HTTP/1.1\r\nhost: ");
            out.push(fixed.host.as_bytes());
            out.push(b"\r\n");
        }
    });
    p.fields();
    p.text(|out| {
        let Some(fixed) = fixed else {
            return;
        };
        out.push(b"authorization: ");
        Bearer(fixed.token).write_to(out);
        out.push(b"\r\nx-ms-version: ");
        out.push(VERSION.as_bytes());
        out.push(b"\r\n");
        for field in d.fields {
            out.push(field.name.as_bytes());
            out.push(b": ");
            write_value(out, field.value);
            out.push(b"\r\n");
        }
        for pair in d.metadata {
            out.push(METADATA_PREFIX.as_bytes());
            out.push(pair.name.as_bytes());
            out.push(b": ");
            out.push(pair.value.as_bytes());
            out.push(b"\r\n");
        }
    });
    p.bytes(b"x-ms-date: ");
    p.hole(Hole::HttpTime);
    p.bytes(b"\r\n");

    let holes = d.holes;
    if let Some(source) = holes.copy_source {
        p.text(|out| {
            out.push(b"x-ms-copy-source: ");
            out.push(source.origin.as_bytes());
            out.push(b"/");
            out.push(source.container.as_bytes());
            out.push(b"/");
        });
        p.hole(Hole::SourceKey);
        match source.revision {
            RevisionKind::Current => {}
            RevisionKind::Snapshot => {
                p.bytes(b"?snapshot=");
                p.hole(Hole::SourceRevision);
            }
            RevisionKind::Version => {
                p.bytes(b"?versionid=");
                p.hole(Hole::SourceRevision);
            }
        }
        p.bytes(b"\r\n");
    }
    if holes.source_range {
        filled_field(p, "x-ms-source-range", Hole::SourceRange);
    }
    if let Some(name) = holes.source_condition {
        filled_field(p, name, Hole::SourceCondition);
    }
    if let Some(hole) = holes.content_length {
        filled_field(p, "content-length", hole);
    }
    if let Some(name) = holes.checksum {
        filled_field(p, name, Hole::Checksum);
    }
    if holes.range {
        filled_field(p, "range", Hole::Range);
    }
    if let Some(name) = holes.condition {
        filled_field(p, name, Hole::Condition);
    }
    p.bytes(b"\r\n");
    p.body(d.body);
}

/// Writes the template of `describe` into `buf`, and lets `stash` write
/// text that no section holds after the table's text.
///
/// A container that names an object makes a template of that object, whose
/// text holds the key. An operation that names no object refuses such a
/// container with [`InvalidRequest::Option`].
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_template<const Q: usize>(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    describe: &Describe<'_, Q>,
    operation: Operation,
    mut detail: Detail,
    buf: &mut [u8],
    stash: impl FnOnce(&mut Builder<'_>, &mut Detail),
) -> Result<usize> {
    if !is_header_value(token.as_bytes()) {
        return Err(Error::InvalidToken);
    }
    let (scheme, host) = container
        .endpoint
        .split_once("://")
        .ok_or(InvalidRequest::Template)?;
    let object = match container.object {
        Some(key) if describe.key => {
            validate_key(key, namespace)?;
            Some(key)
        }
        Some(_) => return Err(InvalidRequest::Option.into()),
        None => None,
    };
    let fixed = Fixed {
        container,
        token,
        scheme,
        host,
    };
    let mut b = Builder::new(buf);
    layout(&mut b, describe, Some(fixed), object.is_some());
    stash(&mut b, &mut detail);
    let object = object.map(|key| b.stash(|out| out.push(key.as_bytes())));
    b.finish(|into| write_meta(into, operation, describe.method, namespace, detail, object))
}

/// Writes a request from `template` into `buf`, with the values that `fill`
/// writes, and returns where it is. `describe` is the template's, without
/// its text.
/// Inlined into each operation's encoder, which so runs its own layout.
#[inline(always)]
pub(super) fn encode_template<F: Fill, const Q: usize>(
    template: &Header,
    bytes: &[u8],
    fill: &F,
    content_len: u64,
    buf: &mut [u8],
    describe: &Describe<'_, Q>,
) -> Result<WireRequest> {
    // A URL that Azure would refuse is refused before any byte is written.
    // It is counted only when its values could make it too long.
    let surely_short = template
        .table
        .url_fixed()
        .saturating_add(fill.values().url_bound())
        <= MAX_URL_LEN;
    if !surely_short && url_too_long(template, bytes, fill, describe)? {
        return Err(InvalidRequest::UrlTooLong.into());
    }
    let mut out = Writer::new(buf);
    let mut state = template::WriteState::default();
    let mut pass = template::EncodePass::new(&template.table, bytes, fill, &mut out, &mut state);
    layout(&mut pass, describe, None, template.object.is_some());
    let written = pass.finish()?;
    let written = template::check_fit(&out, written)?;
    Ok(WireRequest::new(template.method, &written, content_len))
}

#[inline(never)]
fn url_too_long<F: Fill, const Q: usize>(
    template: &Header,
    bytes: &[u8],
    fill: &F,
    describe: &Describe<'_, Q>,
) -> Result<bool> {
    let mut counted = Writer::new(&mut []);
    let mut state = template::WriteState::default();
    let mut pass =
        template::EncodePass::new(&template.table, bytes, fill, &mut counted, &mut state);
    layout(&mut pass, describe, None, template.object.is_some());
    Ok(pass.finish()?.url.len > MAX_URL_LEN)
}

impl Header {
    // The key that a request names: `key`, checked, or `None` for a
    // template of one object, which writes its own. A request of such a
    // template passes that key, or an empty one.
    #[inline(always)]
    pub(super) fn request_key<'k>(&self, bytes: &[u8], key: &'k str) -> Result<Option<&'k str>> {
        match self.object {
            None => {
                validate_key(key, self.namespace)?;
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
