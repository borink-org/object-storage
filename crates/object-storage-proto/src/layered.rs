//! Helpers built on the public API.
//!
//! Each function here uses only the public types, so you can write your own
//! version if you need different behaviour.

use crate::azure::{BlockRef, PhysicalCommitBlocks, PhysicalListBlocks, PhysicalStageBlock};
use crate::{
    Blobs, Error, Payload, PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut, RequestSize,
    Result, Timestamps,
};

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

/// Returns the byte and header-slot capacities that [`Blobs::encode_get`]
/// needs for this plan.
///
/// Call this to size a buffer before you encode; the answer is exact.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if `get` cannot become an Azure request,
/// unchanged from [`Blobs::encode_get`].
pub fn get_requirements(
    blobs: &Blobs<'_>,
    get: &PhysicalGet<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(blobs.encode_get(&mut [], &mut [], get, now).map(drop))
}

/// Returns the byte and header-slot capacities that [`Blobs::encode_put`]
/// needs for this plan.
///
/// Call this to size a buffer before you encode. The answer covers the request
/// head only, and never the content. Only the length of `content` reaches the
/// head, so a [`Payload::Streamed`] sizes a buffer without the bytes.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if `put` cannot become an Azure request,
/// unchanged from [`Blobs::encode_put`].
pub fn put_requirements(
    blobs: &Blobs<'_>,
    put: &PhysicalPut<'_>,
    content: Payload<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    // The head states how long the content is, so the requirement depends on
    // the length of `content`. Its bytes are never read.
    required(
        blobs
            .encode_put(&mut [], &mut [], put, content, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that [`Blobs::encode_delete`]
/// needs for this plan.
///
/// Call this to size a buffer before you encode; the answer is exact.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if `delete` cannot become an Azure request,
/// unchanged from [`Blobs::encode_delete`].
pub fn delete_requirements(
    blobs: &Blobs<'_>,
    delete: &PhysicalDelete<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(blobs.encode_delete(&mut [], &mut [], delete, now).map(drop))
}

/// Returns the byte and header-slot capacities that [`Blobs::encode_list`]
/// needs for this plan.
///
/// Call this to size a buffer before you encode; the answer is exact.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if `list` cannot become an Azure request,
/// unchanged from [`Blobs::encode_list`].
pub fn list_requirements(
    blobs: &Blobs<'_>,
    list: &PhysicalList<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(blobs.encode_list(&mut [], &mut [], list, now).map(drop))
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_stage_block`] needs for this plan.
///
/// As [`put_requirements`]: the answer covers the head, and only the length
/// of `content` reaches it.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_stage_block`].
pub fn stage_block_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalStageBlock<'_>,
    content: Payload<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_stage_block(&mut [], &mut [], plan, content, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_commit_blocks`] needs for this plan.
///
/// Call this to size a buffer before you encode; the answer is exact, and
/// the bytes include the XML body that the request carries.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_commit_blocks`].
pub fn commit_blocks_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalCommitBlocks<'_>,
    blocks: &[BlockRef<'_>],
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_commit_blocks(&mut [], &mut [], plan, blocks, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_list_blocks`] needs for this plan.
///
/// Call this to size a buffer before you encode; the answer is exact.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_list_blocks`].
pub fn list_blocks_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalListBlocks<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_list_blocks(&mut [], &mut [], plan, now)
            .map(drop),
    )
}

/// The most blocks that a Get Block List body of `len` bytes can hold.
///
/// Use it to size the array for [`Blobs::fill_blocks`] from the
/// `expected_len` of
/// [`ListBlocksHeadOutcome::Blocks`](crate::ListBlocksHeadOutcome::Blocks).
/// The smallest element the reader accepts is
/// `<Block><Name>x</Name><Size>0</Size></Block>`, 43 bytes: it requires a
/// name and a size, and does not check that the name is base64, because the
/// service decided what it stored.
pub const fn max_blocks_in(len: usize) -> usize {
    len / 43
}

/// Writes the block ID for `bytes` in the base64 form that Azure stores.
///
/// A block ID is base64 text on the wire, of at most 64 decoded bytes, and
/// every block of one blob must decode to the same length. Choose the bytes
/// however you number blocks, keep them the same length within one blob, and
/// pass what this returns as [`BlockRef::id`] or
/// [`PhysicalStageBlock::id`].
///
/// Copies the encoding into `into` and returns it. Returns [`None`] if
/// `bytes` is empty or longer than 64 bytes, or if `into` is shorter than
/// the encoding, which is four characters for every three bytes, rounded up.
pub fn block_id<'a>(bytes: &[u8], into: &'a mut [u8]) -> Option<&'a str> {
    if bytes.is_empty() || bytes.len() > 64 {
        return None;
    }
    let into = into.get_mut(..bytes.len().div_ceil(3) * 4)?;
    for (group, out) in bytes.chunks(3).zip(into.chunks_mut(4)) {
        // A group is 1 to 3 bytes; the missing ones read as zero and are
        // written as padding below. Each sextet index is at most 63.
        let bits = (u32::from(group[0]) << 16)
            | (u32::from(*group.get(1).unwrap_or(&0)) << 8)
            | u32::from(*group.get(2).unwrap_or(&0));
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = if i <= group.len() {
                BASE64[((bits >> (18 - 6 * i)) & 63) as usize]
            } else {
                b'='
            };
        }
    }
    core::str::from_utf8(into).ok()
}

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
/// [`PhysicalGet::condition_value`](crate::PhysicalGet::condition_value).
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

fn required(result: Result<()>) -> Result<RequestSize> {
    match result {
        Ok(()) => Ok(RequestSize::default()),
        Err(Error::Capacity(error)) => Ok(RequestSize {
            bytes: error.required,
            headers: error.required_headers,
        }),
        Err(error) => Err(error),
    }
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
pub fn decode_into<'a>(value: &[u8], into: &'a mut [u8]) -> Option<&'a [u8]> {
    let into = into.get_mut(..value.len())?;
    into.copy_from_slice(value);
    let len = crate::xml::decode_text(into, false).ok()?;
    Some(&into[..len])
}

/// Reads an HTTP date as milliseconds since the Unix epoch.
///
/// Use this on [`ObjectMeta::last_modified`], which holds the bytes that Azure
/// sent. Returns [`None`] if `value` is not an RFC 1123 date.
///
/// [`ObjectMeta::last_modified`]: crate::ObjectMeta::last_modified
pub fn http_date_ms(value: &[u8]) -> Option<u64> {
    // RFC 1123 `Www, DD Mon YYYY HH:MM:SS GMT`, the only form these services
    // send and the only one this crate writes.
    if value.len() != 29
        || value[3] != b','
        || value[4] != b' '
        || value[7] != b' '
        || value[11] != b' '
        || value[16] != b' '
        || value[19] != b':'
        || value[22] != b':'
        || &value[25..] != b" GMT"
    {
        return None;
    }
    let day = number(&value[5..7])?;
    let month = MONTHS
        .iter()
        .position(|name| name.as_slice() == &value[8..11])? as u64
        + 1;
    let year = number(&value[12..16])? as i64;
    let hour = number(&value[17..19])?;
    let minute = number(&value[20..22])?;
    let second = number(&value[23..25])?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let seconds = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add((hour * 3600 + minute * 60 + second) as i64)?;
    u64::try_from(seconds).ok()?.checked_mul(1000)
}

fn number(bytes: &[u8]) -> Option<u64> {
    // http_date_ms passes only two- or four-byte fields, so the sum is <= 9999.
    bytes.iter().try_fold(0, |value, byte| {
        byte.checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .map(|digit| value * 10 + digit as u64)
    })
}

// Howard Hinnant's `days_from_civil`, the inverse of the conversion in `time`.
// Called only with a four-digit year, month 1..=12 and day 1..=31; all
// intermediates fit i64, including the adjusted year -1 for January of year 0.
fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::{block_id, http_date_ms, quoted_etag};

    #[test]
    fn writes_the_base64_of_chosen_bytes() {
        let mut into = [0; 12];
        assert_eq!(block_id(&[0, 0, 0, 0], &mut into), Some("AAAAAA=="));
        assert_eq!(block_id(&[0, 0, 0, 1], &mut into), Some("AAAAAQ=="));
        assert_eq!(block_id(b"ab", &mut into), Some("YWI="));
        assert_eq!(block_id(b"abc", &mut into), Some("YWJj"));
        assert_eq!(block_id(&[0xfb, 0xff], &mut into), Some("+/8="));
        assert_eq!(block_id(b"", &mut into), None);
        assert_eq!(block_id(&[0; 65], &mut [0; 88]), None);
        assert_eq!(block_id(b"abc", &mut [0; 3]), None);
    }

    #[test]
    fn reads_an_azure_last_modified_header() {
        assert_eq!(
            http_date_ms(b"Fri, 24 May 2013 00:00:00 GMT"),
            Some(1_369_353_600_000)
        );
        assert_eq!(http_date_ms(b"not an HTTP date"), None);
        assert_eq!(http_date_ms(b"Fri, 24 Xxx 2013 00:00:00 GMT"), None);
    }

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
