// Azure templates: what an Azure request is built from.
//
// An operation describes its request as a `Describe`: the method, whether
// the path ends in a key, the query, and the fields, each of which is fixed
// text or a hole. `prepare` writes the template from it. Azure signs nothing
// under a bearer token, so a request is ready as soon as it is written.

use super::{AzureNamespace, Bearer, Container, MAX_URL_LEN, METADATA_PREFIX, VERSION};
use crate::common::write_tags;
use crate::request::{HeaderValue, Writer};
use crate::template::{self, Body, Builder, Fill, Hole, Param, Section, Table, WireRequest};
use crate::{InvalidPlan, MetadataPair, Method, Payload, Result, Tag};

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
#[derive(Debug, Clone, Copy)]
pub struct Template {
    pub(super) table: Table,
    pub(super) operation: Operation,
    pub(super) method: Method,
    pub(super) namespace: AzureNamespace,
    pub(super) detail: Detail,
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
    /// Returns the operation this template writes requests for.
    pub const fn operation(&self) -> Operation {
        self.operation
    }

    /// Returns the kind of account the requests go to.
    pub const fn namespace(&self) -> AzureNamespace {
        self.namespace
    }
}

// The value of one field.
#[derive(Clone, Copy)]
pub(super) enum Value<'p> {
    Bytes(&'p [u8]),
    Tags(&'p [Tag<'p>]),
    Hole(Hole),
    // `Bearer` and a token, as the field of a copy that reads its source
    // as a client would sends it.
    Bearer(&'p str),
    // The URL of a copy's source: the origin and the container, fixed, then
    // the key and the revision of each request.
    CopySource {
        origin: &'p str,
        container: &'p str,
        revision: crate::RevisionKind,
    },
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

// The fields of a request, gathered on the stack. An operation has at most
// a dozen fields of its own; metadata goes apart.
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

// One Azure request, described for its template.
pub(super) struct Describe<'p> {
    pub(super) method: Method,
    // Whether the path ends in the request's key, or names the container.
    pub(super) key: bool,
    pub(super) query: &'p [Param<'p>],
    pub(super) fields: &'p [Field<'p>],
    pub(super) metadata: &'p [MetadataPair<'p>],
    pub(super) body: Body<'p>,
}

fn write_value(out: &mut Writer<'_>, value: Value<'_>) {
    match value {
        Value::Bytes(bytes) => out.push(bytes),
        Value::Tags(tags) => write_tags(out, tags),
        Value::Bearer(token) => Bearer(token).write_to(out),
        Value::Hole(_) | Value::CopySource { .. } => {}
    }
}

/// Writes the template of `describe` into `buf`.
pub(super) fn prepare(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    describe: &Describe<'_>,
    operation: Operation,
    detail: Detail,
    buf: &mut [u8],
) -> Result<Template> {
    let mut template = prepare_with(
        container,
        token,
        namespace,
        describe,
        operation,
        buf,
        |_| {},
    )?;
    template.detail = detail;
    Ok(template)
}

/// Writes the template of `describe` into `buf`, and lets `stash` write
/// text that no section holds before the table. Its detail is `List`
/// until the caller sets it.
pub(super) fn prepare_with(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    describe: &Describe<'_>,
    operation: Operation,
    buf: &mut [u8],
    stash: impl FnOnce(&mut Builder<'_>),
) -> Result<Template> {
    let detail = Detail::List;
    if !crate::common::valid_header(token.as_bytes()) {
        return Err(crate::Error::InvalidToken);
    }
    let (scheme, host) = container
        .endpoint
        .split_once("://")
        .ok_or(InvalidPlan::Template)?;
    let mut b = Builder::new(buf);
    b.with(|out| {
        out.push(scheme.as_bytes());
        out.push(b"://");
        out.push(host.as_bytes());
    });
    b.section(Section::Path);
    b.with(|out| {
        out.push(b"/");
        out.push(container.name.as_bytes());
        if describe.key {
            out.push(b"/");
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
        out.push(host.as_bytes());
        out.push(b"\r\n");
    });
    b.hole(Hole::Fields);
    b.with(|out| {
        out.push(b"authorization: ");
        Bearer(token).write_to(out);
        out.push(b"\r\nx-ms-version: ");
        out.push(VERSION.as_bytes());
        out.push(b"\r\n");
    });
    for field in describe.fields {
        if !matches!(field.value, Value::Hole(_) | Value::CopySource { .. }) {
            b.with(|out| {
                out.push(field.name.as_bytes());
                out.push(b": ");
                write_value(out, field.value);
                out.push(b"\r\n");
            });
        }
    }
    for pair in describe.metadata {
        b.with(|out| {
            out.push(METADATA_PREFIX.as_bytes());
            out.push(pair.name.as_bytes());
            out.push(b": ");
            out.push(pair.value.as_bytes());
            out.push(b"\r\n");
        });
    }
    b.text(b"x-ms-date: ");
    b.hole(Hole::HttpTime);
    b.text(b"\r\n");
    for field in describe.fields {
        match field.value {
            Value::Hole(hole) => {
                b.with(|out| {
                    out.push(field.name.as_bytes());
                    out.push(b": ");
                });
                b.hole(hole);
            }
            Value::CopySource {
                origin,
                container,
                revision,
            } => {
                b.with(|out| {
                    out.push(field.name.as_bytes());
                    out.push(b": ");
                    out.push(origin.as_bytes());
                    out.push(b"/");
                    out.push(container.as_bytes());
                    out.push(b"/");
                });
                b.hole(Hole::SourceKey);
                match revision {
                    crate::RevisionKind::Current => {}
                    crate::RevisionKind::Snapshot => b.text(b"?snapshot="),
                    crate::RevisionKind::Version => b.text(b"?versionid="),
                }
                if revision != crate::RevisionKind::Current {
                    b.hole(Hole::SourceRevision);
                }
            }
            Value::Bytes(_) | Value::Tags(_) | Value::Bearer(_) => continue,
        }
        b.text(b"\r\n");
    }
    b.text(b"\r\n");
    b.body(describe.body);
    stash(&mut b);
    let table = b.finish()?;
    Ok(Template {
        table,
        operation,
        method: describe.method,
        namespace,
        detail,
    })
}

/// Writes a request from `template` into `buf`, with the values that `fill`
/// writes, and returns it.
pub(super) fn encode<'r>(
    template: &Template,
    bytes: &[u8],
    fill: &impl Fill,
    payload: Payload<'r>,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    // A URL that Azure would refuse is refused before any byte is written.
    // It is counted only when its values could make it too long.
    let surely_short = template.table.url_fixed().saturating_add(fill.url_bound()) <= MAX_URL_LEN;
    if !surely_short && template::url_len(&template.table, bytes, fill)? > MAX_URL_LEN {
        return Err(InvalidPlan::UrlTooLong.into());
    }
    let mut out = Writer::new(buf);
    let written = template::write(&template.table, bytes, fill, &mut out)?;
    let bytes: &'r [u8] = buf;
    Ok(WireRequest::new(bytes, template.method, &written, payload))
}
