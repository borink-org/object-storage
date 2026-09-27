//! What a listing call shares between the providers: the plan it asks for,
//! the page a provider reads, and the result it reports. Each provider reads
//! one page with its own client, and passes that read to [`list_page`] or
//! [`list_all_keys`].

use crate::{AdapterError, optional_text, successful_result, unsupported_by_crate};
use borink_object_storage_proto::{
    EntryKind, ListEntry, ListInclude, Listing, PhysicalList, layered,
};
use serde_json::{Value, json};

/// One page, as the result reports it.
pub(crate) struct ListedPage {
    entries: Vec<Value>,
    prefixes: Vec<String>,
    next_marker: Option<String>,
}

/// An entry that a fill wrote: a [`ListEntry`], or one with the values of
/// properties read in the same pass.
pub(crate) trait Listed {
    fn entry(&self) -> &ListEntry<'_>;
}

impl Listed for ListEntry<'_> {
    fn entry(&self) -> &ListEntry<'_> {
        self
    }
}

impl ListedPage {
    /// Splits the entries that a fill wrote into objects and groups of keys,
    /// writing each object with `object_value`.
    pub(crate) fn read<T: Listed>(
        slots: &[T],
        listing: Listing<'_>,
        object_value: impl Fn(&T) -> Value,
    ) -> Self {
        let mut entries = Vec::new();
        let mut prefixes = Vec::new();
        for slot in &slots[..listing.filled] {
            match slot.entry().kind {
                EntryKind::Prefix => prefixes.push(slot.entry().key.to_owned()),
                _ => entries.push(object_value(slot)),
            }
        }
        Self {
            entries,
            prefixes,
            next_marker: listing
                .next_marker
                .filter(|marker| !marker.is_empty())
                .map(str::to_owned),
        }
    }
}

/// The decoded text of a listed value, such as a metadata name or value.
pub(crate) fn decoded_listing_text(raw_value: &[u8]) -> String {
    let mut decoded = vec![0; raw_value.len()];
    match layered::decode_into(raw_value, &mut decoded) {
        Some(text) => String::from_utf8_lossy(text).into_owned(),
        None => String::from_utf8_lossy(raw_value).into_owned(),
    }
}

/// A page read, or the result to report instead of one.
pub(crate) type PageRead = Result<Result<ListedPage, Value>, AdapterError>;

/// An array of `max_results` entries always holds a whole page, and one of
/// the service's maximum does when the plan names none.
pub(crate) fn entry_slots<T: Clone + Default>(plan: &PhysicalList<'_>, maximum: usize) -> Vec<T> {
    let count = plan
        .max_results
        .map_or(maximum, |max_results| max_results as usize);
    vec![T::default(); count]
}

fn requested_page_size(call: &Value) -> Option<u32> {
    call.get("page_size")
        .and_then(Value::as_u64)
        .map(|page_size| page_size as u32)
}

/// Performs a `list_page` call with `read_page`.
pub(crate) fn list_page(
    call: &Value,
    read_page: impl FnOnce(&PhysicalList<'_>) -> PageRead,
) -> Result<Value, AdapterError> {
    let mut include = ListInclude::default();
    for include_option in call
        .get("include")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match include_option.as_str() {
            Some("metadata") => include = include | ListInclude::METADATA,
            _ => return Ok(unsupported_by_crate("ListInclude names metadata only")),
        }
    }
    if call.get("fetch_owner").and_then(Value::as_bool) == Some(true) {
        include = include | ListInclude::OWNER;
    }
    let delimited = match optional_text(call, "delimiter") {
        None => false,
        Some("/") => true,
        Some(_) => return Ok(unsupported_by_crate("PhysicalList delimits on '/' only")),
    };

    let list_plan = PhysicalList {
        prefix: optional_text(call, "prefix").unwrap_or_default(),
        marker: optional_text(call, "continuation_token"),
        start_after: optional_text(call, "start_after"),
        delimited,
        max_results: requested_page_size(call),
        include,
    };
    Ok(match read_page(&list_plan)? {
        Ok(page) => successful_result(json!({
            "entries": page.entries,
            "prefixes": page.prefixes,
            "continuation_token": page.next_marker.unwrap_or_default(),
        })),
        Err(result) => result,
    })
}

/// Performs a `list` call with `read_page`, one page after another.
pub(crate) fn list_all_keys(
    call: &Value,
    mut read_page: impl FnMut(&PhysicalList<'_>) -> PageRead,
) -> Result<Value, AdapterError> {
    let prefix = optional_text(call, "prefix").unwrap_or_default();
    let mut keys = Vec::new();
    let mut marker: Option<String> = None;
    loop {
        let list_plan = PhysicalList {
            marker: marker.as_deref(),
            max_results: requested_page_size(call),
            ..PhysicalList::new(prefix)
        };
        let page = match read_page(&list_plan)? {
            Ok(page) => page,
            Err(result) => return Ok(result),
        };
        keys.extend(page.entries.into_iter().map(|entry| entry["key"].clone()));
        match page.next_marker {
            Some(next_marker) => marker = Some(next_marker),
            None => break,
        }
    }
    Ok(successful_result(json!({"keys": keys})))
}
