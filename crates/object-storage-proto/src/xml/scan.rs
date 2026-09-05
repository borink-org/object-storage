// The scanner. It works the way pugixml does, not the way a tokeniser does.
// It makes one pass over the bytes, uses a 256-entry class table for the
// characters that matter, and builds no tree. At every `<` the next byte says
// what follows: `/` is a close tag, `!` is a comment or something this
// scanner refuses, `?` is a processing instruction, and anything else is an
// element. So the scanner always knows what it is looking at and never
// mistakes text for structure. A separate validating pass is not needed.
//
// The scanner refuses CDATA sections, document type declarations and other
// markup declarations, and namespace prefixes. The service writes none of
// them, and a reader that does not understand them cannot read a document
// that has them correctly. Comments and processing instructions are skipped
// where an element may stand. Inside a value they are a fault, because a
// value split by a comment is no longer one span of the buffer.
//
// The file has two layers. The functions at the top read one thing each from
// a slice at an offset and return where it ended; they hold no state, so the
// attribute walk can reuse them over a shared borrow. `Scan` is the cursor
// over the body: the bytes not yet handed out, and an offset into them. It
// holds the body mutably because a value is decoded where it stands, in the
// caller's buffer, while the read goes on past it. A shared view could not
// lend one entry out for writing while the rest was still being read. So
// `Scan::take` splits the bytes read so far off as their own borrow and
// starts the offset over at zero. The page reader calls it once before each
// child, so the spans recorded while reading the child index the child's own
// bytes, and once after an entry, to get those bytes back and decode them.
//
// The scanner is meant to be total: to read any bytes at all without a
// panic, and to report a document it cannot read as a fault. That is not
// proven. It is kept by writing each place that indexes the bytes on an
// invariant stated beside it or on one of the two below, and by treating a
// panic anywhere in this file as a bug in the scanner, never as a document's
// fault. A read that may run past the end goes through `at`, which returns
// 0 there; a document may not hold a NUL byte, so no loop needs a bounds
// test of its own.
//
// The two invariants most of the file rests on:
//
// - The cursor never passes the end of the bytes. It moves past a byte only
//   after that byte was read and matched, or past a literal that
//   `starts_with` found whole. `at` reads 0 past the end and no match
//   accepts 0, so every move stays at most at the length.
// - A span indexes the bytes it was recorded from. `Scan::take` starts the
//   offset over, so a span recorded before a `take` names nothing after it.
//   The page reader keeps to that by taking before each child and reading
//   the child's spans against the bytes the next `take` returns.

use crate::{Error, ResponseFault, Result};

pub(crate) fn fault<T>() -> Result<T> {
    Err(Error::Response(ResponseFault::Body))
}

// A range of the bytes being scanned, as offsets. A borrowed slice would
// hold the bytes while the scan needs them mutably to go on, so a span is
// read back with `Scan::text` instead, against the bytes it was recorded
// from. See the note at the top of the file.
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
    let mut code = 0usize;
    while code < 256 {
        let byte = code as u8;
        // Every byte a name may hold. Any non-ASCII byte counts as one. The
        // delimiters this scanner acts on are all ASCII, and UTF-8 never
        // puts an ASCII byte inside a multi-byte sequence.
        if byte.is_ascii_alphanumeric()
            || byte == b'-'
            || byte == b'_'
            || byte == b'.'
            || byte >= 0x80
        {
            table[code] |= NAME;
        }
        if byte == b' ' || byte == b'\t' || byte == b'\n' || byte == b'\r' {
            table[code] |= SPACE;
        }
        code += 1;
    }
    table
}

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;

// Returns the high bit of every zero byte in `x`. The result is exact for the
// lowest zero byte. Bytes above it may be flagged when they are not zero, and
// every use below allows for that.
#[inline(always)]
fn zero_bytes(word: u64) -> u64 {
    word.wrapping_sub(ONES) & !word & HIGH
}

// Returns the offset of the first `<` at or after `from`, with the [`AMP`]
// and [`PCT`] flags for the bytes in between. It reads eight bytes a step. A
// listing is mostly values, so most bytes go through this loop.
#[inline]
pub(crate) fn find_lt(bytes: &[u8], from: usize) -> Option<(usize, u8)> {
    let mut flags = 0u8;
    let mut offset = from;
    // `from` is a cursor, so it is at most the length.
    let (words, remainder) = bytes[from..].as_chunks::<8>();
    for word in words {
        // Load eight bytes at once. Written any other way this is eight bounds
        // checks and eight shifts, which the compiler does not merge.
        let word = u64::from_le_bytes(*word);
        let lt = zero_bytes(word ^ (ONES * b'<' as u64));
        let amp = zero_bytes(word ^ (ONES * b'&' as u64));
        let pct = zero_bytes(word ^ (ONES * b'%' as u64));
        if lt != 0 {
            // A false positive is always above a true hit, never below. So
            // masking to the bytes before the first `<` keeps only true hits.
            let below = lt.isolate_lowest_one() - 1;
            flags |= ((amp & below != 0) as u8) | (((pct & below != 0) as u8) << 1);
            return Some((offset + lt.trailing_zeros() as usize / 8, flags));
        }
        flags |= ((amp != 0) as u8) | (((pct != 0) as u8) << 1);
        offset += 8;
    }
    for &byte in remainder {
        match byte {
            b'<' => return Some((offset, flags)),
            b'&' => flags |= AMP,
            b'%' => flags |= PCT,
            _ => {}
        }
        offset += 1;
    }
    None
}

// Returns how many name characters `bytes` starts with. It checks four bytes at a
// time, like pugixml's unrolled scan: one branch per four bytes while the name
// continues. The tag names of a listing are short, so this is most of them.
#[inline(always)]
fn name_len(bytes: &[u8]) -> usize {
    let mut len = 0;
    let (quads, rest) = bytes.as_chunks::<4>();
    for quad in quads {
        let quad_classes = [
            CLASS[quad[0] as usize],
            CLASS[quad[1] as usize],
            CLASS[quad[2] as usize],
            CLASS[quad[3] as usize],
        ];
        if quad_classes[0] & quad_classes[1] & quad_classes[2] & quad_classes[3] & NAME != 0 {
            len += 4;
            continue;
        }
        return len
            + quad_classes
                .iter()
                .position(|class| class & NAME == 0)
                .unwrap_or(4);
    }
    len + rest
        .iter()
        .position(|class| CLASS[*class as usize] & NAME == 0)
        .unwrap_or(rest.len())
}

// Returns the offset of the first `needle` at or after `from`, or the length
// of `bytes` if there is none. `from` may be the length, so that a caller can
// go on from where the last search ended.
#[inline]
pub(crate) fn find_byte(bytes: &[u8], from: usize, needle: u8) -> usize {
    let mut offset = from;
    let (words, remainder) = bytes[from..].as_chunks::<8>();
    for word in words {
        let word = u64::from_le_bytes(*word);
        let hit = zero_bytes(word ^ (ONES * needle as u64));
        if hit != 0 {
            return offset + hit.trailing_zeros() as usize / 8;
        }
        offset += 8;
    }
    for &byte in remainder {
        if byte == needle {
            return offset;
        }
        offset += 1;
    }
    offset
}

// Returns the byte at `offset`, or 0 past the end. A document may not hold a NUL
// byte, so 0 can stand for the end and no loop needs its own bounds test.
#[inline(always)]
fn at(bytes: &[u8], offset: usize) -> u8 {
    bytes.get(offset).copied().unwrap_or(0)
}

// Returns how many whitespace bytes stand at `from`.
#[inline(always)]
fn space_len(bytes: &[u8], from: usize) -> usize {
    let mut offset = from;
    while CLASS[at(bytes, offset) as usize] & SPACE != 0 {
        offset += 1;
    }
    offset - from
}

// Reads the name that begins at `from`.
#[inline(always)]
fn name(bytes: &[u8], from: usize) -> Result<Span> {
    let end = from + bytes.get(from..).map_or(0, name_len);
    // A listing has no namespace prefixes. Reading a document that has them
    // would mean resolving the prefixes, which this crate does not do.
    if end == from || at(bytes, end) == b':' {
        return fault();
    }
    Ok((from, end))
}

// Reads the quoted string that begins at `from` and returns the span between
// the quotes. The closing quote is the byte after it.
fn quoted(bytes: &[u8], from: usize) -> Result<Span> {
    let quote = at(bytes, from);
    if quote != b'"' && quote != b'\'' {
        return fault();
    }
    let start = from + 1;
    let mut offset = start;
    loop {
        match at(bytes, offset) {
            byte if byte == quote => return Ok((start, offset)),
            0 | b'<' => return fault(),
            _ => offset += 1,
        }
    }
}

// Reads the attribute that begins at `from`: a name, `=` and a quoted value,
// with whitespace allowed around the `=`. Returns the two spans and the
// offset after the closing quote.
fn attribute(bytes: &[u8], from: usize) -> Result<(Span, Span, usize)> {
    let name = name(bytes, from)?;
    let mut offset = name.1 + space_len(bytes, name.1);
    if at(bytes, offset) != b'=' {
        return fault();
    }
    offset += 1;
    offset += space_len(bytes, offset);
    let value = quoted(bytes, offset)?;
    Ok((name, value, value.1 + 1))
}

// Reads the close tag that begins at `from` and checks that it names `name`.
// Returns the offset after its `>`.
#[inline(always)]
fn close_tag(bytes: &[u8], from: usize, name: &[u8]) -> Result<usize> {
    // The service writes `</name>` with no whitespace, so compare the whole
    // tag first and only scan a name if that fails.
    let start = from + 2;
    let end = start + name.len();
    // The range is in bounds once `end` is below the length, and `start`
    // is at most `end` because `end` is `start` plus a length.
    if bytes.len() > end && &bytes[start..end] == name && bytes[end] == b'>' {
        return Ok(end + 1);
    }
    close_tag_slow(bytes, from, name)
}

#[inline(never)]
fn close_tag_slow(bytes: &[u8], from: usize, expected: &[u8]) -> Result<usize> {
    let found = name(bytes, from + 2)?;
    // `name` records a span of `bytes`.
    if &bytes[found.0..found.1] != expected {
        return fault();
    }
    let end = found.1 + space_len(bytes, found.1);
    if at(bytes, end) != b'>' {
        return fault();
    }
    Ok(end + 1)
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

// The cursor over the body. See the note at the top of the file.
pub(crate) struct Scan<'b> {
    // The bytes not yet handed out by `take`. Every span this scanner
    // returns indexes these bytes.
    bytes: &'b mut [u8],
    // The offset of the next byte to read. Never past the end of `bytes`;
    // see the note at the top of the file.
    cursor: usize,
}

impl<'b> Scan<'b> {
    pub(crate) fn new(bytes: &'b mut [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    // Splits the bytes read so far off the body and returns them as their
    // own borrow. The scan goes on from the byte after them, at offset zero.
    pub(crate) fn take(&mut self) -> &'b mut [u8] {
        // In range because the cursor never passes the end.
        let (read, rest) = core::mem::take(&mut self.bytes).split_at_mut(self.cursor);
        self.bytes = rest;
        self.cursor = 0;
        read
    }

    // Returns the byte `ahead` bytes past the cursor, or 0 past the end.
    #[inline(always)]
    pub(crate) fn peek(&self, ahead: usize) -> u8 {
        at(self.bytes, self.cursor + ahead)
    }

    #[inline(always)]
    pub(crate) fn cur(&self) -> u8 {
        self.peek(0)
    }

    // Returns the bytes of a span this scanner recorded since the last
    // `take`, which is what makes the range in bounds.
    #[inline(always)]
    pub(crate) fn text(&self, span: Span) -> &[u8] {
        &self.bytes[span.0..span.1]
    }

    // Written on the cursor rather than through `space_len`: the latter gave
    // LLVM a loop it rotated into a shape that ran its body once even when
    // there was no whitespace, which cost three percent of a page.
    #[inline(always)]
    pub(crate) fn skip_space(&mut self) {
        while CLASS[self.cur() as usize] & SPACE != 0 {
            self.cursor += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> Result<()> {
        if self.cur() == expected {
            self.cursor += 1;
            Ok(())
        } else {
            fault()
        }
    }

    fn name(&mut self) -> Result<Span> {
        let span = name(self.bytes, self.cursor)?;
        self.cursor = span.1;
        Ok(span)
    }

    // Reads a start tag. Starts at its `<` and consumes through its `>`. The
    // attributes are checked here and read again by [`Self::attributes`] if
    // the grammar wants them, which on a listing is only for `<Name>`.
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
                    let (_, _, after) = attribute(self.bytes, self.cursor)?;
                    self.cursor = after;
                }
            }
        }
    }

    // Returns every attribute of `tag`, in the order they were written. This
    // reads the span that [`Self::open`] already checked again. That span is
    // empty on nearly every tag either service writes.
    pub(crate) fn attributes(&self, tag: Tag) -> Attributes<'_> {
        Attributes {
            // The span ends where the cursor stood at the tag's `>` or `/`.
            bytes: &self.bytes[..tag.attributes.1],
            cursor: tag.attributes.0,
        }
    }

    // Reads a close tag and checks that it names `name`. Starts at its `<`
    // and consumes through its `>`.
    #[inline(always)]
    pub(crate) fn close(&mut self, name: &[u8]) -> Result<()> {
        self.cursor = close_tag(self.bytes, self.cursor, name)?;
        Ok(())
    }

    // Starts at a `<`. Skips a comment or a processing instruction and returns
    // true. Refuses a CDATA section or a markup declaration. Leaves an element
    // tag alone and returns false.
    pub(crate) fn skip_misc(&mut self) -> Result<bool> {
        match self.peek(1) {
            b'!' => {
                if self.peek(2) == b'-' && self.peek(3) == b'-' {
                    self.cursor += 4;
                    loop {
                        match self.cur() {
                            0 => return fault(),
                            b'-' if self.peek(1) == b'-' => {
                                if self.peek(2) == b'>' {
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
                        b'?' if self.peek(1) == b'>' => {
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
            if self.peek(1) == b'/' {
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
        let (value, end) = self.value_end(self.text(tag.name))?;
        self.cursor = end;
        Ok(value)
    }

    // The same, for a start tag matched whole by [`Self::lit`], which has no
    // `Tag` to name it.
    #[inline(always)]
    pub(crate) fn value_of(&mut self, name: &[u8]) -> Result<(Span, u8)> {
        let (value, end) = self.value_end(name)?;
        self.cursor = end;
        Ok(value)
    }

    // Finds the text and the close tag of a value element, and returns the
    // text's span and flags with the offset after the close tag.
    #[inline(always)]
    fn value_end(&self, name: &[u8]) -> Result<((Span, u8), usize)> {
        let start = self.cursor;
        let Some((end, flags)) = find_lt(self.bytes, start) else {
            return fault();
        };
        // Anything but the close tag here splits the value in two. Returning
        // one of the pieces would report a value the service did not write.
        if at(self.bytes, end + 1) != b'/' {
            return fault();
        }
        let after = close_tag(self.bytes, end, name)?;
        Ok((((start, end), flags), after))
    }

    // Consumes `lit` if the bytes here are exactly `lit`. A page has only a
    // handful of known tags, so this avoids scanning names: `<Name>` is one
    // six-byte compare, and anything else falls through to `child`.
    #[inline(always)]
    pub(crate) fn lit(&mut self, lit: &[u8]) -> bool {
        // In range because the cursor never passes the end.
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
            let Some((tag_at, _)) = find_lt(self.bytes, self.cursor) else {
                return fault();
            };
            self.cursor = tag_at;
            match self.peek(1) {
                b'/' => {
                    // `depth` is at least one here: the loop returns when
                    // it reaches zero.
                    depth -= 1;
                    self.cursor = close_tag(self.bytes, tag_at, self.text(open[depth]))?;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                b'!' | b'?' => {
                    self.skip_misc()?;
                }
                _ => {
                    let child = self.open()?;
                    if !child.empty {
                        // The fixed array checks the nesting without a heap.
                        // A document deeper than it is not a listing.
                        if depth == MAX_DEPTH {
                            return fault();
                        }
                        open[depth] = child.name;
                        depth += 1;
                    }
                }
            }
        }
    }
}

// The attributes of one tag, read from the span of the body that
// [`Scan::open`] recorded for them. The slice ends where the tag's `>` or `/`
// stood, so a value cannot run past it.
pub(crate) struct Attributes<'s> {
    bytes: &'s [u8],
    cursor: usize,
}

impl<'s> Iterator for Attributes<'s> {
    type Item = Result<(&'s [u8], &'s [u8])>;

    fn next(&mut self) -> Option<Self::Item> {
        self.cursor += space_len(self.bytes, self.cursor);
        if self.cursor >= self.bytes.len() {
            return None;
        }
        Some(
            attribute(self.bytes, self.cursor).map(|(name, value, after)| {
                self.cursor = after;
                // `attribute` records spans of these bytes.
                (&self.bytes[name.0..name.1], &self.bytes[value.0..value.1])
            }),
        )
    }
}

// Removes the whitespace around a value that the service writes for itself.
// A key is never trimmed: a key may begin or end with a space, and that space
// is part of it. The span was recorded on `bytes`, so `end` is at most their
// length, and each index below stays between `start` and `end`.
pub(crate) fn trim(bytes: &[u8], (mut start, mut end): Span) -> Span {
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}
