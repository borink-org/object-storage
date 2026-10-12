// Requests written from a template, for both services.
//
// A template is the part of a request that is the same for every object it
// serves: the origin, the path up to the key, the fields whose values never
// change, and the fixed text of the canonical request that S3 signs. It is
// written once into a buffer the caller owns, followed by a table of pieces.
// A piece is either a run of that fixed text or a hole, which each request
// fills with a value of its own, such as its key or the time.
//
// Encoding a request walks the table: it copies each run of fixed text and
// fills each hole. The request is written whole into its own buffer, so it
// borrows nothing from the template.
//
// The table has six sections, written in this order:
//
// - origin: the scheme and the host of the URL;
// - path: the path of the URL, from its first `/`;
// - query: the query of the URL with its `?`, or nothing;
// - head: the HTTP/1.1 request head, from the request line to the empty
//   line;
// - canonical: the canonical request that S3 signs, which Azure has not;
// - body: a body that this crate writes, such as a list of blocks, which
//   most requests have not.
//
// The head holds every field as a line `name: value\r\n`. The fields that a
// template fixes come first, as one run of text, and the fields that each
// request fills follow. Neither service reads meaning into the order of the
// fields: S3 signs them sorted by name in the canonical request.

use crate::common::capacity_error;
use crate::request::{Writer, text};
use crate::url::QueryValue;
use crate::{CapacityError, InvalidPlan, Method, Payload, Result, Span};

/// The value that a request writes into one hole of its template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Hole {
    // The path and the query of the URL, as the request has written them,
    // for its request line.
    Target = 1,
    // The path of the URL, for the canonical request.
    Path = 2,
    // The query of the URL without its `?`, for the canonical request.
    Query = 3,
    // The placeholder of an S3 signature, 64 bytes that `finish` replaces.
    Signature = 4,
    // The object key, percent-encoded as a path.
    Key = 5,
    // The value of a `Range` field.
    Range = 6,
    // The value of a condition, as the field sends it.
    Condition = 7,
    // The value of a condition, as S3 signs it.
    SignedCondition = 8,
    // The snapshot or version, percent-encoded as a query value.
    Revision = 9,
    // The time as `x-amz-date` writes it.
    IsoTime = 10,
    // The time as an HTTP date, for `x-ms-date`.
    HttpTime = 11,
    // The day of the time, as the scope of an S3 signature names it.
    Day = 12,
    // The length of the content, in decimal.
    ContentLength = 13,
    // The SHA-256 of the content, in lowercase hexadecimal.
    PayloadHash = 14,
    // The base64 of the checksum of the content.
    Checksum = 15,
    // Writes nothing, and marks where the head's field lines start: after
    // the request line and `host`.
    Fields = 16,
    // The separator before a query parameter: `?` before the first one that
    // the request writes, and `&` before every other.
    Separator = 17,
    // The optional parameters of a listing, each followed by a piece that
    // holds its name. A request that leaves one out writes neither.
    //
    // The prefix that the listing's keys start with.
    Prefix = 18,
    // Where the previous page ended, or the key of an S3 version marker.
    Marker = 19,
    // The text after which an S3 listing starts.
    StartAfter = 20,
    // The version of an S3 version marker.
    VersionMarker = 21,
    // The part of a body that each request writes, such as its list of
    // parts.
    Body = 22,
    // The length of the body, in decimal, which the engine counts.
    BodyLength = 23,
    // A placeholder for the SHA-256 of the body in lowercase hexadecimal, 64
    // bytes that the caller fills once the body is written.
    BodyHash = 24,
    // A placeholder for the base64 of a checksum of the body, as long as the
    // checksum's text, which the caller fills once the body is written.
    BodyChecksum = 25,
    // The ID of an Azure block, percent-encoded as a query value.
    BlockId = 26,
    // The ID of an S3 upload, percent-encoded as a query value.
    UploadId = 27,
    // The number of an S3 part, in decimal.
    PartNumber = 28,
    // The key of a copy's source, percent-encoded as a path.
    SourceKey = 29,
    // The snapshot or version of a copy's source, percent-encoded as a query
    // value.
    SourceRevision = 30,
    // The value of a condition on a copy's source, as the field sends it.
    SourceCondition = 31,
    // The value of a condition on a copy's source, as S3 signs it.
    SignedSourceCondition = 32,
    // The bytes of a copy's source, as `bytes=a-b`.
    SourceRange = 33,
    // The ID of an Azure copy, as a query value.
    CopyId = 34,
    // The length of the object that an S3 commit publishes, in decimal.
    ObjectSize = 35,
}

impl Hole {
    const ALL: [Self; 35] = [
        Self::Target,
        Self::Path,
        Self::Query,
        Self::Signature,
        Self::Key,
        Self::Range,
        Self::Condition,
        Self::SignedCondition,
        Self::Revision,
        Self::IsoTime,
        Self::HttpTime,
        Self::Day,
        Self::ContentLength,
        Self::PayloadHash,
        Self::Checksum,
        Self::Fields,
        Self::Separator,
        Self::Prefix,
        Self::Marker,
        Self::StartAfter,
        Self::VersionMarker,
        Self::Body,
        Self::BodyLength,
        Self::BodyHash,
        Self::BodyChecksum,
        Self::BlockId,
        Self::UploadId,
        Self::PartNumber,
        Self::SourceKey,
        Self::SourceRevision,
        Self::SourceCondition,
        Self::SignedSourceCondition,
        Self::SourceRange,
        Self::CopyId,
        Self::ObjectSize,
    ];

    // The holes are numbered from 1 in the order of `ALL`.
    fn from_id(id: u32) -> Option<Self> {
        Self::ALL.get((id as usize).wrapping_sub(1)).copied()
    }

    // Whether the hole is an optional parameter, whose name follows it.
    const fn is_optional(self) -> bool {
        matches!(
            self,
            Self::Prefix | Self::Marker | Self::StartAfter | Self::VersionMarker
        )
    }
}

// The value of one query parameter of a template.
#[derive(Clone, Copy)]
pub(crate) enum ParamValue<'p> {
    // A value that every request sends.
    Fixed(QueryValue<'p>),
    // A value that each request fills.
    Hole(Hole),
    // A value that each request may leave out, and the parameter with it.
    Optional(Hole),
}

// One query parameter, or `None` for one that the template leaves out.
pub(crate) type Param<'p> = Option<(&'p str, ParamValue<'p>)>;

// The body of a template's requests: none, the same text for every request,
// or a body that each request writes.
#[derive(Clone, Copy)]
pub(crate) enum Body<'b> {
    None,
    Fixed(&'b dyn Fn(&mut dyn crate::request::ByteSink)),
    Each,
}

/// The sections of a table, in the order that a request is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Section {
    Origin = 0,
    Path = 1,
    Query = 2,
    Head = 3,
    Canonical = 4,
    Body = 5,
}

const SECTIONS: usize = 6;

// The most pieces one template holds. A listing of S3 versions, with every
// parameter filled per request, needs about forty.
const MAX_PIECES: usize = 96;

// A run of fixed text in the template, or a hole: `start` is `HOLE` and
// `len` names the hole.
#[derive(Debug, Clone, Copy, Default)]
struct Piece {
    start: u32,
    len: u32,
}

const HOLE: u32 = u32::MAX;

/// Where a template's table of pieces is in its bytes, and how many pieces
/// each section has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Table {
    start: u32,
    counts: [u8; SECTIONS],
    // The bytes the template takes, the table included.
    end: u32,
    // The length of the fixed text of the URL, without the values that each
    // request writes into it.
    url_fixed: u32,
}

/// Writes a template: its fixed text, and the table of pieces after it.
pub(crate) struct Builder<'t> {
    out: Writer<'t>,
    pieces: [Piece; MAX_PIECES],
    count: usize,
    counts: [u8; SECTIONS],
    section: Section,
}

impl<'t> Builder<'t> {
    pub(crate) fn new(buf: &'t mut [u8]) -> Self {
        Self {
            out: Writer::new(buf),
            pieces: [Piece::default(); MAX_PIECES],
            count: 0,
            counts: [0; SECTIONS],
            section: Section::Origin,
        }
    }

    // Starts the next section. Sections are started in order.
    pub(crate) fn section(&mut self, section: Section) {
        debug_assert!(section >= self.section, "sections are written in order");
        self.section = section;
    }

    // Writes fixed text with `write`.
    pub(crate) fn with(&mut self, write: impl FnOnce(&mut Writer<'t>)) {
        let span = self.out.part(write);
        if span.len == 0 {
            return;
        }
        // Text written right after the previous piece of the same section
        // extends it.
        if self.counts[self.section as usize] > 0 {
            let last = &mut self.pieces[self.count - 1];
            if last.start != HOLE && (last.start + last.len) as usize == span.start {
                last.len += span.len as u32;
                return;
            }
        }
        self.push(Piece {
            start: span.start as u32,
            len: span.len as u32,
        });
    }

    // Writes fixed text.
    pub(crate) fn text(&mut self, bytes: &[u8]) {
        self.with(|out| out.push(bytes));
    }

    // Leaves a hole that each request fills.
    pub(crate) fn hole(&mut self, hole: Hole) {
        self.push(Piece {
            start: HOLE,
            len: hole as u32,
        });
    }

    // Writes the query of the URL. Each request writes the separator before
    // each parameter itself, so that a parameter it leaves out takes its
    // separator along.
    pub(crate) fn query(&mut self, params: &[Param<'_>]) {
        self.section(Section::Query);
        for (name, value) in params.iter().flatten() {
            match *value {
                ParamValue::Fixed(value) => {
                    self.hole(Hole::Separator);
                    self.with(|out| {
                        out.push(name.as_bytes());
                        out.push(b"=");
                        crate::url::write_query_value(out, value);
                    });
                }
                ParamValue::Hole(hole) => {
                    self.hole(Hole::Separator);
                    self.with(|out| {
                        out.push(name.as_bytes());
                        out.push(b"=");
                    });
                    self.hole(hole);
                }
                // The name is a piece of its own, after the hole.
                ParamValue::Optional(hole) => {
                    self.hole(hole);
                    self.text(name.as_bytes());
                }
            }
        }
    }

    // Writes text into the template that no section holds, for a request to
    // read where the returned span says, and returns that span.
    pub(crate) fn stash(&mut self, write: impl FnOnce(&mut Writer<'t>)) -> Span {
        self.out.part(write)
    }

    // Writes the body section.
    pub(crate) fn body(&mut self, body: Body<'_>) {
        self.section(Section::Body);
        match body {
            Body::None => {}
            Body::Fixed(write) => self.with(|out| write(out)),
            Body::Each => self.hole(Hole::Body),
        }
    }

    fn push(&mut self, piece: Piece) {
        // A template past the table's bound is a fault of this crate's own
        // operations, which `finish` reports.
        if let Some(slot) = self.pieces.get_mut(self.count) {
            *slot = piece;
        }
        self.count += 1;
        let count = &mut self.counts[self.section as usize];
        *count = count.saturating_add(1);
    }

    // Appends the table after the text, and returns where it is. A template
    // that does not fit its buffer is refused with the room it needs.
    pub(crate) fn finish(mut self) -> Result<Table> {
        debug_assert!(self.count <= MAX_PIECES, "a template holds too many pieces");
        if self.count > MAX_PIECES {
            return Err(InvalidPlan::RequestTooLarge.into());
        }
        let start = self.out.position();
        for piece in &self.pieces[..self.count] {
            self.out.push(&piece.start.to_le_bytes());
            self.out.push(&piece.len.to_le_bytes());
        }
        let end = self.out.position();
        if !self.out.fits() {
            return Err(capacity_error(CapacityError {
                required: end,
                required_headers: 0,
            }));
        }
        let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(end)) else {
            return Err(InvalidPlan::RequestTooLarge.into());
        };
        // The URL's sections come first, so their pieces do too.
        let url_pieces: usize = self.counts[..Section::Head as usize]
            .iter()
            .map(|&count| count as usize)
            .sum();
        let url_fixed = self.pieces[..url_pieces]
            .iter()
            .filter(|piece| piece.start != HOLE)
            .map(|piece| piece.len)
            .sum();
        Ok(Table {
            start,
            counts: self.counts,
            end,
            url_fixed,
        })
    }
}

impl Table {
    // The length of the fixed text of the URL.
    pub(crate) const fn url_fixed(&self) -> usize {
        self.url_fixed as usize
    }

    // The length of the body, from its fixed text and the length that
    // `fill` knows of what it writes, or `None` if it must be counted.
    fn body_len(&self, template: &[u8], fill: &impl Fill) -> Result<Option<usize>> {
        let pieces = self.pieces(template)?;
        let before: usize = self.counts[..Section::Body as usize]
            .iter()
            .map(|&count| count as usize)
            .sum();
        let mut len = 0;
        for chunk in pieces.chunks_exact(8).skip(before) {
            let start = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let piece_len = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
            len += match (start, Hole::from_id(piece_len)) {
                (HOLE, Some(Hole::Body)) => match fill.body_len() {
                    Some(len) => len,
                    None => return Ok(None),
                },
                (HOLE, _) => return Ok(None),
                _ => piece_len as usize,
            };
        }
        Ok(Some(len))
    }
}

/// Writes the value of each hole that a request fills.
pub(crate) trait Fill {
    // Writes the value of `hole`. Returns `false` for a hole that this
    // request has no value for, which means the template is for another
    // operation.
    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool;

    // Returns the value of the optional parameter `hole`, as text to
    // percent-encode: `Some(None)` for a request that leaves it out, and
    // `None` for a hole that this request has no value for.
    fn param(&self, hole: Hole) -> Option<Option<&[u8]>> {
        let _ = hole;
        None
    }

    // Returns the length of the body that `Hole::Body` writes, if the fill
    // knows it without writing it.
    fn body_len(&self) -> Option<usize> {
        None
    }

    // Returns at least the bytes that the request's values take in its URL,
    // so that a URL that is surely short enough needs no counting.
    fn url_bound(&self) -> usize {
        usize::MAX
    }
}

/// Where a request's parts are in its buffer, once it is written.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Written {
    pub(crate) url: Span,
    pub(crate) head: Span,
    // The field lines in the head, after the request line and `host`.
    pub(crate) fields: Span,
    pub(crate) canonical: Span,
    pub(crate) signature: Option<Span>,
    // The body that the request wrote, if its template has one.
    pub(crate) body: Option<Span>,
    // The placeholders for the SHA-256 and the checksum of the body: in the
    // head, and in the canonical request that S3 signs, whose last line
    // repeats the SHA-256.
    pub(crate) body_hash: [Option<Span>; 3],
    pub(crate) body_checksum: [Option<Span>; 2],
    pub(crate) end: usize,
}

impl Table {
    /// The bytes of the template that this table describes, or the refusal
    /// of bytes that cannot be its own.
    fn pieces<'t>(&self, template: &'t [u8]) -> Result<&'t [u8]> {
        let (start, end) = (self.start as usize, self.end as usize);
        let count: usize = self.counts.iter().map(|&count| count as usize).sum();
        if template.len() < end || end - start != count * 8 {
            return Err(InvalidPlan::Template.into());
        }
        Ok(&template[start..end])
    }
}

/// Writes a request from the template `template`, whose table is `table`,
/// into `out`, with the values that `fill` writes.
pub(crate) fn write(
    table: &Table,
    template: &[u8],
    fill: &impl Fill,
    out: &mut Writer<'_>,
) -> Result<Written> {
    // The head states the length of the body, which is counted first, by
    // writing the body section alone into nothing.
    let body = Section::Body as usize;
    let body_len = if table.counts[body] == 0 {
        0
    } else if let Some(len) = table.body_len(template, fill)? {
        len
    } else {
        let mut counted = Writer::new(&mut []);
        write_sections(table, template, fill, &mut counted, body..SECTIONS, 0)?;
        counted.position()
    };
    let written = write_sections(table, template, fill, out, 0..SECTIONS, body_len)?;
    if !out.fits() {
        return Err(capacity_error(CapacityError {
            required: written.end,
            required_headers: 0,
        }));
    }
    Ok(written)
}

/// Returns the length of the URL that a request from the template would
/// have, without writing it.
pub(crate) fn url_len(table: &Table, template: &[u8], fill: &impl Fill) -> Result<usize> {
    let mut counted = Writer::new(&mut []);
    write_sections(
        table,
        template,
        fill,
        &mut counted,
        0..Section::Head as usize,
        0,
    )?;
    Ok(counted.position())
}

fn write_sections(
    table: &Table,
    template: &[u8],
    fill: &impl Fill,
    out: &mut Writer<'_>,
    sections: core::ops::Range<usize>,
    body_len: usize,
) -> Result<Written> {
    let pieces = table.pieces(template)?;
    let mut spans = [Span::default(); SECTIONS];
    let mut written = Written::default();
    let mut fields = 0;
    let mut params = 0;
    let mut pieces = pieces.chunks_exact(8).map(|chunk| Piece {
        start: u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
        len: u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
    });
    let fixed = |piece: Piece| {
        let (from, to) = (
            piece.start as usize,
            piece.start as usize + piece.len as usize,
        );
        match template.get(from..to) {
            Some(bytes) if piece.start != HOLE => Ok(bytes),
            _ => Err(crate::Error::from(InvalidPlan::Template)),
        }
    };
    for (section, &count) in table.counts.iter().enumerate().take(sections.end) {
        // A section before the range is skipped, piece by piece.
        if section < sections.start {
            pieces.by_ref().take(count as usize).for_each(drop);
            continue;
        }
        let start = out.position();
        let mut section_pieces = pieces.by_ref().take(count as usize);
        while let Some(piece) = section_pieces.next() {
            if piece.start != HOLE {
                out.push(fixed(piece)?);
                continue;
            }
            // The path and the query are written before any section that
            // repeats them.
            let (path, query) = (
                spans[Section::Path as usize],
                spans[Section::Query as usize],
            );
            match Hole::from_id(piece.len) {
                Some(Hole::Target) => out.repeat(Span {
                    start: path.start,
                    len: path.len + query.len,
                }),
                Some(Hole::Path) => out.repeat(path),
                // The query without its `?`.
                Some(Hole::Query) if query.len > 0 => out.repeat(Span {
                    start: query.start + 1,
                    len: query.len - 1,
                }),
                Some(Hole::Query) => {}
                Some(Hole::Fields) => fields = out.position(),
                Some(Hole::Signature) => {
                    written.signature = Some(out.part(|out| out.push(&[b'0'; 64])));
                }
                Some(Hole::BodyLength) => {
                    out.push(crate::request::U64Decimal::new(body_len as u64).as_bytes());
                }
                Some(Hole::BodyHash) => {
                    let span = out.part(|out| out.push(&[b'0'; 64]));
                    record(&mut written.body_hash, span);
                }
                Some(Hole::BodyChecksum) => {
                    let mut filled = false;
                    let span = out.part(|out| filled = fill.write(Hole::BodyChecksum, out));
                    if !filled {
                        return Err(InvalidPlan::Template.into());
                    }
                    record(&mut written.body_checksum, span);
                }
                Some(Hole::Separator) => {
                    out.push(if params == 0 { b"?" } else { b"&" });
                    params += 1;
                }
                Some(hole) if hole.is_optional() => {
                    let name = section_pieces.next().ok_or(InvalidPlan::Template)?;
                    match fill.param(hole) {
                        Some(Some(value)) => {
                            out.push(if params == 0 { b"?" } else { b"&" });
                            params += 1;
                            out.push(fixed(name)?);
                            out.push(b"=");
                            for part in crate::url::encode_query_value(value) {
                                out.push(part);
                            }
                        }
                        Some(None) => {}
                        None => return Err(InvalidPlan::Template.into()),
                    }
                }
                Some(hole) if fill.write(hole, out) => {}
                _ => return Err(InvalidPlan::Template.into()),
            }
        }
        spans[section] = Span {
            start,
            len: out.position() - start,
        };
    }
    let origin = spans[Section::Origin as usize];
    written.url = Span {
        start: origin.start,
        len: origin.len + spans[Section::Path as usize].len + spans[Section::Query as usize].len,
    };
    written.head = spans[Section::Head as usize];
    // The head ends with the empty line, which no field holds.
    let head_end = written.head.start + written.head.len;
    written.fields = Span {
        start: fields,
        len: head_end.saturating_sub(2).saturating_sub(fields),
    };
    written.canonical = spans[Section::Canonical as usize];
    written.body =
        (table.counts[Section::Body as usize] > 0).then_some(spans[Section::Body as usize]);
    written.end = out.position();
    Ok(written)
}

// Records the span of a placeholder in the first free slot: the head's,
// then the canonical request's.
fn record(slots: &mut [Option<Span>], span: Span) {
    if let Some(slot) = slots.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(span);
    }
}

/// One request, written whole into a buffer you own.
///
/// Send it with any HTTP client. A client that takes a URL and a list of
/// header fields reads [`Self::url`] and [`Self::headers`], and writes
/// `host` from the URL itself. A client that writes HTTP/1.1 itself sends
/// [`Self::head`], which holds the request line and `host` too, and then
/// the content.
///
/// # Lifetime
///
/// The request borrows the buffer it was written into, and the content of a
/// write that you passed as [`Payload::Slice`]. It borrows nothing else, so
/// the template and every value you encoded it from can go.
#[derive(Debug, Clone, Copy)]
pub struct WireRequest<'r> {
    bytes: &'r [u8],
    method: Method,
    url: Span,
    head: Span,
    fields: Span,
    payload: Payload<'r>,
    body: Option<Span>,
}

impl<'r> WireRequest<'r> {
    // A request with a body that this crate wrote sends that body, and any
    // other sends `payload`.
    pub(crate) fn new(
        bytes: &'r [u8],
        method: Method,
        written: &Written,
        payload: Payload<'r>,
    ) -> Self {
        let payload = match written.body {
            Some(body) => Payload::Slice(&bytes[body.start..body.start + body.len]),
            None => payload,
        };
        Self {
            bytes,
            method,
            url: written.url,
            head: written.head,
            fields: written.fields,
            payload,
            body: written.body,
        }
    }

    /// Returns the HTTP method.
    pub fn method(&self) -> Method {
        self.method
    }

    /// Returns the URL.
    ///
    /// Send it unchanged. S3 signs the `host` that your client writes from
    /// it, and the path and the query as they are written here.
    pub fn url(&self) -> &'r str {
        text(self.slice(self.url))
    }

    /// Returns each header field as a name and a value, other than `host`.
    ///
    /// Your HTTP client writes `host` from the URL. Write every field that
    /// this returns, unchanged and none twice.
    pub fn headers(&self) -> impl Iterator<Item = (&'r str, &'r str)> + Clone + use<'r> {
        Fields(self.slice(self.fields))
    }

    /// Returns the HTTP/1.1 request head: the request line, `host`, every
    /// field line and the empty line that ends the head.
    ///
    /// Write these bytes to a connection to the URL's host, then the
    /// content, if any.
    pub fn head(&self) -> &'r [u8] {
        self.slice(self.head)
    }

    /// Returns the content of the request.
    ///
    /// A read has no content, so this is an empty [`Payload::Slice`]. For a
    /// write of streamed content this states the length that you must send,
    /// and carries no bytes. For a body that this crate wrote, such as a
    /// list of parts, it lends that body.
    pub fn payload(&self) -> Payload<'r> {
        self.payload
    }

    /// Returns where the URL is in the buffer.
    pub fn url_span(&self) -> Span {
        self.url
    }

    /// Returns where the HTTP/1.1 head is in the buffer.
    pub fn head_span(&self) -> Span {
        self.head
    }

    /// Returns where the field lines are in the buffer: every field but
    /// `host`, each written as `name: value` and a CRLF.
    pub fn fields_span(&self) -> Span {
        self.fields
    }

    /// Returns where the body that this crate wrote is in the buffer, or
    /// [`None`] for a request whose content you supply.
    pub fn body_span(&self) -> Option<Span> {
        self.body
    }

    fn slice(&self, span: Span) -> &'r [u8] {
        &self.bytes[span.start..span.start + span.len]
    }
}

// The field lines of a head, each read as its name and its value.
#[derive(Clone)]
struct Fields<'r>(&'r [u8]);

impl<'r> Iterator for Fields<'r> {
    type Item = (&'r str, &'r str);

    fn next(&mut self) -> Option<Self::Item> {
        let end = self.0.windows(2).position(|pair| pair == b"\r\n")?;
        let (line, rest) = self.0.split_at(end);
        self.0 = &rest[2..];
        // A name holds no `:`, so the first `: ` ends it.
        let colon = line.windows(2).position(|pair| pair == b": ")?;
        Some((text(&line[..colon]), text(&line[colon + 2..])))
    }
}

/// The values of one request that fill the holes both services share.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Values<'i> {
    pub(crate) now: Option<&'i crate::Timestamps>,
    pub(crate) key: Option<&'i str>,
    pub(crate) range: Option<crate::RequestedRange>,
    pub(crate) condition: Option<crate::ConditionValue<'i>>,
    pub(crate) revision: Option<&'i str>,
    pub(crate) content_length: Option<u64>,
    pub(crate) checksum: Option<&'i str>,
    pub(crate) payload_hash: Option<&'i [u8; 64]>,
    // The optional parameters of a listing. An empty text leaves its
    // parameter out.
    pub(crate) prefix: Option<&'i str>,
    pub(crate) marker: Option<&'i str>,
    pub(crate) start_after: Option<&'i str>,
    pub(crate) version_marker: Option<&'i str>,
    // What names one part of an object written in parts.
    pub(crate) block_id: Option<&'i str>,
    pub(crate) upload_id: Option<&'i str>,
    pub(crate) part_number: Option<u32>,
    // The source of a copy, and the ID of an Azure copy.
    pub(crate) source_key: Option<&'i str>,
    pub(crate) source_revision: Option<&'i str>,
    pub(crate) source_condition: Option<crate::ConditionValue<'i>>,
    pub(crate) source_range: Option<crate::RequestedRange>,
    pub(crate) copy_id: Option<&'i str>,
    // The kind of checksum of a body that this crate writes, whose
    // placeholder the request holds, and the length of an S3 object.
    pub(crate) body_checksum: Option<crate::checksum::ChecksumKind>,
    pub(crate) object_size: Option<u64>,
}

impl Fill for Values<'_> {
    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        use crate::request::HeaderValue;
        match hole {
            Hole::Key => match self.key {
                Some(key) => {
                    for part in crate::url::encode_object_key(key) {
                        out.push(part);
                    }
                }
                None => return false,
            },
            Hole::Range => match self.range {
                Some(range) => crate::common::write_range(out, range),
                None => return false,
            },
            Hole::Condition => match self.condition {
                Some(value) => value.write_to(out),
                None => return false,
            },
            Hole::Revision => match self.revision {
                Some(revision) => {
                    for part in crate::url::encode_query_value(revision.as_bytes()) {
                        out.push(part);
                    }
                }
                None => return false,
            },
            Hole::IsoTime => match self.now {
                Some(now) => out.push(now.iso8601_bytes()),
                None => return false,
            },
            Hole::HttpTime => match self.now {
                Some(now) => out.push(now.rfc1123_bytes()),
                None => return false,
            },
            Hole::Day => match self.now {
                Some(now) => out.push(now.date()),
                None => return false,
            },
            Hole::ContentLength => match self.content_length {
                Some(len) => out.push(crate::request::U64Decimal::new(len).as_bytes()),
                None => return false,
            },
            Hole::Checksum => match self.checksum {
                Some(text) => out.push(text.as_bytes()),
                None => return false,
            },
            Hole::PayloadHash => match self.payload_hash {
                Some(hex) => out.push(hex),
                None => return false,
            },
            Hole::BlockId | Hole::UploadId => match self.block_id.or(self.upload_id) {
                Some(text) => {
                    for part in crate::url::encode_query_value(text.as_bytes()) {
                        out.push(part);
                    }
                }
                None => return false,
            },
            Hole::SourceKey => match self.source_key {
                Some(key) => {
                    for part in crate::url::encode_object_key(key) {
                        out.push(part);
                    }
                }
                None => return false,
            },
            Hole::SourceRevision | Hole::CopyId => match self.source_revision.or(self.copy_id) {
                Some(text) => {
                    for part in crate::url::encode_query_value(text.as_bytes()) {
                        out.push(part);
                    }
                }
                None => return false,
            },
            Hole::SourceCondition => match self.source_condition {
                Some(value) => value.write_to(out),
                None => return false,
            },
            Hole::SourceRange => match self.source_range {
                Some(range) => crate::common::write_range(out, range),
                None => return false,
            },
            Hole::BodyChecksum => match self.body_checksum {
                Some(kind) => {
                    for _ in 0..kind.digest_len().div_ceil(3) * 4 {
                        out.push(b"=");
                    }
                }
                None => return false,
            },
            Hole::ObjectSize => match self.object_size {
                Some(size) => out.push(crate::request::U64Decimal::new(size).as_bytes()),
                None => return false,
            },
            Hole::PartNumber => match self.part_number {
                Some(number) => out.push(crate::request::U64Decimal::new(number.into()).as_bytes()),
                None => return false,
            },
            _ => return false,
        }
        true
    }

    fn url_bound(&self) -> usize {
        // Percent-encoding writes at most three bytes for one, and each
        // parameter adds its separator and a name of under 24 bytes.
        let texts = [
            self.key,
            self.revision,
            self.prefix,
            self.marker,
            self.start_after,
            self.version_marker,
            self.block_id,
            self.upload_id,
            self.copy_id,
        ];
        texts
            .iter()
            .flatten()
            .map(|text| 3 * text.len() + 25)
            .sum::<usize>()
            + 25
    }

    fn param(&self, hole: Hole) -> Option<Option<&[u8]>> {
        let value = match hole {
            Hole::Prefix => self.prefix,
            Hole::Marker => self.marker,
            Hole::StartAfter => self.start_after,
            Hole::VersionMarker => self.version_marker,
            _ => return None,
        };
        Some(value.filter(|text| !text.is_empty()).map(str::as_bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::Hole;

    // `Hole::from_id` reads a hole by its number, so `ALL` lists the holes
    // in the order of their numbers, from 1.
    #[test]
    fn the_holes_are_listed_in_the_order_of_their_numbers() {
        for (index, hole) in Hole::ALL.iter().enumerate() {
            assert_eq!(*hole as usize, index + 1);
            assert_eq!(Hole::from_id(index as u32 + 1), Some(*hole));
        }
        assert_eq!(Hole::from_id(0), None);
        assert_eq!(Hole::from_id(Hole::ALL.len() as u32 + 1), None);
    }
}
