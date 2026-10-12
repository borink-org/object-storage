// Azure templates: what an Azure request is built from.
//
// An operation describes its request as a `Describe`: the method, whether
// the path ends in a key, the query, the fields that each request fills,
// and the fields that every request sends alike. `layout` writes the
// request from it, once as the template's pass and once as each request's:
// see `crate::template::Pass`. Azure signs nothing under a bearer token, so
// a request is ready as soon as it is written.

use super::{AzureNamespace, Bearer, Container, MAX_URL_LEN, METADATA_PREFIX, VERSION};
use crate::common::write_tags;
use crate::request::{HeaderValue, Writer};
use crate::template::{
    self, Body, Builder, Fill, Hole, Param, ParamValue, Pass, Section, Table, WireRequest,
};
use crate::{InvalidPlan, MetadataPair, Method, Payload, Result, RevisionKind, Span, Tag};

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

/// The part of every Azure request that is the same for each blob, written
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
    pub(super) namespace: AzureNamespace,
    pub(super) detail: Detail,
    // Where the template holds the key of its object, for a template of
    // one object.
    pub(super) object: Option<Span>,
}

// What an operation keeps in its template to check each request against.
#[derive(Debug, Clone, Copy)]
pub(super) enum Detail {
    Get(crate::GetOptions),
    Put {
        condition: crate::ConditionKind,
        checksum: Option<crate::checksum::ChecksumKind>,
    },
    // A request that names one object: the condition it carries, and the
    // kind of revision it names.
    Object {
        condition: crate::ConditionKind,
        revision: crate::RevisionKind,
    },
    List,
    Stage(Option<crate::checksum::ChecksumKind>),
    Commit {
        condition: crate::ConditionKind,
        content_md5: bool,
    },
    // A batch: where the template holds the start of each part's path, and
    // the fields that end each part.
    Batch {
        path: crate::Span,
        tail: crate::Span,
    },
    Copy {
        condition: crate::ConditionKind,
        source_revision: crate::RevisionKind,
        source_condition: crate::ConditionKind,
        source_ranging: crate::Ranging,
    },
}

impl Template {
    /// Checks that `bytes` are the bytes that this template was written
    /// into, as its `prepare_*` function wrote them. See [Bytes](Self#bytes).
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPlan::Template`] if they are not.
    pub fn check(&self, bytes: &[u8]) -> Result<()> {
        self.table.check(bytes)
    }

    /// Returns the operation this template writes requests for.
    pub const fn operation(&self) -> Operation {
        self.operation
    }

    /// Returns the kind of account the requests go to.
    pub const fn namespace(&self) -> AzureNamespace {
        self.namespace
    }
}

// The value of one field that every request sends alike.
#[derive(Clone, Copy)]
pub(super) enum Value<'p> {
    Bytes(&'p [u8]),
    Tags(&'p [Tag<'p>]),
    // `Bearer` and a token, as the field of a copy that reads its source
    // as a client would sends it.
    Bearer(&'p str),
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

// The fields that every request of a template sends alike, gathered on the
// stack. An operation has at most a dozen; metadata goes apart.
pub(super) struct FieldList<'p> {
    fields: [Field<'p>; 16],
    count: usize,
}

impl<'p> FieldList<'p> {
    pub(super) const fn new() -> Self {
        Self {
            fields: [Field::new("", Value::Bytes(b"")); 16],
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
    pub(super) body: Body<'p>,
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

fn write_value(out: &mut Writer<'_>, value: Value<'_>) {
    match value {
        Value::Bytes(bytes) => out.push(bytes),
        Value::Tags(tags) => write_tags(out, tags),
        Value::Bearer(token) => Bearer(token).write_to(out),
    }
}

// Whether every parameter of `query` is the same for each request, so that
// a URL with no key of its own is the template's text.
fn fixed_query(query: &[Param<'_>]) -> bool {
    query
        .iter()
        .flatten()
        .all(|(_, value)| matches!(value, ParamValue::Fixed(_)))
}

// Writes a query whose every parameter is fixed, as the template's pass
// writes it.
fn write_fixed_query(out: &mut Writer<'_>, query: &[Param<'_>]) {
    for (index, (name, value)) in query.iter().flatten().enumerate() {
        out.push(if index == 0 { b"?" } else { b"&" });
        out.push(name.as_bytes());
        out.push(b"=");
        if let ParamValue::Fixed(value) = value {
            crate::url::write_query_value(out, *value);
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
        for part in crate::url::encode_object_key(fixed.container.object.unwrap_or_default()) {
            out.push(part);
        }
    }
}

// A field that each request fills with `hole`.
#[inline(always)]
fn field(p: &mut impl Pass, name: &str, hole: Hole) {
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
                    for part in
                        crate::url::encode_object_key(fixed.container.object.unwrap_or_default())
                    {
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
    if (object || !d.key) && fixed_query(&d.query) {
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
        field(p, "x-ms-source-range", Hole::SourceRange);
    }
    if let Some(name) = holes.source_condition {
        field(p, name, Hole::SourceCondition);
    }
    if let Some(hole) = holes.content_length {
        field(p, "content-length", hole);
    }
    if let Some(name) = holes.checksum {
        field(p, name, Hole::Checksum);
    }
    if holes.range {
        field(p, "range", Hole::Range);
    }
    if let Some(name) = holes.condition {
        field(p, name, Hole::Condition);
    }
    p.bytes(b"\r\n");
    p.body(d.body);
}

/// Writes the template of `describe` into `buf`, and lets `stash` write
/// text that no section holds after the table's text.
///
/// A container that names an object makes a template of that object: its
/// key is the template's text, and an operation that names none refuses
/// it with [`InvalidPlan::Option`].
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare<const Q: usize>(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    describe: &Describe<'_, Q>,
    operation: Operation,
    detail: Detail,
    buf: &mut [u8],
    stash: impl FnOnce(&mut Builder<'_>),
) -> Result<Template> {
    if !crate::common::valid_header(token.as_bytes()) {
        return Err(crate::Error::InvalidToken);
    }
    let (scheme, host) = container
        .endpoint
        .split_once("://")
        .ok_or(InvalidPlan::Template)?;
    let object = match container.object {
        Some(key) if describe.key => {
            super::validate_key(key, namespace)?;
            Some(key)
        }
        Some(_) => return Err(InvalidPlan::Option.into()),
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
    stash(&mut b);
    let object = object.map(|key| b.stash(|out| out.push(key.as_bytes())));
    let table = b.finish()?;
    Ok(Template {
        table,
        operation,
        method: describe.method,
        namespace,
        detail,
        object,
    })
}

/// Writes a request from `template` into `buf`, with the values that `fill`
/// writes, and returns it. `describe` is the template's, without its text.
/// Inlined into each operation's encoder, which so runs its own layout.
#[inline(always)]
pub(super) fn encode<'r, F: Fill, const Q: usize>(
    template: &Template,
    bytes: &[u8],
    fill: &F,
    payload: Payload<'r>,
    buf: &'r mut [u8],
    describe: &Describe<'_, Q>,
) -> Result<WireRequest<'r>> {
    // A URL that Azure would refuse is refused before any byte is written.
    // It is counted only when its values could make it too long.
    let surely_short = template
        .table
        .url_fixed()
        .saturating_add(fill.values().url_bound())
        <= MAX_URL_LEN;
    // It is measured on a copy of the values, so that the values a request
    // is written from stay where the compiler put them.
    if !surely_short {
        let values = *fill.values();
        if template::url_len(&template.table, bytes, &values)? > MAX_URL_LEN {
            return Err(InvalidPlan::UrlTooLong.into());
        }
    }
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
    let bytes: &'r [u8] = buf;
    Ok(WireRequest::new(bytes, template.method, &written, payload))
}

impl Template {
    // The key that a request names: `key`, checked, or `None` for a
    // template of one object, which writes its own. A request of such a
    // template passes that key, or an empty one.
    pub(super) fn request_key<'k>(&self, bytes: &[u8], key: &'k str) -> Result<Option<&'k str>> {
        match self.object {
            None => {
                super::validate_key(key, self.namespace)?;
                Ok(Some(key))
            }
            Some(span)
                if key.is_empty()
                    || bytes.get(span.start..span.start + span.len) == Some(key.as_bytes()) =>
            {
                Ok(None)
            }
            Some(_) => Err(InvalidPlan::Template.into()),
        }
    }
}
