// Azure templates: what an Azure request is built from.
//
// An operation describes its request as a `Describe`: the method, whether
// the path ends in a key, the query, and the fields, each of which is fixed
// text or a hole. `prepare` writes the template from it. Azure signs nothing
// under a bearer token, so a request is ready as soon as it is written.

use super::{AzureNamespace, Bearer, Container, MAX_URL_LEN, METADATA_PREFIX, VERSION};
use crate::common::write_tags;
use crate::request::{HeaderValue, Writer};
use crate::template::{self, Builder, Fill, Hole, Param, Section, Table, WireRequest};
use crate::{InvalidPlan, MetadataPair, Method, Payload, Result, Span, Tag};

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
    Delete(crate::DeleteOptions),
    List,
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
}

fn write_value(out: &mut Writer<'_>, value: Value<'_>) {
    match value {
        Value::Bytes(bytes) => out.push(bytes),
        Value::Tags(tags) => write_tags(out, tags),
        Value::Hole(_) => {}
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
        if !matches!(field.value, Value::Hole(_)) {
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
        if let Value::Hole(hole) = field.value {
            b.with(|out| {
                out.push(field.name.as_bytes());
                out.push(b": ");
            });
            b.hole(hole);
            b.text(b"\r\n");
        }
    }
    b.text(b"\r\n");
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
    body: Option<Span>,
    buf: &'r mut [u8],
) -> Result<WireRequest<'r>> {
    // The URL is counted first, so that a URL that Azure would refuse is
    // refused before any byte is written.
    if template::url_len(&template.table, bytes, fill)? > MAX_URL_LEN {
        return Err(InvalidPlan::UrlTooLong.into());
    }
    let mut out = Writer::new(buf);
    let written = template::write(&template.table, bytes, fill, &mut out)?;
    let bytes: &'r [u8] = buf;
    Ok(WireRequest::new(
        bytes,
        template.method,
        &written,
        payload,
        body,
    ))
}
