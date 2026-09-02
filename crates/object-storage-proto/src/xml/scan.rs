// The structural scanner, built the way pugixml scans rather than the way a
// tokeniser does: one pass over the bytes, a 256-entry class table for the
// characters that matter, and no tree. At every `<` the next byte says what
// stands there — `/` a close tag, `!` a comment or something this refuses,
// `?` an instruction, anything else an element — so the scan always knows
// what it is looking at and never takes text for structure. That is what a
// separate validating pass used to buy, and here it costs nothing.
//
// What this refuses: character-data sections, document type and other markup
// declarations, and namespace prefixes. The service writes none of them, and
// a reader that does not model them cannot read a document that carries them
// soundly. Comments and processing instructions are skipped where elements
// are allowed; inside a value they are a fault, because a value split by a
// comment is no longer one span of the buffer.

use crate::{Error, ResponseFault, Result};

pub(crate) fn fault<T>() -> Result<T> {
    Err(Error::Response(ResponseFault::Body))
}

// A range of the bytes being scanned.
pub(crate) type Span = (usize, usize);

// Set on a value's flags when it holds a `&`.
pub(crate) const AMP: u8 = 1;
// Set on a value's flags when it holds a `%`.
pub(crate) const PCT: u8 = 2;

const NAME: u8 = 1;
const SPACE: u8 = 2;

static CLASS: [u8; 256] = classes();

const fn classes() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut c = 0usize;
    while c < 256 {
        let b = c as u8;
        // Everything a name character can be, treating any non-ASCII byte as
        // one: the delimiters this scanner acts on are all ASCII, and UTF-8
        // never spells an ASCII byte inside a multi-byte sequence.
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b >= 0x80 {
            table[c] |= NAME;
        }
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
            table[c] |= SPACE;
        }
        c += 1;
    }
    table
}

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;

// The high bit of every zero byte in `x`. Exact for the lowest such byte;
// bytes above the lowest may be flagged when they are not zero, which every
// use below is written to tolerate.
#[inline(always)]
fn zero_bytes(x: u64) -> u64 {
    x.wrapping_sub(ONES) & !x & HIGH
}

// The offset of the first `<` at or after `from`, with the [`AMP`] and
// [`PCT`] flags for what lies between. Eight bytes a step; a listing is
// mostly values, so this is the loop that most bytes go through.
#[inline]
pub(crate) fn find_lt(b: &[u8], from: usize) -> Option<(usize, u8)> {
    let mut flags = 0u8;
    let mut i = from;
    let (words, remainder) = b[from..].as_chunks::<8>();
    for word in words {
        // Eight bytes as one load. Written any other way this is eight bounds
        // checks and eight shifts, which the compiler does not merge.
        let w = u64::from_le_bytes(*word);
        let lt = zero_bytes(w ^ (ONES * b'<' as u64));
        let amp = zero_bytes(w ^ (ONES * b'&' as u64));
        let pct = zero_bytes(w ^ (ONES * b'%' as u64));
        if lt != 0 {
            // A false positive sits above a true hit, never below, so masking
            // the bytes before the first `<` keeps only true hits.
            let below = lt.isolate_lowest_one() - 1;
            flags |= ((amp & below != 0) as u8) | (((pct & below != 0) as u8) << 1);
            return Some((i + lt.trailing_zeros() as usize / 8, flags));
        }
        flags |= ((amp != 0) as u8) | (((pct != 0) as u8) << 1);
        i += 8;
    }
    for &c in remainder {
        match c {
            b'<' => return Some((i, flags)),
            b'&' => flags |= AMP,
            b'%' => flags |= PCT,
            _ => {}
        }
        i += 1;
    }
    None
}

// How many name characters `b` starts with. Four at a time, the way pugixml's
// unrolled scan does it: one branch per four bytes while the name goes on,
// which for the tag names of a listing is all of it.
#[inline(always)]
fn name_len(b: &[u8]) -> usize {
    let mut n = 0;
    let (quads, rest) = b.as_chunks::<4>();
    for q in quads {
        let c = [
            CLASS[q[0] as usize],
            CLASS[q[1] as usize],
            CLASS[q[2] as usize],
            CLASS[q[3] as usize],
        ];
        if c[0] & c[1] & c[2] & c[3] & NAME != 0 {
            n += 4;
            continue;
        }
        return n + c.iter().position(|x| x & NAME == 0).unwrap_or(4);
    }
    n + rest
        .iter()
        .position(|x| CLASS[*x as usize] & NAME == 0)
        .unwrap_or(rest.len())
}

// The offset of the first `needle` at or after `from`, or the length.
#[inline]
pub(crate) fn find_byte(b: &[u8], from: usize, needle: u8) -> usize {
    let mut i = from;
    let (words, remainder) = b[from..].as_chunks::<8>();
    for word in words {
        let w = u64::from_le_bytes(*word);
        let hit = zero_bytes(w ^ (ONES * needle as u64));
        if hit != 0 {
            return i + hit.trailing_zeros() as usize / 8;
        }
        i += 8;
    }
    for &c in remainder {
        if c == needle {
            return i;
        }
        i += 1;
    }
    i
}

// A listing nests the root, the entries, one entry and its properties, so an
// element inside a property is already deeper than a listing goes.
const MAX_DEPTH: usize = 16;

#[derive(Clone, Copy)]
pub(crate) struct Tag {
    pub(crate) name: Span,
    pub(crate) attrs: Span,
    pub(crate) empty: bool,
}

pub(crate) enum Child {
    Open(Tag),
    Close,
}

pub(crate) struct Scan<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) i: usize,
}

impl<'a> Scan<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

    // The byte at `i`, or 0 past the end. NUL is not a character a document
    // may hold, so one sentinel serves for both and no loop needs a bounds
    // test of its own.
    #[inline(always)]
    fn at(&self, i: usize) -> u8 {
        self.b.get(i).copied().unwrap_or(0)
    }

    #[inline(always)]
    pub(crate) fn cur(&self) -> u8 {
        self.at(self.i)
    }

    #[inline(always)]
    pub(crate) fn text(&self, span: Span) -> &'a [u8] {
        &self.b[span.0..span.1]
    }

    #[inline(always)]
    pub(crate) fn skip_space(&mut self) {
        while CLASS[self.cur() as usize] & SPACE != 0 {
            self.i += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        if self.cur() == c {
            self.i += 1;
            Ok(())
        } else {
            fault()
        }
    }

    fn name(&mut self) -> Result<Span> {
        let start = self.i;
        self.i += name_len(&self.b[start..]);
        // A listing carries no namespace prefix. A document that does is not
        // this one however its names would resolve, and resolving them is
        // what this crate would then have to do.
        if self.i == start || self.cur() == b':' {
            return fault();
        }
        Ok((start, self.i))
    }

    // At the `<` of a start tag. Consumes through its `>`.
    #[inline(always)]
    pub(crate) fn open(&mut self) -> Result<Tag> {
        self.i += 1;
        let name = self.name()?;
        let start = self.i;
        loop {
            self.skip_space();
            match self.cur() {
                b'>' => {
                    let attrs = (start, self.i);
                    self.i += 1;
                    return Ok(Tag {
                        name,
                        attrs,
                        empty: false,
                    });
                }
                b'/' => {
                    let attrs = (start, self.i);
                    self.i += 1;
                    self.expect(b'>')?;
                    return Ok(Tag {
                        name,
                        attrs,
                        empty: true,
                    });
                }
                _ => {
                    self.name()?;
                    self.skip_space();
                    self.expect(b'=')?;
                    self.skip_space();
                    self.quoted()?;
                }
            }
        }
    }

    fn quoted(&mut self) -> Result<Span> {
        let quote = self.cur();
        if quote != b'"' && quote != b'\'' {
            return fault();
        }
        self.i += 1;
        let start = self.i;
        loop {
            match self.cur() {
                c if c == quote => break,
                0 | b'<' => return fault(),
                _ => self.i += 1,
            }
        }
        let span = (start, self.i);
        self.i += 1;
        Ok(span)
    }

    // Every attribute of `tag`, in the order it was written. Re-reads the
    // span that [`Self::open`] already checked, which is empty on nearly
    // every tag either service writes.
    pub(crate) fn attributes(&self, tag: Tag) -> Attributes<'a> {
        Attributes {
            scan: Scan {
                b: &self.b[..tag.attrs.1],
                i: tag.attrs.0,
            },
        }
    }

    // At the `<` of a close tag. Consumes through its `>` and checks that it
    // names `name`.
    #[inline(always)]
    pub(crate) fn close(&mut self, name: &[u8]) -> Result<()> {
        // `</name>` with nothing in between is what the service writes, so
        // compare that whole before scanning a name.
        let at = self.i + 2;
        if self.b.len() > at + name.len()
            && &self.b[at..at + name.len()] == name
            && self.b[at + name.len()] == b'>'
        {
            self.i = at + name.len() + 1;
            return Ok(());
        }
        self.close_slow(name)
    }

    #[inline(never)]
    fn close_slow(&mut self, name: &[u8]) -> Result<()> {
        self.i += 2;
        let found = self.name()?;
        if self.text(found) != name {
            return fault();
        }
        self.skip_space();
        self.expect(b'>')
    }

    // At a `<`. Skips a comment or a processing instruction and says so;
    // refuses a character-data section and a markup declaration; leaves an
    // element tag alone.
    pub(crate) fn skip_misc(&mut self) -> Result<bool> {
        match self.at(self.i + 1) {
            b'!' => {
                if self.at(self.i + 2) == b'-' && self.at(self.i + 3) == b'-' {
                    self.i += 4;
                    loop {
                        match self.cur() {
                            0 => return fault(),
                            b'-' if self.at(self.i + 1) == b'-' => {
                                if self.at(self.i + 2) == b'>' {
                                    self.i += 3;
                                    return Ok(true);
                                }
                                return fault();
                            }
                            _ => self.i += 1,
                        }
                    }
                }
                // A character-data section and a markup declaration may both
                // hold the very tags that the entries are read by.
                fault()
            }
            b'?' => {
                self.i += 2;
                loop {
                    match self.cur() {
                        0 => return fault(),
                        b'?' if self.at(self.i + 1) == b'>' => {
                            self.i += 2;
                            return Ok(true);
                        }
                        _ => self.i += 1,
                    }
                }
            }
            _ => Ok(false),
        }
    }

    // Inside an element that holds elements. Returns the next child's start
    // tag, consumed, or `Close` once the parent's close tag is consumed.
    #[inline(always)]
    pub(crate) fn child(&mut self, parent: &[u8]) -> Result<Child> {
        loop {
            self.skip_space();
            if self.cur() != b'<' {
                return fault();
            }
            if self.at(self.i + 1) == b'/' {
                self.close(parent)?;
                return Ok(Child::Close);
            }
            if self.skip_misc()? {
                continue;
            }
            return Ok(Child::Open(self.open()?));
        }
    }

    // The start tag of a value element has been consumed. Consumes its text
    // and its close tag; returns the text's span and its decode flags.
    #[inline]
    pub(crate) fn value(&mut self, tag: Tag) -> Result<(Span, u8)> {
        if tag.empty {
            return Ok(((self.i, self.i), 0));
        }
        self.value_of(self.text(tag.name))
    }

    #[inline(always)]
    pub(crate) fn value_of(&mut self, name: &[u8]) -> Result<(Span, u8)> {
        let start = self.i;
        let Some((end, flags)) = find_lt(self.b, start) else {
            return fault();
        };
        self.i = end;
        // Anything but the close tag here would split the value in two, and
        // taking one of the pieces would report what the service did not say.
        if self.at(end + 1) != b'/' {
            return fault();
        }
        self.close(name)?;
        Ok(((start, end), flags))
    }

    // Consumes `lit` if the bytes here are exactly it. This is how a grammar
    // of a handful of known tags avoids scanning names at all: `<Name>` is
    // one six-byte compare, and everything else falls through to `child`.
    #[inline(always)]
    pub(crate) fn lit(&mut self, lit: &[u8]) -> bool {
        if self.b[self.i..].starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    // The start tag has been consumed. Consumes everything through the
    // matching close tag, checking that the tags in between nest.
    pub(crate) fn skip(&mut self, tag: Tag) -> Result<()> {
        if tag.empty {
            return Ok(());
        }
        let mut open = [(0usize, 0usize); MAX_DEPTH];
        open[0] = tag.name;
        let mut depth = 1;
        loop {
            let Some((at, _)) = find_lt(self.b, self.i) else {
                return fault();
            };
            self.i = at;
            match self.at(at + 1) {
                b'/' => {
                    depth -= 1;
                    self.close(self.text(open[depth]))?;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                b'!' | b'?' => {
                    self.skip_misc()?;
                }
                _ => {
                    let t = self.open()?;
                    if !t.empty {
                        // The array is what lets the nesting be checked with
                        // no heap. A document deeper than it is not a listing.
                        if depth == MAX_DEPTH {
                            return fault();
                        }
                        open[depth] = t.name;
                        depth += 1;
                    }
                }
            }
        }
    }
}

// The attributes of one tag, as spans of the bytes they were read from.
pub(crate) struct Attributes<'a> {
    scan: Scan<'a>,
}

impl<'a> Iterator for Attributes<'a> {
    type Item = Result<(&'a [u8], &'a [u8])>;

    fn next(&mut self) -> Option<Self::Item> {
        self.scan.skip_space();
        if self.scan.i >= self.scan.b.len() {
            return None;
        }
        Some(self.read())
    }
}

impl<'a> Attributes<'a> {
    fn read(&mut self) -> Result<(&'a [u8], &'a [u8])> {
        let name = self.scan.name()?;
        self.scan.skip_space();
        if self.scan.cur() != b'=' {
            return fault();
        }
        self.scan.i += 1;
        self.scan.skip_space();
        let value = self.scan.quoted()?;
        // The spans are of the whole body, which outlives the attribute span
        // this scan was confined to.
        Ok((self.scan.text(name), self.scan.text(value)))
    }
}

// Hands out the first `count` bytes as their own borrow of the body, which is
// what lets an entry be decoded while the read goes on past it.
pub(crate) fn split_off<'b>(rest: &mut &'b mut [u8], count: usize) -> &'b mut [u8] {
    let (head, tail) = core::mem::take(rest).split_at_mut(count);
    *rest = tail;
    head
}

// A value that the service writes for itself, without the whitespace that the
// document may hold around it. A key is never trimmed: a key may begin or end
// with a space, and that space is part of it.
pub(crate) fn trim(bytes: &[u8], (mut start, mut end): Span) -> Span {
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}
