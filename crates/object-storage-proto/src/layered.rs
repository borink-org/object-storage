//! Helpers built on the public API.
//!
//! Each function here uses only the public types, so you can write your own
//! version if you need different behaviour.
//!
//! The `*_requirements` functions encode the request into an empty buffer
//! and read the capacities from the refusal. They allocate nothing.

use crate::azure::{
    BlockRef, PhysicalListBlocks, PhysicalSnapshot, PhysicalStageBlock, PhysicalStageBlockFromUrl,
};
use crate::{
    Blobs, Error, Payload, PhysicalCommit, PhysicalCopy, PhysicalDelete, PhysicalDeleteMany,
    PhysicalGet, PhysicalList, PhysicalPut, PhysicalRestore, PhysicalSetTags, RequestSize, Result,
    Revision, Timestamps,
};

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
/// unchanged from [`Blobs::encode_get`], which reports it again.
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
/// unchanged from [`Blobs::encode_put`], which reports it again.
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
/// unchanged from [`Blobs::encode_delete`], which reports it again.
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
/// unchanged from [`Blobs::encode_list`], which reports it again.
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
/// unchanged from [`Blobs::encode_stage_block`], which reports it again.
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
/// unchanged from [`Blobs::encode_commit_blocks`], which reports it again.
pub fn commit_blocks_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalCommit<'_>,
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
/// [`Blobs::encode_set_tier`] needs.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the request cannot become an Azure
/// request, unchanged from [`Blobs::encode_set_tier`], which reports it again.
pub fn set_tier_requirements(
    blobs: &Blobs<'_>,
    key: &str,
    tier: &str,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_set_tier(&mut [], &mut [], key, tier, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_copy`] needs for this plan.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_copy`], which reports it again.
pub fn copy_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalCopy<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(blobs.encode_copy(&mut [], &mut [], plan, now).map(drop))
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_copy_from_url`] needs for this plan.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_copy_from_url`], which reports it again.
pub fn copy_from_url_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalCopy<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_copy_from_url(&mut [], &mut [], plan, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_put_from_url`] needs for this plan.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_put_from_url`], which reports it again.
pub fn put_from_url_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalCopy<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_put_from_url(&mut [], &mut [], plan, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_stage_block_from_url`] needs for this plan.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_stage_block_from_url`], which reports it again.
pub fn stage_block_from_url_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalStageBlockFromUrl<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_stage_block_from_url(&mut [], &mut [], plan, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_abort_copy`] needs for this plan.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_abort_copy`], which reports it again.
pub fn abort_copy_requirements(
    blobs: &Blobs<'_>,
    key: &str,
    copy_id: &str,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_abort_copy(&mut [], &mut [], key, copy_id, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_restore`] needs for this plan.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_restore`], which reports it again.
pub fn restore_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalRestore<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(blobs.encode_restore(&mut [], &mut [], plan, now).map(drop))
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_snapshot`] needs for this plan.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_snapshot`], which reports it again.
pub fn snapshot_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalSnapshot<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(blobs.encode_snapshot(&mut [], &mut [], plan, now).map(drop))
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_set_tags`] needs, the body included.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_set_tags`], which reports it again.
pub fn set_tags_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalSetTags<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(blobs.encode_set_tags(&mut [], &mut [], plan, now).map(drop))
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_delete_many`] needs, the body included.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the plan cannot become an Azure request,
/// unchanged from [`Blobs::encode_delete_many`], which reports it again.
pub fn delete_many_requirements(
    blobs: &Blobs<'_>,
    plan: &PhysicalDeleteMany<'_>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_delete_many(&mut [], &mut [], plan, now)
            .map(drop),
    )
}

/// Returns the byte and header-slot capacities that
/// [`Blobs::encode_get_tags`] needs.
///
/// # Errors
///
/// Returns [`Error::InvalidPlan`] if the request cannot become an Azure
/// request, unchanged from [`Blobs::encode_get_tags`], which reports it again.
pub fn get_tags_requirements(
    blobs: &Blobs<'_>,
    key: &str,
    revision: Option<Revision<'_>>,
    now: &Timestamps,
) -> Result<RequestSize> {
    required(
        blobs
            .encode_get_tags(&mut [], &mut [], key, revision, now)
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
/// unchanged from [`Blobs::encode_list_blocks`], which reports it again.
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
/// [`ListPartsHeadOutcome::Parts`](crate::ListPartsHeadOutcome::Parts).
/// The smallest element that the reader accepts is
/// `<Block><Name>x</Name><Size>0</Size></Block>`, 43 bytes. The reader
/// requires a name and a size, and does not check that the name is base64.
pub const fn max_blocks_in(len: usize) -> usize {
    len / 43
}

/// Writes the block ID for `bytes` in the base64 form that Azure stores.
///
/// A block ID is base64 text on the wire, and decodes to at most 64 bytes.
/// Every block of one blob must decode to the same length. Choose the bytes
/// however you number blocks, and pass what this returns as [`BlockRef::id`]
/// or [`PhysicalStageBlock::id`].
///
/// Copies the encoding into `into` and returns it. The encoding is four
/// characters for every three bytes, rounded up, so 88 bytes of `into` fit
/// every ID. Returns [`None`] if `bytes` is empty or longer than 64 bytes,
/// or if `into` is shorter than the encoding.
pub fn block_id<'a>(bytes: &[u8], into: &'a mut [u8]) -> Option<&'a str> {
    if bytes.is_empty() || bytes.len() > 64 {
        return None;
    }
    let into = into.get_mut(..bytes.len().div_ceil(3) * 4)?;
    Some(crate::encoding::base64_into(bytes, into))
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
/// Use this on [`ObjectMeta::last_modified`], and on
/// [`ListEntry::last_modified`] of an Azure listing. Returns [`None`] if
/// `value` is not an RFC 1123 date.
///
/// [`ObjectMeta::last_modified`]: crate::ObjectMeta::last_modified
/// [`ListEntry::last_modified`]: crate::ListEntry::last_modified
pub fn http_date_ms(value: &str) -> Option<u64> {
    // RFC 1123 `Www, DD Mon YYYY HH:MM:SS GMT`, the only form these services
    // send and the only one this crate writes.
    let value = value.as_bytes();
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

/// Reads an ISO 8601 date as milliseconds since the Unix epoch.
///
/// Use this on [`ListEntry::last_modified`] of an S3 listing, which is
/// written as `2026-08-22T12:01:01.000Z`. The fraction of a second is
/// optional and read to the millisecond. Returns [`None`] if `value` is not
/// such a date in UTC.
///
/// [`ListEntry::last_modified`]: crate::ListEntry::last_modified
pub fn iso8601_ms(value: &str) -> Option<u64> {
    let value = value.as_bytes();
    if value.len() < 20
        || value[4] != b'-'
        || value[7] != b'-'
        || value[10] != b'T'
        || value[13] != b':'
        || value[16] != b':'
        || value.last() != Some(&b'Z')
    {
        return None;
    }
    let year = number(&value[..4])? as i64;
    let month = number(&value[5..7])?;
    let day = number(&value[8..10])?;
    let hour = number(&value[11..13])?;
    let minute = number(&value[14..16])?;
    let second = number(&value[17..19])?;
    // The fraction, if any, between the seconds and the `Z`.
    let millis = match &value[19..value.len() - 1] {
        [] => 0,
        [b'.', digits @ ..] if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) => {
            // Three digits are milliseconds; fewer are padded and more cut.
            digits
                .iter()
                .chain(core::iter::repeat(&b'0'))
                .take(3)
                .fold(0, |value, digit| value * 10 + (digit - b'0') as u64)
        }
        _ => return None,
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let seconds = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add((hour * 3600 + minute * 60 + second) as i64)?;
    u64::try_from(seconds)
        .ok()?
        .checked_mul(1000)?
        .checked_add(millis)
}

fn number(bytes: &[u8]) -> Option<u64> {
    // The date readers pass only two- or four-byte fields, so the sum is <= 9999.
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

/// The `*_requirements` functions of an S3 client.
pub mod s3 {
    use super::required;
    use crate::s3::{
        Objects, PartRef, PayloadHash, PhysicalAbortUpload, PhysicalCreateUpload,
        PhysicalListParts, PhysicalStagePart, PhysicalStagePartCopy,
    };
    use crate::{
        Payload, PhysicalCommit, PhysicalCopy, PhysicalDelete, PhysicalDeleteMany, PhysicalGet,
        PhysicalList, PhysicalPut, PhysicalRestore, PhysicalSetTags, RequestSize, Result, Revision,
        Timestamps,
    };

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_restore`] needs for this plan, the body included.
    ///
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_restore`], which reports it again.
    pub fn restore_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalRestore<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_restore(&mut [], &mut [], plan, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_copy`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, unchanged from [`Objects::encode_copy`],
    /// which reports it again.
    pub fn copy_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalCopy<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(objects.encode_copy(&mut [], &mut [], plan, now).map(drop))
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_stage_part_copy`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_stage_part_copy`], which reports it again.
    pub fn stage_part_copy_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalStagePartCopy<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_stage_part_copy(&mut [], &mut [], plan, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_get`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `get`
    /// cannot become an S3 request, unchanged from [`Objects::encode_get`],
    /// which reports it again.
    pub fn get_requirements(
        objects: &Objects<'_>,
        get: &PhysicalGet<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(objects.encode_get(&mut [], &mut [], get, now).map(drop))
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_put`] needs for this plan.
    ///
    /// The answer covers the request head only, and never the content. This
    /// function reads no byte of `content` and computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `put`
    /// cannot become an S3 request, unchanged from [`Objects::encode_put`],
    /// which reports it again.
    pub fn put_requirements(
        objects: &Objects<'_>,
        put: &PhysicalPut<'_>,
        content: Payload<'_>,
        hash: PayloadHash,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_put(&mut [], &mut [], put, content, hash, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_delete`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `delete`
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_delete`], which reports it again.
    pub fn delete_requirements(
        objects: &Objects<'_>,
        delete: &PhysicalDelete<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_delete(&mut [], &mut [], delete, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_list`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `list`
    /// cannot become an S3 request, unchanged from [`Objects::encode_list`],
    /// which reports it again.
    pub fn list_requirements(
        objects: &Objects<'_>,
        list: &PhysicalList<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(objects.encode_list(&mut [], &mut [], list, now).map(drop))
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_create_session`] needs.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if the
    /// client cannot create a session, unchanged from
    /// [`Objects::encode_create_session`], which reports it again.
    pub fn create_session_requirements(
        objects: &Objects<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_create_session(&mut [], &mut [], now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_create_upload`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_create_upload`], which reports it again.
    pub fn create_upload_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalCreateUpload<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_create_upload(&mut [], &mut [], plan, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_stage_part`] needs for this plan.
    ///
    /// As [`put_requirements`]: the answer covers the head, and this
    /// function reads no byte of `content` and computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_stage_part`], which reports it again.
    pub fn stage_part_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalStagePart<'_>,
        content: Payload<'_>,
        hash: PayloadHash,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_stage_part(&mut [], &mut [], plan, content, hash, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_commit_parts`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact,
    /// and the bytes include the XML body that the request carries. This
    /// function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if the
    /// commit cannot become an S3 request, unchanged from
    /// [`Objects::encode_commit_parts`], which reports it again.
    pub fn commit_parts_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalCommit<'_>,
        upload_id: &str,
        parts: &[PartRef<'_>],
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_commit_parts(&mut [], &mut [], plan, upload_id, parts, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_abort_upload`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_abort_upload`], which reports it again.
    pub fn abort_upload_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalAbortUpload<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_abort_upload(&mut [], &mut [], plan, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_list_parts`] needs for this plan.
    ///
    /// Call this to size a buffer before you encode; the answer is exact.
    /// This function computes no signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if `plan`
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_list_parts`], which reports it again.
    pub fn list_parts_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalListParts<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_list_parts(&mut [], &mut [], plan, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_put_tagging`] needs, the body included.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if the plan
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_put_tagging`], which reports it again.
    pub fn put_tagging_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalSetTags<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_put_tagging(&mut [], &mut [], plan, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_delete_many`] needs, the body included.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if the plan
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_delete_many`], which reports it again.
    pub fn delete_many_requirements(
        objects: &Objects<'_>,
        plan: &PhysicalDeleteMany<'_>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_delete_many(&mut [], &mut [], plan, now)
                .map(drop),
        )
    }

    /// Returns the byte and header-slot capacities that
    /// [`Objects::encode_get_tagging`] needs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`](crate::Error::InvalidPlan) if the key
    /// cannot become an S3 request, unchanged from
    /// [`Objects::encode_get_tagging`], which reports it again.
    pub fn get_tagging_requirements(
        objects: &Objects<'_>,
        key: &str,
        revision: Option<Revision<'_>>,
        now: &Timestamps,
    ) -> Result<RequestSize> {
        required(
            objects
                .encode_get_tagging(&mut [], &mut [], key, revision, now)
                .map(drop),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{block_id, http_date_ms, iso8601_ms, quoted_etag};

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
            http_date_ms("Fri, 24 May 2013 00:00:00 GMT"),
            Some(1_369_353_600_000)
        );
        assert_eq!(http_date_ms("not an HTTP date"), None);
        assert_eq!(http_date_ms("Fri, 24 Xxx 2013 00:00:00 GMT"), None);
    }

    #[test]
    fn reads_an_s3_listing_date() {
        assert_eq!(
            iso8601_ms("2013-05-24T00:00:00.000Z"),
            Some(1_369_353_600_000)
        );
        assert_eq!(
            iso8601_ms("2013-05-24T00:00:01.5Z"),
            Some(1_369_353_601_500)
        );
        assert_eq!(iso8601_ms("2013-05-24T00:00:01Z"), Some(1_369_353_601_000));
        for value in [
            "2013-05-24T00:00:00.000",
            "2013-05-24 00:00:00Z",
            "2013-05-24T00:00:00.Z",
            "2013-13-24T00:00:00Z",
            "Fri, 24 May 2013 00:00:00 GMT",
        ] {
            assert_eq!(iso8601_ms(value), None, "{value}");
        }
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
