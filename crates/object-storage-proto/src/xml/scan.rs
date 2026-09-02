// The scanner. It works the way pugixml does, not the way a tokeniser does.
// It makes one pass over the bytes, uses a 256-entry class table for the
// characters that matter, and builds no tree. At every `<` the next byte says
// what follows: `/` is a
// close tag, `!` is a comment or something this scanner refuses, `?` is a
// processing instruction, and anything else is an element. So the scanner
// always knows what it is looking at and never mistakes text for structure.
// A separate validating pass is not needed.
//
// The scanner refuses CDATA sections, document type declarations and other
// markup declarations, and namespace prefixes. The service writes none of
// them, and a reader that does not understand them cannot read a document
// that has them correctly. Comments and processing instructions are skipped
// where an element may stand. Inside a value they are a fault, because a
// value split by a comment is no longer one span of the buffer.

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
        // Every byte a name may hold. Any non-ASCII byte counts as one. The
        // delimiters this scanner acts on are all ASCII, and UTF-8 never
        // puts an ASCII byte inside a multi-byte sequence.
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

// Returns the high bit of every zero byte in `x`. The result is exact for the
// lowest zero byte. Bytes above it may be flagged when they are not zero, and
// every use below allows for that.
#[inline(always)]
fn zero_bytes(x: u64) -> u64 {
    x.wrapping_sub(ONES) & !x & HIGH
}

// Returns the offset of the first `<` at or after `from`, with the [`AMP`]
// and [`PCT`] flags for the bytes in between. It reads eight bytes a step. A
// listing is mostly values, so most bytes go through this loop.
#[inline]
pub(crate) fn find_lt(b: &[u8], from: usize) -> Option<(usize, u8)> {
    let mut flags = 0u8;
    let mut i = from;
    let (words, remainder) = b[from..].as_chunks::<8>();
    for word in words {
        // Load eight bytes at once. Written any other way this is eight bounds
        // checks and eight shifts, which the compiler does not merge.
        let w = u64::from_le_bytes(*word);
        let lt = zero_bytes(w ^ (ONES * b'<' as u64));
        let amp = zero_bytes(w ^ (ONES * b'&' as u64));
        let pct = zero_bytes(w ^ (ONES * b'%' as u64));
        if lt != 0 {
            // A false positive is always above a true hit, never below. So
            // masking to the bytes before the first `<` keeps only true hits.
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

// Returns how many name characters `b` starts with. It checks four bytes at a
// time, like pugixml's unrolled scan: one branch per four bytes while the name
// continues. The tag names of a listing are short, so this is most of them.
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

// Returns the offset of the first `needle` at or after `from`, or the length
// of `b` if there is none.
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

// A listing nests four levels: the root, the entries, one entry, and its
// properties. An element inside a property is already deeper than that.
const MAX_DEPTH: usize = 16;

#[derive(Clone, Copy)]
pub(crate) struct Tag {
    pub(crate) name: Span,
    pub(crate) attributes: Span,
    pub(crate) empty: bool,
}

pub(crate) enum Child {
    Open(Tag),
    Close,
}

pub(crate) struct Scan<'a> {
    pub(crate) bytes: &'a [u8],
    // The offset of the next byte to read.
    pub(crate) cursor: usize,
}

impl<'a> Scan<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    // Returns the byte at `i`, or 0 past the end. A document may not hold a
    // NUL byte, so 0 can stand for the end and no loop needs its own bounds
    // test.
    #[inline(always)]
    fn at(&self, i: usize) -> u8 {
        self.bytes.get(i).copied().unwrap_or(0)
    }

    #[inline(always)]
    pub(crate) fn cur(&self) -> u8 {
        self.at(self.cursor)
    }

    #[inline(always)]
    pub(crate) fn text(&self, span: Span) -> &'a [u8] {
        &self.bytes[span.0..span.1]
    }

    #[inline(always)]
    pub(crate) fn skip_space(&mut self) {
        while CLASS[self.cur() as usize] & SPACE != 0 {
            self.cursor += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        if self.cur() == c {
            self.cursor += 1;
            Ok(())
        } else {
            fault()
        }
    }

    fn name(&mut self) -> Result<Span> {
        let start = self.cursor;
        self.cursor += name_len(&self.bytes[start..]);
        // A listing has no namespace prefixes. Reading a document that has
        // them would mean resolving the prefixes, which this crate does not
        // do.
        if self.cursor == start || self.cur() == b':' {
            return fault();
        }
        Ok((start, self.cursor))
    }

    // Reads a start tag. Starts at its `<` and consumes through its `>`.
    #[inline(always)]
    pub(crate) fn open(&mut self) -> Result<Tag> {
        self.cursor += 1;
        let name = self.name()?;
        let start = self.cursor;
        loop {
            self.skip_space();
            match self.cur() {
                b'>' => {
                    let attributes = (start, self.cursor);
                    self.cursor += 1;
                    return Ok(Tag {
                        name,
                        attributes,
                        empty: false,
                    });
                }
                b'/' => {
                    let attributes = (start, self.cursor);
                    self.cursor += 1;
                    self.expect(b'>')?;
                    return Ok(Tag {
                        name,
                        attributes,
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
        self.cursor += 1;
        let start = self.cursor;
        loop {
            match self.cur() {
                c if c == quote => break,
                0 | b'<' => return fault(),
                _ => self.cursor += 1,
            }
        }
        let span = (start, self.cursor);
        self.cursor += 1;
        Ok(span)
    }

    // Returns every attribute of `tag`, in the order they were written. This
    // reads the span that [`Self::open`] already checked again. That span is
    // empty on nearly every tag either service writes.
    pub(crate) fn attributes(&self, tag: Tag) -> Attributes<'a> {
        Attributes {
            scan: Scan {
                bytes: &self.bytes[..tag.attributes.1],
                cursor: tag.attributes.0,
            },
        }
    }

    // Reads a close tag and checks that it names `name`. Starts at its `<`
    // and consumes through its `>`.
    #[inline(always)]
    pub(crate) fn close(&mut self, name: &[u8]) -> Result<()> {
        // The service writes `</name>` with no whitespace, so compare the
        // whole tag first and only scan a name if that fails.
        let at = self.cursor + 2;
        if self.bytes.len() > at + name.len()
            && &self.bytes[at..at + name.len()] == name
            && self.bytes[at + name.len()] == b'>'
        {
            self.cursor = at + name.len() + 1;
            return Ok(());
        }
        self.close_slow(name)
    }

    #[inline(never)]
    fn close_slow(&mut self, name: &[u8]) -> Result<()> {
        self.cursor += 2;
        let found = self.name()?;
        if self.text(found) != name {
            return fault();
        }
        self.skip_space();
        self.expect(b'>')
    }

    // Starts at a `<`. Skips a comment or a processing instruction and returns
    // true. Refuses a CDATA section or a markup declaration. Leaves an element
    // tag alone and returns false.
    pub(crate) fn skip_misc(&mut self) -> Result<bool> {
        match self.at(self.cursor + 1) {
            b'!' => {
                if self.at(self.cursor + 2) == b'-' && self.at(self.cursor + 3) == b'-' {
                    self.cursor += 4;
                    loop {
                        match self.cur() {
                            0 => return fault(),
                            b'-' if self.at(self.cursor + 1) == b'-' => {
                                if self.at(self.cursor + 2) == b'>' {
                                    self.cursor += 3;
                                    return Ok(true);
                                }
                                return fault();
                            }
                            _ => self.cursor += 1,
                        }
                    }
                }
                // A CDATA section and a markup declaration may both hold the
                // tags that entries are read by.
                fault()
            }
            b'?' => {
                self.cursor += 2;
                loop {
                    match self.cur() {
                        0 => return fault(),
                        b'?' if self.at(self.cursor + 1) == b'>' => {
                            self.cursor += 2;
                            return Ok(true);
                        }
                        _ => self.cursor += 1,
                    }
                }
            }
            _ => Ok(false),
        }
    }

    // Reads the next child of an element that holds elements. Returns the
    // child's start tag after consuming it, or `Close` after consuming the
    // parent's close tag.
    #[inline(always)]
    pub(crate) fn child(&mut self, parent: &[u8]) -> Result<Child> {
        loop {
            self.skip_space();
            if self.cur() != b'<' {
                return fault();
            }
            if self.at(self.cursor + 1) == b'/' {
                self.close(parent)?;
                return Ok(Child::Close);
            }
            if self.skip_misc()? {
                continue;
            }
            return Ok(Child::Open(self.open()?));
        }
    }

    // Reads the text of a value element whose start tag was consumed, then
    // consumes its close tag. Returns the span of the text and its decode
    // flags.
    #[inline]
    pub(crate) fn value(&mut self, tag: Tag) -> Result<(Span, u8)> {
        if tag.empty {
            return Ok(((self.cursor, self.cursor), 0));
        }
        self.value_of(self.text(tag.name))
    }

    #[inline(always)]
    pub(crate) fn value_of(&mut self, name: &[u8]) -> Result<(Span, u8)> {
        let start = self.cursor;
        let Some((end, flags)) = find_lt(self.bytes, start) else {
            return fault();
        };
        self.cursor = end;
        // Anything but the close tag here splits the value in two. Returning
        // one of the pieces would report a value the service did not write.
        if self.at(end + 1) != b'/' {
            return fault();
        }
        self.close(name)?;
        Ok(((start, end), flags))
    }

    // Consumes `lit` if the bytes here are exactly `lit`. A page has only a
    // handful of known tags, so this avoids scanning names: `<Name>` is one
    // six-byte compare, and anything else falls through to `child`.
    #[inline(always)]
    pub(crate) fn lit(&mut self, lit: &[u8]) -> bool {
        if self.bytes[self.cursor..].starts_with(lit) {
            self.cursor += lit.len();
            true
        } else {
            false
        }
    }

    // Skips an element whose start tag was consumed. Consumes everything
    // through the matching close tag and checks that the tags in between
    // nest.
    pub(crate) fn skip(&mut self, tag: Tag) -> Result<()> {
        if tag.empty {
            return Ok(());
        }
        let mut open = [(0usize, 0usize); MAX_DEPTH];
        open[0] = tag.name;
        let mut depth = 1;
        loop {
            let Some((at, _)) = find_lt(self.bytes, self.cursor) else {
                return fault();
            };
            self.cursor = at;
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
                        // The fixed array checks the nesting without a heap.
                        // A document deeper than it is not a listing.
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
        if self.scan.cursor >= self.scan.bytes.len() {
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
        self.scan.cursor += 1;
        self.scan.skip_space();
        let value = self.scan.quoted()?;
        // The spans index the whole body, which outlives the attribute span
        // this scan was limited to.
        Ok((self.scan.text(name), self.scan.text(value)))
    }
}

// Splits the first `count` bytes off as their own borrow of the body. This
// lets an entry be decoded while the read continues past it.
pub(crate) fn split_off<'b>(rest: &mut &'b mut [u8], count: usize) -> &'b mut [u8] {
    let (head, tail) = core::mem::take(rest).split_at_mut(count);
    *rest = tail;
    head
}

// Removes the whitespace around a value that the service writes for itself.
// A key is never trimmed: a key may begin or end with a space, and that space
// is part of it.
pub(crate) fn trim(bytes: &[u8], (mut start, mut end): Span) -> Span {
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}
