// Reading the values of a listing.

/// Writes an entity tag from a listing in the quoted form that HTTP defines.
///
/// A listing writes an entity tag without the quotes that the `ETag` header
/// carries. Azure conditions a request on either form; this writes the quoted
/// one.
///
/// Copies `listed` into `into`, adding the quotes unless it already carries
/// them or is a weak tag, and returns what it wrote. Returns [`None`] if
/// `into` is too small; it needs at most two bytes more than `listed`.
///
/// Use it on [`ListEntry::e_tag`](crate::ListEntry::e_tag) to turn an entry of
/// a listing into a
/// [`GetItem::condition_value`](crate::GetItem::condition_value).
pub fn quoted_etag<'a>(listed: &[u8], into: &'a mut [u8]) -> Option<&'a [u8]> {
    let quoted = listed.starts_with(b"\"") && listed.ends_with(b"\"") && listed.len() >= 2;
    if quoted || listed.starts_with(b"W/") {
        let into = into.get_mut(..listed.len())?;
        into.copy_from_slice(listed);
        return Some(into);
    }
    // A byte slice is at most isize::MAX bytes, leaving usize room for two quotes.
    let into = into.get_mut(..listed.len() + 2)?;
    into[0] = b'"';
    into[1..listed.len() + 1].copy_from_slice(listed);
    into[listed.len() + 1] = b'"';
    Some(into)
}

/// Writes the text of a listed value with its references resolved.
///
/// Use this on a value that
/// [`ListEntry::property`](crate::ListEntry::property) returned, which holds
/// the bytes that the service wrote. XML writes an `&` as `&amp;`, and a
/// character that the document cannot carry as `&#233;`. This writes what
/// those stand for.
///
/// Copies `value` into `into` and returns what it wrote, which is never longer
/// than `value`. Returns [`None`] if `into` is shorter than `value`, and for a
/// reference that no listing declares.
///
/// This undoes XML references and nothing else. It does not percent-decode.
/// Azure escapes the metadata it returns for XML but never percent-encodes
/// it, so `already%80escaped` is the text itself and not an escape. Only a
/// name that the service marked as encoded is percent-decoded, and reading
/// the page did that already.
///
/// The fields of a [`ListEntry`](crate::ListEntry) do not need this. Reading
/// the page decoded them in place.
pub fn decode_property<'a>(value: &[u8], into: &'a mut [u8]) -> Option<&'a [u8]> {
    let into = into.get_mut(..value.len())?;
    into.copy_from_slice(value);
    let len = crate::xml::decode_text(into, false).ok()?;
    Some(&into[..len])
}

#[cfg(test)]
mod tests {
    use super::quoted_etag;

    #[test]
    fn quotes_a_listed_etag_once() {
        let mut into = [0; 32];
        assert_eq!(
            quoted_etag(b"0x8DF0046E8E555AF", &mut into),
            Some(b"\"0x8DF0046E8E555AF\"".as_slice())
        );
        assert_eq!(
            quoted_etag(b"\"already\"", &mut into),
            Some(b"\"already\"".as_slice())
        );
        assert_eq!(
            quoted_etag(b"W/\"weak\"", &mut into),
            Some(b"W/\"weak\"".as_slice())
        );
        assert_eq!(quoted_etag(b"0x8DF", &mut [0; 6]), None);
    }
}
