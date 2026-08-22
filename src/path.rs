use percent_encoding::{AsciiSet, CONTROLS, PercentEncode, utf8_percent_encode};

// Escape bytes that could change the URL structure. A slash remains literal
// because Azure blob names use it to represent virtual directory segments.
const OBJECT_KEY_ESCAPE: &AsciiSet = &CONTROLS
    .add(b':')
    .add(b'?')
    .add(b'#')
    .add(b'[')
    .add(b']')
    .add(b'@')
    .add(b'!')
    .add(b'$')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b';')
    .add(b'=')
    .add(b'"')
    .add(b' ')
    .add(b'<')
    .add(b'>')
    .add(b'%')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\')
    .add(b'^')
    .add(b'`');

pub(crate) fn encode_object_key(value: &str) -> PercentEncode<'_> {
    utf8_percent_encode(value, OBJECT_KEY_ESCAPE)
}
