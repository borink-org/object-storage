// The query of a request, which both providers write the same way: optional
// parameters, each a name and a value, written as `name=value` pairs joined
// by `&`, in the order given.
//
// A name is a constant of this crate, such as `prefix`, and needs no
// percent-encoding. A value is one of the forms below. An S3 request signs its
// query, and the URL carries it in the canonical form that SigV4 signs. That
// holds only when the caller lists the parameters in the order of their names
// and every value reads the same whether or not it is percent-encoded again.

use crate::request::U64Decimal;

// One query value, in the form that the writer needs it.
#[derive(Clone, Copy)]
pub(crate) enum QueryValue<'q> {
    // A constant of this crate, such as `url` in `encoding-type=url`. It
    // holds only bytes that a URL carries as they are, so it is written
    // unencoded.
    Literal(&'q str),
    // Text from the caller or the service, such as a prefix or a marker,
    // which is percent-encoded as it is written.
    Encoded(&'q [u8]),
    Number(u32),
    // Constants of this crate joined by commas, such as `metadata` in
    // Azure's `include=metadata`. The commas are written unencoded, and SigV4
    // would encode them, so a signed query does not use this form.
    Words(&'q [&'q str]),
}

// One parameter, or `None` for one that the request leaves out.
pub(crate) type Parameter<'q> = Option<(&'q str, QueryValue<'q>)>;

// Writes the parameters that `query` holds, without the `?` that begins a
// query in a URL.
pub(crate) fn write(out: &mut dyn FnMut(&[u8]), query: &[Parameter<'_>]) {
    for (index, (name, value)) in query.iter().flatten().enumerate() {
        if index != 0 {
            out(b"&");
        }
        out(name.as_bytes());
        out(b"=");
        match *value {
            QueryValue::Literal(value) => out(value.as_bytes()),
            QueryValue::Encoded(value) => {
                for part in crate::path::encode_query_value(value) {
                    out(part);
                }
            }
            QueryValue::Number(value) => out(U64Decimal::new(value.into()).as_bytes()),
            QueryValue::Words(words) => {
                for (index, word) in words.iter().enumerate() {
                    if index != 0 {
                        out(b",");
                    }
                    out(word.as_bytes());
                }
            }
        }
    }
}

// Writes the query as a URL ends with it: nothing if `query` holds no
// parameter, and otherwise a `?` and the parameters.
pub(crate) fn write_in_url(out: &mut dyn FnMut(&[u8]), query: &[Parameter<'_>]) {
    if query.iter().any(Option::is_some) {
        out(b"?");
        write(out, query);
    }
}
