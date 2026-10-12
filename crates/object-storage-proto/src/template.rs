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
// The table has five sections, written in this order:
//
// - origin: the scheme and the host of the URL;
// - path: the path of the URL, from its first `/`;
// - query: the query of the URL with its `?`, or nothing;
// - head: the HTTP/1.1 request head, from the request line to the empty
//   line;
// - canonical: the canonical request that S3 signs, which Azure has not.
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
}

impl Hole {
    const ALL: [Self; 21] = [
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
    ];

    fn from_id(id: u32) -> Option<Self> {
        Self::ALL.iter().copied().find(|hole| *hole as u32 == id)
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

/// The sections of a table, in the order that a request is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Section {
    Origin = 0,
    Path = 1,
    Query = 2,
    Head = 3,
    Canonical = 4,
}

const SECTIONS: usize = 5;

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
        Ok(Table {
            start,
            counts: self.counts,
            end,
        })
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
    write_sections(table, template, fill, out, SECTIONS)
}

/// Returns the length of the URL that a request from the template would
/// have, without writing it.
pub(crate) fn url_len(table: &Table, template: &[u8], fill: &impl Fill) -> Result<usize> {
    let mut counted = Writer::new(&mut []);
    write_sections(table, template, fill, &mut counted, Section::Head as usize)?;
    Ok(counted.position())
}

fn write_sections(
    table: &Table,
    template: &[u8],
    fill: &impl Fill,
    out: &mut Writer<'_>,
    sections: usize,
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
    for (section, &count) in table.counts.iter().enumerate().take(sections) {
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
    written.end = out.position();
    if sections == SECTIONS && !out.fits() {
        return Err(capacity_error(CapacityError {
            required: written.end,
            required_headers: 0,
        }));
    }
    Ok(written)
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
    pub(crate) fn new(
        bytes: &'r [u8],
        method: Method,
        written: &Written,
        payload: Payload<'r>,
        body: Option<Span>,
    ) -> Self {
        Self {
            bytes,
            method,
            url: written.url,
            head: written.head,
            fields: written.fields,
            payload,
            body,
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
            _ => return false,
        }
        true
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
