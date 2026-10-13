// Requests written from a template, for both services.
//
// A template is the part of a request that is the same for every object it
// serves: the origin, the path up to the key, the fields whose values never
// change, and the fixed text of the canonical request that S3 signs. It is
// written once into a buffer the caller owns, followed by a table of steps.
// A step is a run of that fixed text and the hole after it, which each
// request fills with a value of its own, such as its key or the time. The
// runs follow each other in the text, so a step holds only their lengths.
//
// Encoding a request walks the table: it copies each run of fixed text and
// fills each hole after it. The request is written whole into its own buffer, so it
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

impl Hole {
    const ALL: [Self; 34] = [
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
    fn from_id(id: u8) -> Option<Self> {
        Self::ALL.get((id as usize).wrapping_sub(1)).copied()
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
    Fixed(Text<'b>),
    Each,
}

// Writes text that every request sends alike, such as a fixed body.
pub(crate) type Text<'t> = &'t dyn Fn(&mut dyn crate::request::ByteSink);

// The text of a fixed body, for a request's describe: a request's pass
// writes no text.
pub(crate) const NO_TEXT: Text<'static> = &|_| {};

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

// A run of fixed text, which starts where the previous step's ended, and
// the hole after it: `hole` is the number of a `Hole`, or `NO_HOLE`. The
// name of an optional parameter follows the run in the text, `name` bytes
// long, and is written only with a value. `ends` has a bit for each
// section that ends after the step.
#[derive(Debug, Clone, Copy, Default)]
struct Step {
    len: u32,
    hole: u8,
    name: u8,
    ends: u8,
}

pub(crate) const NO_HOLE: u8 = 0;

/// Where a template's table of steps is in its bytes, and how many steps
/// each section has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Table {
    start: u32,
    counts: [u8; SECTIONS],
    // The steps of all sections.
    total: u8,
    // The query parameters that the template writes as text, before the
    // first that a request may leave out. A request counts on from here.
    params: u8,
    // The bytes the template takes, the table included.
    end: u32,
    // The length of the fixed text of the URL, without the values that each
    // request writes into it.
    url_fixed: u32,
    // Where the head's field lines start, after the request line and
    // `host`: the step whose run they start in, and where in the run.
    fields: (u8, u32),
    // Whether the request writes a body of its own: text or a hole in the
    // body section, beside the step of text alone that ends it.
    body: bool,
    // A hash of the template's bytes, by which it knows them again.
    seal: u64,
}

/// One run of a request's layout.
///
/// An operation writes its layout once, generic over the pass. The
/// template's pass ([`Builder`]) writes the fixed text and leaves the holes.
/// A request's pass ([`Encode`]) copies that text from the template and
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
    fn body(&mut self, body: Body<'_>) {
        self.section(Section::Body);
        match body {
            Body::None => {}
            Body::Fixed(write) => self.text(|out| write(out)),
            Body::Each => self.hole(Hole::Body),
        }
    }
}

/// Writes a template: its fixed text, and the table of steps after it.
pub(crate) struct Builder<'t> {
    out: Writer<'t>,
    steps: [Step; MAX_STEPS],
    count: usize,
    counts: [u8; SECTIONS],
    section: Section,
    // Whether the last step takes more text: it is of this section, has no
    // hole yet, and is no optional parameter's name.
    open: bool,
    // Where the text of the steps ends, and whether any of it was written
    // elsewhere, after text that no step holds.
    text_end: usize,
    scattered: bool,
    fields: (u8, u32),
    // The query parameters written as text so far, and whether a parameter
    // that a request may leave out has come.
    params: u8,
    optional: bool,
}

impl<'t> Builder<'t> {
    pub(crate) fn new(buf: &'t mut [u8]) -> Self {
        Self {
            out: Writer::new(buf),
            steps: [Step::default(); MAX_STEPS],
            count: 0,
            counts: [0; SECTIONS],
            section: Section::Origin,
            open: false,
            text_end: 0,
            scattered: false,
            fields: (u8::MAX, 0),
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
                self.push_in(section, Step::default());
            }
            if let Some(last) = self.steps.get_mut(self.count - 1) {
                last.ends |= 1 << section;
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

    fn push(&mut self, step: Step) {
        self.push_in(self.section as usize, step);
    }

    // Adds `step` to the section `section`.
    fn push_in(&mut self, section: usize, step: Step) {
        // A template past the table's bound is a fault of this crate's own
        // operations, which `finish` reports.
        if let Some(slot) = self.steps.get_mut(self.count) {
            *slot = step;
        }
        self.count += 1;
        let count = &mut self.counts[section];
        *count = count.saturating_add(1);
    }

    // Appends the table after the text, and returns where it is. A template
    // that does not fit its buffer is refused with the room it needs.
    pub(crate) fn finish(mut self) -> Result<Table> {
        debug_assert!(self.count <= MAX_STEPS, "a template holds too many steps");
        debug_assert!(!self.scattered, "a template's text is written in order");
        if self.count > MAX_STEPS {
            return Err(InvalidPlan::RequestTooLarge.into());
        }
        self.end_sections(SECTIONS);
        if self.scattered {
            return Err(InvalidPlan::Template.into());
        }
        let start = self.out.position();
        for step in &self.steps[..self.count] {
            self.out.push(&step.len.to_le_bytes());
            self.out.push(&[step.hole, step.name, step.ends, 0]);
        }
        let end = self.out.position();
        if !self.out.fits() {
            return Err(capacity_error(CapacityError {
                required: end,
                required_headers: 0,
            }));
        }
        let seal = seal(self.out.written());
        let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(end)) else {
            return Err(InvalidPlan::RequestTooLarge.into());
        };
        // The URL's sections come first, so their steps do too.
        let url_steps: usize = self.counts[..Section::Head as usize]
            .iter()
            .map(|&count| count as usize)
            .sum();
        let url_fixed = self.steps[..url_steps]
            .iter()
            .map(|step| step.len + u32::from(step.name))
            .sum();
        let body = self.steps
            [self.count - self.counts[Section::Body as usize] as usize..self.count]
            .iter()
            .any(|step| step.len > 0 || step.hole != NO_HOLE);
        Ok(Table {
            start,
            counts: self.counts,
            total: self.count as u8,
            params: self.params,
            end,
            url_fixed,
            fields: self.fields,
            body,
            seal,
        })
    }
}

// A hash of a template's bytes, its text and its table: not a check against
// tampering, but against bytes that are another template's or were changed.
pub(crate) fn seal(bytes: &[u8]) -> u64 {
    let (words, rest) = bytes.as_chunks::<8>();
    let mut last = [0; 8];
    last[..rest.len()].copy_from_slice(rest);
    words
        .iter()
        .chain([&last])
        .fold(0x243F_6A88_85A3_08D3 ^ bytes.len() as u64, |seal, word| {
            (seal ^ u64::from_le_bytes(*word))
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .rotate_left(23)
        })
}

// Writes the values of a stored template one after another, as
// `Template::to_bytes` of each service stores them.
pub(crate) struct Store<'s> {
    bytes: &'s mut [u8],
    at: usize,
}

impl<'s> Store<'s> {
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

// Reads the values of a stored template back, in the order `Store` wrote
// them. Past the end, it reads zeros. Every read is inlined, so that a
// template read back reads each value at an offset the compiler knows.
pub(crate) struct Load<'s> {
    bytes: &'s [u8],
    at: usize,
}

impl<'s> Load<'s> {
    #[inline(always)]
    pub(crate) fn new(bytes: &'s [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    #[inline(always)]
    pub(crate) fn take<const N: usize>(&mut self) -> [u8; N] {
        let part = self
            .bytes
            .get(self.at..self.at + N)
            .and_then(|part| part.try_into().ok())
            .unwrap_or([0; N]);
        self.at += N;
        part
    }

    #[inline(always)]
    pub(crate) fn u8(&mut self) -> u8 {
        let [byte] = self.take();
        byte
    }

    #[inline(always)]
    pub(crate) fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
}

impl Table {
    // The bytes that `store` writes.
    pub(crate) const STORED_LEN: usize = 34;

    // The length of the template's bytes, the table included.
    pub(crate) const fn len(&self) -> usize {
        self.end as usize
    }

    pub(crate) fn store(&self, into: &mut Store<'_>) {
        into.u32(self.start);
        into.put(&self.counts);
        into.put(&[self.total, self.params]);
        into.u32(self.end);
        into.u32(self.url_fixed);
        into.put(&[self.fields.0]);
        into.u32(self.fields.1);
        into.put(&[u8::from(self.body)]);
        into.put(&self.seal.to_le_bytes());
    }

    // Reads a table that `store` wrote, or `None` for one that no template
    // has: its steps are those of its sections, and fill its bytes from
    // `start` to `end`.
    #[inline(always)]
    pub(crate) fn load(from: &mut Load<'_>) -> Option<Self> {
        let start = from.u32();
        let counts: [u8; SECTIONS] = from.take();
        let [total, params] = from.take();
        let end = from.u32();
        let url_fixed = from.u32();
        let fields = (from.u8(), from.u32());
        let body = match from.u8() {
            0 => false,
            1 => true,
            _ => return None,
        };
        let seal = u64::from_le_bytes(from.take());
        let steps: usize = counts.iter().map(|&count| usize::from(count)).sum();
        if steps != usize::from(total)
            || steps > MAX_STEPS
            || end.checked_sub(start) != Some(u32::from(total) * 8)
            || fields.0 > total
        {
            return None;
        }
        Some(Self {
            start,
            counts,
            total,
            params,
            end,
            url_fixed,
            fields,
            body,
            seal,
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
        // Text right after text of the same step extends it.
        if self.open {
            if let Some(last) = self.steps.get_mut(self.count - 1) {
                last.len += span.len as u32;
            }
            return;
        }
        self.push(Step {
            len: span.len as u32,
            ..Step::default()
        });
        self.open = true;
    }

    fn hole(&mut self, hole: Hole) {
        match self.steps.get_mut(self.count.wrapping_sub(1)) {
            Some(last) if self.open => last.hole = hole as u8,
            _ => self.push(Step {
                len: 0,
                hole: hole as u8,
                ..Step::default()
            }),
        }
        self.open = false;
    }

    fn fields(&mut self) {
        self.fields = match self.steps.get(self.count.wrapping_sub(1)) {
            Some(last) if self.open => ((self.count - 1) as u8, last.len),
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
                    crate::url::write_query_value(out, value);
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
            // The name follows the run of the hole's step, and a request
            // writes it only with a value.
            ParamValue::Optional(hole) => {
                self.optional = true;
                self.hole(hole);
                let span = self.out.part(|out| out.push(name.as_bytes()));
                self.scattered |= span.start != self.text_end || span.len > 255;
                self.text_end = span.start + span.len;
                if let Some(last) = self.steps.get_mut(self.count - 1) {
                    last.name = span.len as u8;
                }
            }
        }
    }
}

impl Table {
    // The length of the fixed text of the URL.
    pub(crate) const fn url_fixed(&self) -> usize {
        self.url_fixed as usize
    }

    /// The steps of the template that this table describes, or the refusal
    /// of bytes that cannot be its own.
    fn steps<'t>(&self, template: &'t [u8]) -> Result<&'t [[u8; 8]]> {
        let table = template
            .get(self.start as usize..self.end as usize)
            .ok_or(InvalidPlan::Template)?;
        let (steps, rest) = table.as_chunks::<8>();
        if !rest.is_empty() || steps.len() != self.total as usize {
            return Err(InvalidPlan::Template.into());
        }
        Ok(steps)
    }

    // The parts of `bytes`, or the refusal of bytes that cannot be the
    // template's.
    fn parts<'t>(&self, bytes: &'t [u8]) -> Result<Parts<'t>> {
        let steps = self.steps(bytes)?;
        let text = bytes
            .get(..self.start as usize)
            .ok_or(InvalidPlan::Template)?;
        Ok(Parts { bytes, steps, text })
    }

    // Checks that `bytes` are the template's own: its text and its table
    // are the ones it sealed. A request's pass checks neither.
    pub(crate) fn check(&self, bytes: &[u8]) -> Result<()> {
        match bytes.get(..self.end as usize) {
            Some(own) if seal(own) == self.seal => Ok(()),
            _ => Err(InvalidPlan::Template.into()),
        }
    }

    // The steps of the sections before `section`.
    fn steps_before(&self, section: usize) -> usize {
        self.counts[..section]
            .iter()
            .map(|&count| count as usize)
            .sum()
    }

    // The length of the body, from its fixed text and the length that
    // `fill` knows of what it writes. A body that `fill` does not know the
    // length of is counted by writing it into nothing. A fill that writes
    // no body is not handed on, so that it stays where the compiler put it.
    #[inline(always)]
    fn body_len<F: Fill>(&self, parts: Parts<'_>, fill: &F) -> Result<usize> {
        match (self.body, F::BODY) {
            (false, _) => Ok(0),
            (true, false) => self.fixed_body_len(parts.steps),
            (true, true) => self.filled_body_len(parts, fill),
        }
    }

    // The length of a body that is the template's text alone.
    fn fixed_body_len(&self, steps: &[[u8; 8]]) -> Result<usize> {
        let mut len = 0;
        for at in self.steps_before(Section::Body as usize)..steps.len() {
            let step = step(steps, at)?;
            if step.hole != NO_HOLE {
                return Err(InvalidPlan::Template.into());
            }
            len += step.len as usize;
        }
        Ok(len)
    }

    // The length of a body that `fill` writes into the template's text.
    fn filled_body_len(&self, parts: Parts<'_>, fill: &impl Fill) -> Result<usize> {
        let body = Section::Body as usize;
        let steps = parts.steps;
        let mut len = 0;
        for at in self.steps_before(body)..steps.len() {
            let step = step(steps, at)?;
            len += step.len as usize + step.name as usize;
            len += match step.hole {
                NO_HOLE => 0,
                hole if hole == Hole::Body as u8 => match fill.body_len() {
                    Some(len) => len,
                    None => {
                        let mut counted = Writer::new(&mut []);
                        walk(self, parts.bytes, fill, &mut counted, body..SECTIONS, 0)?;
                        return Ok(counted.position());
                    }
                },
                _ => return Err(InvalidPlan::Template.into()),
            };
        }
        Ok(len)
    }
}

// The step at `at` of `steps`.
fn step(steps: &[[u8; 8]], at: usize) -> Result<Step> {
    let [l0, l1, l2, l3, hole, name, ends, _] = *steps.get(at).ok_or(InvalidPlan::Template)?;
    Ok(Step {
        len: u32::from_le_bytes([l0, l1, l2, l3]),
        hole,
        name,
        ends,
    })
}

/// A template's bytes, and the table and the text in them.
#[derive(Debug, Clone, Copy)]
struct Parts<'t> {
    bytes: &'t [u8],
    steps: &'t [[u8; 8]],
    text: &'t [u8],
}

impl Parts<'_> {
    const EMPTY: Parts<'static> = Parts {
        bytes: &[],
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
    fn values(&self) -> &Values<'_>;

    // Writes the value of a hole that `Values` does not fill: a body that
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
pub(crate) struct State {
    start: usize,
    ends: [usize; SECTIONS],
    marks: Marks,
    fields: usize,
    params: usize,
    body_len: usize,
}

impl State {
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

    // Records that the sections whose bits `ends` has end at `position`.
    #[inline(always)]
    fn end(&mut self, ends: u8, position: usize) {
        for (section, end) in self.ends.iter_mut().enumerate() {
            if ends & 1 << section != 0 {
                *end = position;
            }
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
    state: &mut State,
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
            out.push(crate::request::U64Decimal::new(state.body_len as u64).as_bytes());
        }
        Hole::BodyHash => {
            let span = out.part(|out| out.push(&[b'0'; 64]));
            record(&mut state.marks.body_hash, span);
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
            record(&mut state.marks.body_checksum, span);
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

/// Writes a request by walking its template's table, one function for
/// every operation. With the feature `compact`, an encoder calls this in
/// place of its layout.
#[cfg(feature = "compact")]
pub(crate) fn walk_request(
    table: &Table,
    template: &[u8],
    fill: &impl Fill,
    out: &mut Writer<'_>,
) -> Result<Written> {
    let body_len = table.body_len(table.parts(template)?, fill)?;
    walk(table, template, fill, out, 0..SECTIONS, body_len)
}

/// Returns where the parts of a request written into `out` are, or the
/// refusal of one that did not fit, with the room it needs.
#[inline(always)]
pub(crate) fn fitted(
    table: &Table,
    template: &[u8],
    fill: &impl Fill,
    out: &Writer<'_>,
    written: Written,
) -> Result<Written> {
    if !out.fits() {
        return Err(capacity_error(CapacityError {
            required: written.end,
            required_headers: 0,
        }));
    }
    #[cfg(test)]
    same_as_walked(table, template, fill, out, &written);
    #[cfg(not(test))]
    let _ = (table, template, fill);
    Ok(written)
}

// In tests, checks that walking the table writes what the layout wrote, for
// bytes that are the template's own: from other bytes, both write a wrong
// request, and not the same one.
#[cfg(test)]
fn same_as_walked(
    table: &Table,
    template: &[u8],
    fill: &impl Fill,
    out: &Writer<'_>,
    written: &Written,
) {
    extern crate std;
    if table.check(template).is_err() {
        return;
    }
    let mut walked = std::vec![0u8; written.end];
    let mut copy = Writer::new(&mut walked);
    let body_len = table
        .body_len(table.parts(template).expect("the template's own"), fill)
        .expect("the body counts");
    let again = walk(table, template, fill, &mut copy, 0..SECTIONS, body_len)
        .expect("walking the table writes the request");
    assert_eq!(
        &walked[..again.end],
        out.written(),
        "the layout and the table write alike"
    );
    assert_eq!(
        (
            again.url,
            again.head,
            again.fields,
            again.canonical,
            again.body
        ),
        (
            written.url,
            written.head,
            written.fields,
            written.canonical,
            written.body
        ),
        "the layout and the table place the parts alike",
    );
}

/// Returns the length of the URL that a request from the template would
/// have, without writing it.
pub(crate) fn url_len(table: &Table, template: &[u8], fill: &impl Fill) -> Result<usize> {
    let mut counted = Writer::new(&mut []);
    walk(
        table,
        template,
        fill,
        &mut counted,
        0..Section::Head as usize,
        0,
    )?;
    Ok(counted.position())
}

// Writes the sections `sections` of a request by walking the template's
// table: it copies each run of text and fills each hole after it.
fn walk(
    table: &Table,
    template: &[u8],
    fill: &impl Fill,
    out: &mut Writer<'_>,
    sections: core::ops::Range<usize>,
    body_len: usize,
) -> Result<Written> {
    let steps = table.steps(template)?;
    // The text of the steps, before the table, and a run of it.
    let text = &template[..table.start as usize];
    let run = |cursor: usize, len: usize| {
        text.get(cursor..cursor.wrapping_add(len))
            .ok_or(InvalidPlan::Template)
    };
    // The steps of the sections before the range are skipped, and their
    // text with them.
    let first = table.steps_before(sections.start);
    let last = table.steps_before(sections.end);
    let mut cursor = 0;
    for at in 0..first {
        let step = step(steps, at)?;
        cursor += step.len as usize + step.name as usize;
    }
    let mut state = State::new(table, out.position(), body_len);
    let (fields_step, fields_offset) = table.fields;
    for at in first..last {
        let step = step(steps, at)?;
        let fixed = run(cursor, step.len as usize)?;
        if at == fields_step as usize {
            state.fields = out.position() + fixed.len().min(fields_offset as usize);
        }
        out.push(fixed);
        cursor += step.len as usize;
        if step.hole != NO_HOLE {
            let name = run(cursor, step.name as usize)?;
            let hole = Hole::from_id(step.hole).ok_or(InvalidPlan::Template)?;
            if !write_hole(&mut state, hole, name, fill, out) {
                return Err(InvalidPlan::Template.into());
            }
        }
        cursor += step.name as usize;
        if step.ends != 0 {
            state.end(step.ends, out.position());
        }
    }
    // A section after the range ends where the request does.
    let end = out.position();
    for later in &mut state.ends[sections.end..] {
        *later = end;
    }
    Ok(state.written(table, end))
}

/// A request's pass: it copies the template's text, run by run, and fills
/// each hole as the layout names it. Each hole copies the run before it,
/// and the end of each section copies the step of text alone that ends it.
/// So where each step is follows from the layout alone, and the compiler
/// writes each as straight-line code. The pass does not check that each
/// step holds the hole that the layout names. Bytes that are not the
/// template's write a wrong request, or fail where they run out. They cannot
/// make the pass read outside them or panic.
#[cfg_attr(feature = "compact", allow(dead_code))]
pub(crate) struct Encode<'e, 'o, F: Fill> {
    steps: &'e [[u8; 8]],
    text: &'e [u8],
    table: &'e Table,
    fill: &'e F,
    out: &'e mut Writer<'o>,
    state: &'e mut State,
    // The next step and where its run starts, and the current section.
    at: usize,
    cursor: usize,
    section: usize,
    // Whether a parameter that a request may leave out has come.
    optional: bool,
    failed: bool,
}

#[cfg_attr(feature = "compact", allow(dead_code))]
impl<'e, 'o, F: Fill> Encode<'e, 'o, F> {
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
        state: &'e mut State,
    ) -> Self {
        let (parts, body_len, failed) = match table.parts(template) {
            Ok(parts) => match table.body_len(parts, fill) {
                Ok(body_len) => (parts, body_len, false),
                Err(_) => (Parts::EMPTY, 0, true),
            },
            Err(_) => (Parts::EMPTY, 0, true),
        };
        *state = State::new(table, out.position(), body_len);
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

    // Copies the run of the next step, and returns the length of the name
    // it holds.
    #[inline(always)]
    fn take(&mut self) -> usize {
        // The step, read whole: its run's length, its hole, its name. The
        // pass moves on whatever the step holds, so that where it is stays
        // what the layout makes it.
        let at = self.at;
        self.at += 1;
        let Some(&step) = self.steps.get(at) else {
            self.failed = true;
            return 0;
        };
        let step = u64::from_le_bytes(step);
        let len = step as u32 as usize;
        match self.text.get(self.cursor..self.cursor.wrapping_add(len)) {
            // The step that ends a section often holds no text.
            Some(run) if !run.is_empty() => self.out.push(run),
            Some(_) => {}
            None => self.failed = true,
        }
        self.cursor = self.cursor.wrapping_add(len);
        (step >> 40) as u8 as usize
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
            return Err(InvalidPlan::Template.into());
        }
        let end = self.out.position();
        Ok(core::mem::take(self.state).written(self.table, end))
    }
}

impl<F: Fill> Pass for Encode<'_, '_, F> {
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
        let len = self.steps.get(self.at).map_or(0, |&[l0, l1, l2, l3, ..]| {
            u32::from_le_bytes([l0, l1, l2, l3])
        });
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
                let name_len = self.take();
                self.failed |= !write_hole(self.state, hole, name.as_bytes(), self.fill, self.out);
                self.cursor = self.cursor.wrapping_add(name_len);
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
            Some(body) => Payload::Slice(
                bytes
                    .get(body.start..body.start + body.len)
                    .unwrap_or_default(),
            ),
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

    // The bytes of `span`. A request finished in a buffer that is not its
    // own may have spans past that buffer's end, which hold nothing.
    fn slice(&self, span: Span) -> &'r [u8] {
        self.bytes
            .get(span.start..span.start + span.len)
            .unwrap_or_default()
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
    fn values(&self) -> &Values<'_> {
        self
    }
}

impl Values<'_> {
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
                Some(range) => crate::common::write_range(out, range),
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
                Some(range) => crate::common::write_range(out, range),
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
    for part in crate::url::encode_query_value(value) {
        out.push(part);
    }
}

// Writes `key` percent-encoded as a path: whole, if it holds no byte to
// escape.
#[inline(always)]
fn write_key(out: &mut Writer<'_>, key: &str) {
    if crate::url::key_is_plain(key.as_bytes()) {
        out.push(key.as_bytes());
    } else {
        for part in crate::url::encode_object_key(key) {
            out.push(part);
        }
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
            assert_eq!(Hole::from_id(index as u8 + 1), Some(*hole));
        }
        assert_eq!(Hole::from_id(0), None);
        assert_eq!(Hole::from_id(Hole::ALL.len() as u8 + 1), None);
    }
}
