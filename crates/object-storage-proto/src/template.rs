// Requests written from a template, for both services.
//
// A template is the part of a request that is the same for every object it
// serves. That is the origin, the path up to the key, the fields whose values
// never change, and the fixed text of the canonical request that S3 signs.
// It is checked and written once, into a buffer the caller owns, followed
// by a table of steps. A step is a run of that fixed text and the hole after
// it. Each request fills a hole with a value of its own, such as its key or
// the time. The runs follow each other in the text, so a step holds only
// their lengths. A request then only copies text and writes its own values.
//
// Each operation describes its request once, in a function generic over a
// `Pass`. Run with the template's pass, it writes the fixed text and the
// step lengths. Run with a request's pass, it copies each run of fixed text
// from the template and fills the hole after it, as straight-line code.
// Either way the request is written whole into its own buffer, so it borrows
// nothing from the template.
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

use crate::checksum::ChecksumKind;
use crate::error::capacity_error;
use crate::request::{ByteSink, U64Decimal, Writer, write_range, written_text};
use crate::url::{
    QueryValue, encode_object_key, encode_query_value, key_is_plain, write_query_value,
};
use crate::{
    CapacityError, ConditionValue, InvalidRequest, Method, RequestedRange, Result, Span, Timestamps,
};

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
    // The placeholder of an S3 signature, 64 bytes that `s3::set_signature` replaces.
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
    // The separator before a query parameter: `?` before the first one that
    // the request writes, and `&` before every other.
    Separator = 16,
    // The optional parameters of a listing, each in a step that holds its
    // name. A request that leaves one out writes neither.
    //
    // The prefix that the listing's keys start with.
    Prefix = 17,
    // Where the previous page ended, or the key of an S3 version marker.
    Marker = 18,
    // The text after which an S3 listing starts.
    StartAfter = 19,
    // The version of an S3 version marker.
    VersionMarker = 20,
    // The part of a body that each request writes, such as its list of
    // parts.
    Body = 21,
    // The length of the body, in decimal, which the engine counts.
    BodyLength = 22,
    // A placeholder for the SHA-256 of the body in lowercase hexadecimal, 64
    // bytes that the caller fills once the body is written.
    BodyHash = 23,
    // A placeholder for the base64 of a checksum of the body, as long as the
    // checksum's text, which the caller fills once the body is written.
    BodyChecksum = 24,
    // The ID of an Azure block, percent-encoded as a query value.
    BlockId = 25,
    // The ID of an S3 upload, percent-encoded as a query value.
    UploadId = 26,
    // The number of an S3 part, in decimal.
    PartNumber = 27,
    // The key of a copy's source, percent-encoded as a path.
    SourceKey = 28,
    // The snapshot or version of a copy's source, percent-encoded as a query
    // value.
    SourceRevision = 29,
    // The value of a condition on a copy's source, as the field sends it.
    SourceCondition = 30,
    // The value of a condition on a copy's source, as S3 signs it.
    SignedSourceCondition = 31,
    // The bytes of a copy's source, as `bytes=a-b`.
    SourceRange = 32,
    // The ID of an Azure copy, as a query value.
    CopyId = 33,
    // The length of the object that an S3 commit publishes, in decimal.
    ObjectSize = 34,
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

// Whether every parameter of `query` is the same for each request, so that
// a URL with no key of its own is the template's text.
pub(crate) fn fixed_query(query: &[Param<'_>]) -> bool {
    query
        .iter()
        .flatten()
        .all(|(_, value)| matches!(value, ParamValue::Fixed(_)))
}

// The body of a template's requests: none, the same text for every request,
// or a body that each request writes.
#[derive(Clone, Copy)]
pub(crate) enum TemplateBody<'b> {
    None,
    Fixed(FixedText<'b>),
    Each,
}

// Writes text that every request sends alike, such as a fixed body.
pub(crate) type FixedText<'t> = &'t dyn Fn(&mut dyn ByteSink);

// The text of a fixed body, for a request's describe: a request's pass
// writes no text.
pub(crate) const NO_TEXT: FixedText<'static> = &|_| {};

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

// The most steps one template holds. A listing of S3 versions, with every
// parameter filled per request, needs about thirty.
const MAX_STEPS: usize = 96;

// The fixed header at the start of every template buffer: service metadata
// first (starting with the 4-byte magic), then the table header.
pub(crate) const META_LEN: usize = 40;
pub(crate) const TABLE_HEADER_LEN: usize = 24;
pub(crate) const HEADER_LEN: usize = META_LEN + TABLE_HEADER_LEN;
// The trailing 8-byte fingerprint covering everything before it.
pub(crate) const TRAILER_LEN: usize = 8;

/// Where a template's step lengths are in its bytes, and the fixed counts
/// that encoding reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Table {
    start: u32,
    // The steps of all sections.
    total: u8,
    // The query parameters that the template writes as text, before the
    // first that a request may leave out. A request counts on from here.
    params: u8,
    // The bytes the template takes, header, step lengths and fingerprint
    // included.
    end: u32,
    // The length of the fixed text of the URL, without the values that each
    // request writes into it.
    url_fixed: u32,
    // The length of the fixed text of the body section.
    body_fixed: u32,
    // Where the head's field lines start, after the request line and
    // `host`: the step whose run they start in, and where in the run.
    fields: (u8, u32),
    // Whether the request writes a body of its own: fixed text or a hole in
    // the body section.
    body: bool,
    // Whether the body section holds a `Hole::Body` that each request fills.
    body_hole: bool,
}

/// One run of a request's layout.
///
/// An operation writes its layout once, generic over the pass. The
/// template's pass ([`Builder`]) writes the fixed text and leaves the holes.
/// A request's pass ([`EncodePass`]) copies that text from the template and
/// fills the holes. The compiler writes each operation's request pass as
/// straight-line code, so a request walks no table. What decides the holes,
/// the sections and the query parameters is the same in both passes: the
/// template's options. Text may come from anything the template is written
/// from, and a request's pass never reads it.
pub(crate) trait Pass {
    // Starts the next section. Sections are started in order.
    fn section(&mut self, section: Section);

    // Fixed text, which `write` writes into the template.
    fn text(&mut self, write: impl FnOnce(&mut Writer<'_>));

    // A hole that each request fills.
    fn hole(&mut self, hole: Hole);

    // Marks where the head's field lines start: after the request line and
    // `host`.
    fn fields(&mut self);

    // A query parameter. Each request writes the separator before a
    // parameter itself once a parameter that it may leave out has come.
    fn param(&mut self, name: &str, value: ParamValue<'_>);

    // Fixed text that is the same for every template.
    #[inline(always)]
    fn bytes(&mut self, bytes: &[u8]) {
        self.text(|out| out.push(bytes));
    }

    // The body section.
    #[inline(always)]
    fn body(&mut self, body: TemplateBody<'_>) {
        self.section(Section::Body);
        match body {
            TemplateBody::None => {}
            TemplateBody::Fixed(write) => self.text(|out| write(out)),
            TemplateBody::Each => self.hole(Hole::Body),
        }
    }
}

/// Writes a template: its fixed text, and the step lengths after it.
pub(crate) struct Builder<'t> {
    out: Writer<'t>,
    steps: [u32; MAX_STEPS],
    count: usize,
    section: Section,
    // Whether the last step takes more text: it is of this section and has
    // no hole yet.
    open: bool,
    // Where the text of the steps ends, and whether any of it was written
    // elsewhere, after text that no step holds.
    text_end: usize,
    scattered: bool,
    fields: (u8, u32),
    url_fixed: u32,
    body_fixed: u32,
    body_hole: bool,
    // The query parameters written as text so far, and whether a parameter
    // that a request may leave out has come.
    params: u8,
    optional: bool,
}

impl<'t> Builder<'t> {
    pub(crate) fn new(buf: &'t mut [u8]) -> Self {
        let mut out = Writer::new(buf);
        out.push(&[0; HEADER_LEN]);
        Self {
            out,
            steps: [0; MAX_STEPS],
            count: 0,
            section: Section::Origin,
            open: false,
            text_end: HEADER_LEN,
            scattered: false,
            fields: (u8::MAX, 0),
            url_fixed: 0,
            body_fixed: 0,
            body_hole: false,
            params: 0,
            optional: false,
        }
    }

    // Ends the sections from the current one to `until`. Each ends with a
    // step of text alone, empty if need be, so that every section has as
    // many steps as its layout has holes, and one: a request's pass knows
    // where each step is.
    fn end_sections(&mut self, until: usize) {
        for section in self.section as usize..until {
            if !(self.open && section == self.section as usize) {
                self.push(0);
            }
        }
    }

    // Writes text into the template that no section holds, for a request to
    // read where the returned span says, and returns that span. It comes
    // after every section's text.
    pub(crate) fn stash(&mut self, write: impl FnOnce(&mut Writer<'t>)) -> Span {
        self.out.part(write)
    }

    // The separator before a parameter: text until a parameter that a
    // request may leave out has come, and a hole from there on.
    fn separator(&mut self) {
        if self.optional {
            self.hole(Hole::Separator);
        } else {
            let separator: &[u8] = if self.params == 0 { b"?" } else { b"&" };
            self.text(|out| out.push(separator));
            self.params = self.params.saturating_add(1);
        }
    }

    fn push(&mut self, len: u32) {
        // A template past the table's bound is a fault of this crate's own
        // operations, which `finish` reports.
        if let Some(slot) = self.steps.get_mut(self.count) {
            *slot = len;
        }
        self.count += 1;
    }

    // Appends the step lengths, writes the header and trailing fingerprint,
    // and returns the total bytes written. A template that does not fit its
    // buffer is refused with the room it needs.
    pub(crate) fn finish(
        mut self,
        write_meta: impl FnOnce(&mut StoredWriter<'_>),
    ) -> Result<usize> {
        debug_assert!(self.count <= MAX_STEPS, "a template holds too many steps");
        debug_assert!(!self.scattered, "a template's text is written in order");
        if self.count > MAX_STEPS {
            return Err(InvalidRequest::RequestTooLarge.into());
        }
        self.end_sections(SECTIONS);
        if self.scattered {
            return Err(InvalidRequest::Template.into());
        }
        let start = self.out.position();
        for &len in &self.steps[..self.count] {
            self.out.push(&len.to_le_bytes());
        }
        self.out.push(&[0; TRAILER_LEN]);
        let end = self.out.position();
        if !self.out.fits() {
            return Err(capacity_error(CapacityError { required: end }));
        }
        let (Ok(start), Ok(end_u32)) = (u32::try_from(start), u32::try_from(end)) else {
            return Err(InvalidRequest::RequestTooLarge.into());
        };
        let table = Table {
            start,
            total: self.count as u8,
            params: self.params,
            end: end_u32,
            url_fixed: self.url_fixed,
            body_fixed: self.body_fixed,
            fields: self.fields,
            body: self.body_fixed > 0 || self.body_hole,
            body_hole: self.body_hole,
        };
        let written = self.out.into_written();
        let (meta_bytes, rest) = written.split_at_mut(META_LEN);
        write_meta(&mut StoredWriter::new(meta_bytes));
        let (table_bytes, _) = rest.split_at_mut(TABLE_HEADER_LEN);
        table.store(&mut StoredWriter::new(table_bytes));
        let payload_len = end - TRAILER_LEN;
        let hash = fingerprint(&written[..payload_len]);
        written[payload_len..end].copy_from_slice(&hash.to_le_bytes());
        Ok(end)
    }
}

// A hash of a template's bytes, its text and its table: not a check against
// tampering, but against bytes that are another template's or were changed.
pub(crate) fn fingerprint(bytes: &[u8]) -> u64 {
    let (words, rest) = bytes.as_chunks::<8>();
    let mut last = [0; 8];
    last[..rest.len()].copy_from_slice(rest);
    words.iter().chain([&last]).fold(
        0x243F_6A88_85A3_08D3 ^ bytes.len() as u64,
        |fingerprint, word| {
            (fingerprint ^ u64::from_le_bytes(*word))
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .rotate_left(23)
        },
    )
}

// Writes the header fields of a template one after another into its
// buffer.
pub(crate) struct StoredWriter<'s> {
    bytes: &'s mut [u8],
    at: usize,
}

impl<'s> StoredWriter<'s> {
    pub(crate) fn new(bytes: &'s mut [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub(crate) fn put(&mut self, part: &[u8]) {
        if let Some(slot) = self.bytes.get_mut(self.at..self.at + part.len()) {
            slot.copy_from_slice(part);
        }
        self.at += part.len();
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.put(&value.to_le_bytes());
    }
}

// Splits the fixed header at the start of a template buffer into its service
// metadata and table header, or `None` if the buffer is shorter than a header.
#[inline(always)]
pub(crate) fn split_header(bytes: &[u8]) -> Option<(&[u8; META_LEN], &[u8; TABLE_HEADER_LEN])> {
    let header = bytes.first_chunk::<HEADER_LEN>()?;
    let (meta, table) = header.split_at(META_LEN);
    Some((meta.try_into().ok()?, table.try_into().ok()?))
}

impl Table {
    // Where the fixed text and stashed spans end, just before the step table.
    pub(crate) const fn text_end(&self) -> usize {
        self.start as usize
    }

    fn store(&self, into: &mut StoredWriter<'_>) {
        let flags = u8::from(self.body) | (u8::from(self.body_hole) << 1);
        into.u32(self.start);
        into.u32(self.end);
        into.u32(self.url_fixed);
        into.u32(self.body_fixed);
        into.u32(self.fields.1);
        into.put(&[self.fields.0, self.total, self.params, flags]);
    }

    // Reads a table from the header slice of a template buffer, or `None`
    // if the bounds or flags are invalid for `bytes_len`.
    #[inline(always)]
    pub(crate) fn load(header: &[u8; TABLE_HEADER_LEN], bytes_len: usize) -> Option<Self> {
        let u32_at =
            |i: usize| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
        let start = u32_at(0);
        let end = u32_at(4);
        let url_fixed = u32_at(8);
        let body_fixed = u32_at(12);
        let fields_offset = u32_at(16);
        let [fields_step, total, params, flags] = [header[20], header[21], header[22], header[23]];
        if flags & !0b11 != 0 || usize::from(total) > MAX_STEPS || fields_step > total {
            return None;
        }
        let table_end = end.checked_sub(TRAILER_LEN as u32)?;
        if (start as usize) < HEADER_LEN
            || table_end.checked_sub(start) != Some(u32::from(total) * 4)
            || bytes_len < end as usize
        {
            return None;
        }
        Some(Self {
            start,
            total,
            params,
            end,
            url_fixed,
            body_fixed,
            fields: (fields_step, fields_offset),
            body: flags & 1 != 0,
            body_hole: flags & 2 != 0,
        })
    }
}

impl Pass for Builder<'_> {
    fn section(&mut self, section: Section) {
        debug_assert!(section >= self.section, "sections are written in order");
        self.end_sections(section as usize);
        self.section = section;
        self.open = false;
    }

    fn text(&mut self, write: impl FnOnce(&mut Writer<'_>)) {
        let span = self.out.part(write);
        if span.len == 0 {
            return;
        }
        self.scattered |= span.start != self.text_end;
        self.text_end = span.start + span.len;
        if (self.section as usize) < Section::Head as usize {
            self.url_fixed = self.url_fixed.saturating_add(span.len as u32);
        } else if self.section == Section::Body {
            self.body_fixed = self.body_fixed.saturating_add(span.len as u32);
        }
        // Text right after text of the same step extends it.
        if self.open {
            if let Some(last) = self.steps.get_mut(self.count - 1) {
                *last += span.len as u32;
            }
            return;
        }
        self.push(span.len as u32);
        self.open = true;
    }

    fn hole(&mut self, hole: Hole) {
        if self.section == Section::Body && hole == Hole::Body {
            self.body_hole = true;
        }
        if !self.open {
            self.push(0);
        }
        self.open = false;
    }

    fn fields(&mut self) {
        self.fields = match self.steps.get(self.count.wrapping_sub(1)) {
            Some(&last) if self.open => ((self.count - 1) as u8, last),
            _ => (self.count as u8, 0),
        };
    }

    fn param(&mut self, name: &str, value: ParamValue<'_>) {
        match value {
            ParamValue::Fixed(value) => {
                self.separator();
                self.text(|out| {
                    out.push(name.as_bytes());
                    out.push(b"=");
                    write_query_value(out, value);
                });
            }
            ParamValue::Hole(hole) => {
                self.separator();
                self.text(|out| {
                    out.push(name.as_bytes());
                    out.push(b"=");
                });
                self.hole(hole);
            }
            ParamValue::Optional(hole) => {
                self.optional = true;
                self.hole(hole);
            }
        }
    }
}

impl Table {
    // The length of the fixed text of the URL.
    pub(crate) const fn url_fixed(&self) -> usize {
        self.url_fixed as usize
    }

    /// The step lengths of the template that this table describes, or the
    /// refusal of bytes that cannot be its own.
    #[inline(always)]
    fn steps<'t>(&self, template: &'t [u8]) -> Result<&'t [[u8; 4]]> {
        let table_end = (self.end as usize)
            .checked_sub(TRAILER_LEN)
            .ok_or(InvalidRequest::Template)?;
        let table = template
            .get(self.start as usize..table_end)
            .ok_or(InvalidRequest::Template)?;
        let (steps, rest) = table.as_chunks::<4>();
        if !rest.is_empty() || steps.len() != self.total as usize {
            return Err(InvalidRequest::Template.into());
        }
        Ok(steps)
    }

    // The parts of `bytes`, or the refusal of bytes that cannot be the
    // template's.
    #[inline(always)]
    fn parts<'t>(&self, bytes: &'t [u8]) -> Result<TemplateParts<'t>> {
        let steps = self.steps(bytes)?;
        let text = bytes
            .get(HEADER_LEN..self.start as usize)
            .ok_or(InvalidRequest::Template)?;
        Ok(TemplateParts { steps, text })
    }

    // Checks the trailing fingerprint of `bytes` over its header, text and
    // step lengths. A request's pass does not run this check.
    pub(crate) fn check(&self, bytes: &[u8]) -> Result<()> {
        let end = self.end as usize;
        let payload_len = end
            .checked_sub(TRAILER_LEN)
            .ok_or(InvalidRequest::Template)?;
        let own = bytes.get(..end).ok_or(InvalidRequest::Template)?;
        let (payload, hash) = own.split_at(payload_len);
        let hash: [u8; 8] = hash.try_into().map_err(|_| InvalidRequest::Template)?;
        if fingerprint(payload) == u64::from_le_bytes(hash) {
            Ok(())
        } else {
            Err(InvalidRequest::Template.into())
        }
    }

    // The length of the body, from its fixed text and the length that `fill`
    // knows of what it writes. A body that `fill` does not know the length of
    // is counted by writing it into nothing.
    #[inline(always)]
    fn body_len<F: Fill>(&self, fill: &F) -> Result<usize> {
        match (self.body_hole, F::BODY) {
            (false, _) => Ok(self.body_fixed as usize),
            (true, false) => Err(InvalidRequest::Template.into()),
            (true, true) => {
                let filled = match fill.body_len() {
                    Some(len) => len,
                    None => {
                        let mut counted = Writer::new(&mut []);
                        if !fill.write(Hole::Body, &mut counted) {
                            return Err(InvalidRequest::Template.into());
                        }
                        counted.position()
                    }
                };
                Ok((self.body_fixed as usize).saturating_add(filled))
            }
        }
    }
}

/// A template's bytes, as its step lengths and its text.
#[derive(Debug, Clone, Copy)]
struct TemplateParts<'t> {
    steps: &'t [[u8; 4]],
    text: &'t [u8],
}

impl TemplateParts<'_> {
    const EMPTY: TemplateParts<'static> = TemplateParts {
        steps: &[],
        text: &[],
    };
}

/// The values that fill the holes of a request.
pub(crate) trait Fill {
    // Whether the fill writes a body, which a request then counts before
    // it writes its head.
    const BODY: bool = false;

    // The values of the holes that both services share.
    fn values(&self) -> &HoleValues<'_>;

    // Writes the value of a hole that `HoleValues` does not fill: a body that
    // this crate writes, or a value in the form that only one service
    // needs. Returns `false` for a hole that this request has no value for,
    // which means the template is for another operation.
    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        let _ = (hole, out);
        false
    }

    // Returns the length of the body that `Hole::Body` writes, if the fill
    // knows it without writing it.
    fn body_len(&self) -> Option<usize> {
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
    // The body that the request wrote, if its template has one.
    pub(crate) body: Option<Span>,
    // The placeholders for the SHA-256 and the checksum of the body: in the
    // head, and in the canonical request that S3 signs, whose last line
    // repeats the SHA-256.
    pub(crate) body_hash: [Option<Span>; 3],
    pub(crate) body_checksum: [Option<Span>; 2],
    pub(crate) end: usize,
}

// What a request has written so far that its holes and its parts need:
// where it starts and where each section ends, the placeholders it holds,
// where its fields start, and how many query parameters it has.
#[derive(Default)]
pub(crate) struct WriteState {
    start: usize,
    ends: [usize; SECTIONS],
    marks: Marks,
    fields: usize,
    params: usize,
    body_len: usize,
}

impl WriteState {
    #[inline(always)]
    fn new(table: &Table, start: usize, body_len: usize) -> Self {
        Self {
            start,
            ends: [start; SECTIONS],
            marks: Marks::default(),
            fields: 0,
            params: table.params as usize,
            body_len,
        }
    }

    // Where the request's parts are, once every section has ended. The ends
    // come from the template's bytes, so a span between two of them is
    // empty if they are out of order.
    #[inline(always)]
    fn written(self, table: &Table, end: usize) -> Written {
        let ends = self.ends;
        let span = |section: usize| {
            let from = if section == 0 {
                self.start
            } else {
                ends[section - 1]
            };
            Span {
                start: from,
                len: ends[section].saturating_sub(from),
            }
        };
        let head = span(Section::Head as usize);
        // The head ends with the empty line, which no field holds. Bytes
        // that are not the template's can put where the fields start past
        // it, so that span is kept within the head.
        let head_end = head.start + head.len;
        let fields = self.fields.clamp(head.start, head_end);
        Written {
            url: Span {
                start: self.start,
                len: ends[Section::Query as usize].saturating_sub(self.start),
            },
            head,
            fields: Span {
                start: fields,
                len: head_end.saturating_sub(2).saturating_sub(fields),
            },
            canonical: span(Section::Canonical as usize),
            signature: self.marks.signature,
            body: table.body.then(|| span(Section::Body as usize)),
            body_hash: self.marks.body_hash,
            body_checksum: self.marks.body_checksum,
            end,
        }
    }
}

// Writes the value of `hole`, whose step holds `name` if it is an optional
// parameter. Returns `false` for a hole that the request has no value for.
// A request's pass calls this with a hole it knows, which the compiler
// reduces to the one arm.
#[inline(always)]
fn write_hole(
    state: &mut WriteState,
    hole: Hole,
    name: &[u8],
    fill: &impl Fill,
    out: &mut Writer<'_>,
) -> bool {
    // The path and the query are written before any section that repeats
    // them.
    let ends = &state.ends;
    let (origin, path, query) = (
        ends[Section::Origin as usize],
        ends[Section::Path as usize],
        ends[Section::Query as usize],
    );
    let values = fill.values();
    match hole {
        Hole::Target => out.repeat(Span {
            start: origin,
            len: query.saturating_sub(origin),
        }),
        Hole::Path => out.repeat(Span {
            start: origin,
            len: path.saturating_sub(origin),
        }),
        // The query without its `?`.
        Hole::Query => {
            if query > path {
                out.repeat(Span {
                    start: path + 1,
                    len: query - path - 1,
                });
            }
        }
        Hole::Signature => {
            state.marks.signature = Some(out.part(|out| out.push(&[b'0'; 64])));
        }
        Hole::BodyLength => {
            out.push(U64Decimal::new(state.body_len as u64).as_bytes());
        }
        Hole::BodyHash => {
            let span = out.part(|out| out.push(&[b'0'; 64]));
            record_placeholder(&mut state.marks.body_hash, span);
        }
        Hole::BodyChecksum => {
            let Some(kind) = values.body_checksum else {
                return false;
            };
            let span = out.part(|out| {
                for _ in 0..kind.digest_len().div_ceil(3) * 4 {
                    out.push(b"=");
                }
            });
            record_placeholder(&mut state.marks.body_checksum, span);
        }
        Hole::Separator => {
            out.push(if state.params == 0 { b"?" } else { b"&" });
            state.params += 1;
        }
        Hole::Prefix | Hole::Marker | Hole::StartAfter | Hole::VersionMarker => {
            let Some(value) = values.param(hole) else {
                return false;
            };
            if let Some(value) = value {
                out.push(if state.params == 0 { b"?" } else { b"&" });
                state.params += 1;
                out.push(name);
                out.push(b"=");
                write_query(out, value);
            }
        }
        hole => return values.write(hole, out) || fill.write(hole, out),
    }
    true
}

/// Returns where the parts of a request written into `out` are, or the
/// refusal of one that did not fit, with the room it needs.
#[inline(always)]
pub(crate) fn check_fit(out: &Writer<'_>, written: Written) -> Result<Written> {
    if !out.fits() {
        return Err(capacity_error(CapacityError {
            required: written.end,
        }));
    }
    Ok(written)
}

/// A request's pass: it copies the template's text, run by run, and fills
/// each hole as the layout names it. Each hole copies the run before it,
/// and the end of each section copies the step of text alone that ends it.
/// So where each step is follows from the layout alone, and the compiler
/// writes each as straight-line code. Bytes that are not the template's
/// write a wrong request, or fail where they run out. They cannot make the
/// pass read outside them or panic.
pub(crate) struct EncodePass<'e, 'o, F: Fill> {
    steps: &'e [[u8; 4]],
    text: &'e [u8],
    table: &'e Table,
    fill: &'e F,
    out: &'e mut Writer<'o>,
    state: &'e mut WriteState,
    // The next step and where its run starts, and the current section.
    at: usize,
    cursor: usize,
    section: usize,
    // Whether a parameter that a request may leave out has come.
    optional: bool,
    failed: bool,
}

impl<'e, 'o, F: Fill> EncodePass<'e, 'o, F> {
    // Starts a request's pass over the template `template`, whose table is
    // `table`, which writes into `out` with the values that `fill` writes,
    // and keeps what the request's parts need in `state`. Bytes whose table
    // cannot be the template's fail the pass, which `finish` reports. The
    // pass itself holds only scalars and borrows, so the compiler keeps it
    // in registers.
    #[inline(always)]
    pub(crate) fn new(
        table: &'e Table,
        template: &'e [u8],
        fill: &'e F,
        out: &'e mut Writer<'o>,
        state: &'e mut WriteState,
    ) -> Self {
        let (parts, body_len, failed) = match table.parts(template) {
            Ok(parts) => match table.body_len(fill) {
                Ok(body_len) => (parts, body_len, false),
                Err(_) => (TemplateParts::EMPTY, 0, true),
            },
            Err(_) => (TemplateParts::EMPTY, 0, true),
        };
        *state = WriteState::new(table, out.position(), body_len);
        Self {
            steps: parts.steps,
            text: parts.text,
            table,
            fill,
            out,
            state,
            at: 0,
            cursor: 0,
            section: 0,
            optional: false,
            failed,
        }
    }

    // Copies the run of the next step.
    #[inline(always)]
    fn take(&mut self) {
        let at = self.at;
        self.at += 1;
        let Some(&step) = self.steps.get(at) else {
            self.failed = true;
            return;
        };
        let len = u32::from_le_bytes(step) as usize;
        match self.text.get(self.cursor..self.cursor.wrapping_add(len)) {
            // The step that ends a section often holds no text.
            Some(run) if !run.is_empty() => self.out.push(run),
            Some(_) => {}
            None => self.failed = true,
        }
        self.cursor = self.cursor.wrapping_add(len);
    }

    // Ends the sections before `section`, each with its step of text
    // alone.
    #[inline(always)]
    fn end_until(&mut self, section: usize) {
        while self.section < section {
            self.take();
            self.state.ends[self.section] = self.out.position();
            self.section += 1;
        }
    }

    // Ends every section, and returns where the request's parts are.
    #[inline(always)]
    pub(crate) fn finish(&mut self) -> Result<Written> {
        self.end_until(SECTIONS);
        if self.failed || self.at != self.steps.len() {
            return Err(InvalidRequest::Template.into());
        }
        let end = self.out.position();
        Ok(core::mem::take(self.state).written(self.table, end))
    }
}

impl<F: Fill> Pass for EncodePass<'_, '_, F> {
    #[inline(always)]
    fn section(&mut self, section: Section) {
        self.end_until(section as usize);
    }

    #[inline(always)]
    fn text(&mut self, _write: impl FnOnce(&mut Writer<'_>)) {}

    #[inline(always)]
    fn hole(&mut self, hole: Hole) {
        self.take();
        self.failed |= !write_hole(self.state, hole, &[], self.fill, self.out);
    }

    // The fields start in the run of the next step, which starts here.
    #[inline(always)]
    fn fields(&mut self) {
        let (step, offset) = self.table.fields;
        self.failed |= step as usize != self.at;
        let len = self
            .steps
            .get(self.at)
            .map_or(0, |&step| u32::from_le_bytes(step));
        self.state.fields = self.out.position() + len.min(offset) as usize;
    }

    #[inline(always)]
    fn param(&mut self, name: &str, value: ParamValue<'_>) {
        match value {
            ParamValue::Fixed(_) if self.optional => self.hole(Hole::Separator),
            ParamValue::Fixed(_) => {}
            ParamValue::Hole(hole) => {
                if self.optional {
                    self.hole(Hole::Separator);
                }
                self.hole(hole);
            }
            ParamValue::Optional(hole) => {
                self.optional = true;
                self.take();
                self.failed |= !write_hole(self.state, hole, name.as_bytes(), self.fill, self.out);
            }
        }
    }
}

// The placeholders that a request holds, for the caller to fill once it is
// written.
#[derive(Default)]
struct Marks {
    signature: Option<Span>,
    body_hash: [Option<Span>; 3],
    body_checksum: [Option<Span>; 2],
}

// Records the span of a placeholder in the first free slot: the head's,
// then the canonical request's.
#[inline(always)]
fn record_placeholder(slots: &mut [Option<Span>], span: Span) {
    if let Some(slot) = slots.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(span);
    }
}

/// One request: where its parts are in the buffer it was written into.
///
/// Every function that writes a request returns one. It borrows nothing, so
/// you can keep it while you borrow the buffer again. You can grow a buffer
/// that was too small and write the request again, or send the request
/// later. Pass the buffer to each method that reads a part:
///
/// ```
/// use borink_object_storage_proto::azure::{self, AzureNamespace, Container};
/// use borink_object_storage_proto::{
///     ContentLength, Error, PutItem, PutOptions, Result, Timestamps, WireRequest,
/// };
///
/// fn write(
///     buf: &mut Vec<u8>,
///     encode: impl Fn(&mut [u8]) -> Result<WireRequest>,
/// ) -> Result<WireRequest> {
///     match encode(buf) {
///         Err(Error::Capacity(capacity)) => {
///             buf.resize(capacity.required, 0);
///             encode(buf)
///         }
///         request => request,
///     }
/// }
///
/// # fn main() -> Result<()> {
/// let container = Container::new("https://account.blob.core.windows.net", "container")?;
/// let options = PutOptions::default();
/// let namespace = AzureNamespace::Unknown;
/// let mut template_buf = vec![0; 1024];
/// let len = azure::prepare_put(&container, "token", namespace, &options, &mut template_buf)?;
/// let template = azure::Template::from_bytes(&template_buf[..len]);
/// let content = b"hello";
/// let item = PutItem::new("greeting.txt", ContentLength::of(content));
/// let now = Timestamps::from_unix(1_787_400_000);
/// let mut buf = Vec::new();
/// let request = write(&mut buf, |buf| azure::encode_put(template, &item, &now, buf))?;
/// assert!(request.head(&buf).starts_with(b"PUT /container/greeting.txt HTTP/1.1\r\n"));
/// assert_eq!(request.content_length(), 5);
/// assert_eq!(request.body(&buf), None);
/// # Ok(())
/// # }
/// ```
///
/// A buffer that you keep between requests stops growing once it fits the
/// longest, and from then on each request is written once.
///
/// Send the request with any HTTP client. A client that takes a URL and a
/// list of header fields reads [`Self::url`] and [`Self::headers`], and
/// writes `host` from the URL itself. A client that writes HTTP/1.1 itself
/// sends [`Self::head`], which holds the request line and `host` too, and
/// then the content.
///
/// Each method that reads a part takes the buffer the request was written
/// into. Another buffer gives a wrong request, but never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireRequest {
    method: Method,
    url: Span,
    head: Span,
    fields: Span,
    body: Option<Span>,
    content_len: u64,
}

impl WireRequest {
    // The request of `method` written as `written`, which sends
    // `content_len` bytes of content.
    pub(crate) fn new(method: Method, written: &Written, content_len: u64) -> Self {
        Self {
            method,
            url: written.url,
            head: written.head,
            fields: written.fields,
            body: written.body,
            content_len,
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
    pub fn url<'b>(&self, buf: &'b [u8]) -> &'b str {
        written_text(span_bytes(buf, self.url))
    }

    /// Returns each header field as a name and a value, other than `host`.
    ///
    /// Your HTTP client writes `host` from the URL. Write every field that
    /// this returns, unchanged and none twice.
    pub fn headers<'b>(
        &self,
        buf: &'b [u8],
    ) -> impl Iterator<Item = (&'b str, &'b str)> + Clone + use<'b> {
        FieldLines(span_bytes(buf, self.fields))
    }

    /// Returns the HTTP/1.1 request head: the request line, `host`, every
    /// field line and the empty line that ends the head.
    ///
    /// Write these bytes to a connection to the URL's host, then the
    /// content, if any.
    pub fn head<'b>(&self, buf: &'b [u8]) -> &'b [u8] {
        span_bytes(buf, self.head)
    }

    /// Returns the body that this crate wrote into `buf`, such as a list of
    /// parts or a set of tags.
    ///
    /// Returns [`None`] for a request with no body, and for a write whose
    /// content you send yourself. Read [`Self::content_length`] for the length
    /// of that content.
    pub fn body<'b>(&self, buf: &'b [u8]) -> Option<&'b [u8]> {
        self.body.map(|body| span_bytes(buf, body))
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

    /// Returns the length of the content: of the body that this crate
    /// wrote, or of the content that you send.
    pub fn content_length(&self) -> u64 {
        match self.body {
            Some(body) => body.len as u64,
            None => self.content_len,
        }
    }
}

// The bytes of `span` in `buf`. A request read from a buffer that is not
// its own may have spans past that buffer's end, which hold nothing.
fn span_bytes(buf: &[u8], span: Span) -> &[u8] {
    buf.get(span.start..span.start + span.len)
        .unwrap_or_default()
}

// The field lines of a head, each read as its name and its value.
#[derive(Clone)]
struct FieldLines<'r>(&'r [u8]);

impl<'r> Iterator for FieldLines<'r> {
    type Item = (&'r str, &'r str);

    fn next(&mut self) -> Option<Self::Item> {
        let end = self.0.windows(2).position(|pair| pair == b"\r\n")?;
        let (line, rest) = self.0.split_at(end);
        self.0 = &rest[2..];
        // A name holds no `:`, so the first `: ` ends it.
        let colon = line.windows(2).position(|pair| pair == b": ")?;
        Some((
            written_text(&line[..colon]),
            written_text(&line[colon + 2..]),
        ))
    }
}

/// The values of one request that fill the holes both services share.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HoleValues<'i> {
    pub(crate) now: Option<&'i Timestamps>,
    pub(crate) key: Option<&'i str>,
    pub(crate) range: Option<RequestedRange>,
    pub(crate) condition: Option<ConditionValue<'i>>,
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
    pub(crate) source_condition: Option<ConditionValue<'i>>,
    pub(crate) source_range: Option<RequestedRange>,
    pub(crate) copy_id: Option<&'i str>,
    // The kind of checksum of a body that this crate writes, whose
    // placeholder the request holds, and the length of an S3 object.
    pub(crate) body_checksum: Option<ChecksumKind>,
    pub(crate) object_size: Option<u64>,
}

impl Fill for HoleValues<'_> {
    fn values(&self) -> &HoleValues<'_> {
        self
    }
}

impl HoleValues<'_> {
    // Writes the value of `hole`. Returns `false` for a hole that is not
    // one of these values, or that this request has no value for.
    #[inline(always)]
    fn write(&self, hole: Hole, out: &mut Writer<'_>) -> bool {
        use crate::request::HeaderValue;
        match hole {
            Hole::Key => match self.key {
                Some(key) => write_key(out, key),
                None => return false,
            },
            Hole::Range => match self.range {
                Some(range) => write_range(out, range),
                None => return false,
            },
            Hole::Condition => match self.condition {
                Some(value) => value.write_to(out),
                None => return false,
            },
            Hole::Revision => match self.revision {
                Some(revision) => write_query(out, revision.as_bytes()),
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
                Some(len) => out.push(U64Decimal::new(len).as_bytes()),
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
                Some(text) => write_query(out, text.as_bytes()),
                None => return false,
            },
            Hole::SourceKey => match self.source_key {
                Some(key) => write_key(out, key),
                None => return false,
            },
            Hole::SourceRevision | Hole::CopyId => match self.source_revision.or(self.copy_id) {
                Some(text) => write_query(out, text.as_bytes()),
                None => return false,
            },
            Hole::SourceCondition => match self.source_condition {
                Some(value) => value.write_to(out),
                None => return false,
            },
            Hole::SourceRange => match self.source_range {
                Some(range) => write_range(out, range),
                None => return false,
            },
            Hole::ObjectSize => match self.object_size {
                Some(size) => out.push(U64Decimal::new(size).as_bytes()),
                None => return false,
            },
            Hole::PartNumber => match self.part_number {
                Some(number) => out.push(U64Decimal::new(number.into()).as_bytes()),
                None => return false,
            },
            _ => return false,
        }
        true
    }

    // Returns at least the bytes that the request's values take in its URL,
    // so that a URL that is surely short enough needs no counting.
    pub(crate) fn url_bound(&self) -> usize {
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

    // Returns the value of the optional parameter `hole`, as text to
    // percent-encode: `Some(None)` for a request that leaves it out, and
    // `None` for a hole that this request has no value for.
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

// Writes `value` percent-encoded as a query value.
#[inline(always)]
fn write_query(out: &mut Writer<'_>, value: &[u8]) {
    for part in encode_query_value(value) {
        out.push(part);
    }
}

// Writes `key` percent-encoded as a path: whole, if it holds no byte to
// escape.
#[inline(always)]
fn write_key(out: &mut Writer<'_>, key: &str) {
    if key_is_plain(key.as_bytes()) {
        out.push(key.as_bytes());
    } else {
        for part in encode_object_key(key) {
            out.push(part);
        }
    }
}
