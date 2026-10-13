//! The borink-org/object-tests adapter for `borink-object-storage-proto`, the
//! sans-IO Azure Blob and S3 crate of this repository. The grader starts it
//! once per case and writes one message to its standard input. The crate
//! encodes each request into buffers that this adapter sizes, and reads the
//! response head and body that it hands back. `ureq` sends the requests, as
//! in the crate's own ureq host.
//!
//! This file reads the message, sends the requests and writes the result.
//! `azure.rs` and `s3.rs` map the calls of each provider onto the crate.

use base64::{Engine, engine::general_purpose::STANDARD};
use borink_object_storage_crypto::sigv4::{hmac, sha256};
use borink_object_storage_crypto::{Checksum, Crc64, Md5RustCrypto, Sha256RustCrypto};
use borink_object_storage_proto::{
    BodyWindow, ChecksumKind, ChecksumValue, ConditionKind, ConditionValue, ContentProperties,
    CopyItem, CopyOptions, DeleteKind, DeleteOptions, DeleteTarget, Error as CrateError, GetItem,
    GetKind, GetOptions, MetadataPair, ObjectItem, ObjectMeta, Payload, Ranging, RequestedRange,
    ResponseHead, RestoreHeadOutcome, RestoreOptions, RestorePriority, Revision, RevisionKind,
    ServiceErrorKind, Tag, Timestamps, WireRequest, WriteOptions,
};
use serde_json::{Value, json};
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

type AdapterError = Box<dyn std::error::Error>;

/// The version of the object-tests protocol this adapter speaks. The grader
/// sends its own in every message, and a different one is refused.
const PROTOCOL_VERSION: u64 = 1;

const MAX_RESPONSE_BODY_BYTES: u64 = 64 * 1024 * 1024;

/// One HTTP exchange, held whole so the crate can borrow its head and body.
struct HttpExchange {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
    /// The length of a successful body read with [`BodyRead::Sink`], which
    /// leaves `body` empty.
    sunk_length: Option<u64>,
}

/// How the adapter reads the body of a successful response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyRead<'a> {
    /// Holds the body for the crate to read.
    Hold,
    /// Copies the body to the grader's socket at this address, for a call
    /// with `body_sink` that reads more than the adapter holds.
    Sink(&'a str),
}

impl<'a> BodyRead<'a> {
    fn of(call: &'a Value) -> Self {
        match optional_text(call, "body_sink") {
            Some(address) => Self::Sink(address),
            None => Self::Hold,
        }
    }
}

/// The body of a write: bytes that the call holds, or a pattern repeated to
/// a length, which the adapter generates as it sends the request.
enum CallBody {
    Held(Vec<u8>),
    Generated { pattern: Vec<u8>, length: u64 },
}

impl CallBody {
    fn of(call: &Value) -> Result<Self, AdapterError> {
        let Some(body) = call.get("body") else {
            return Ok(Self::Held(decode_base64_field(call, "body_base64")?));
        };
        if optional_text(body, "encoding") != Some("repeat") {
            return Err("unknown body encoding".into());
        }
        let data = body.get("data").ok_or("missing body data")?;
        let pattern = decode_base64_field(data, "pattern_base64")?;
        let length = data
            .get("length")
            .and_then(Value::as_u64)
            .ok_or("missing body length")?;
        if pattern.is_empty() && length > 0 {
            return Err("empty body pattern".into());
        }
        Ok(Self::Generated { pattern, length })
    }

    fn payload(&self) -> Payload<'_> {
        match self {
            Self::Held(bytes) => Payload::Slice(bytes),
            Self::Generated { length, .. } => Payload::Streamed { len: *length },
        }
    }
}

/// Reads a pattern repeated to a length, from a block of whole repeats so
/// that each read copies once.
struct Repeat {
    block: Vec<u8>,
    period: usize,
    offset: usize,
    remaining: u64,
}

impl Repeat {
    fn new(pattern: &[u8], length: u64) -> Self {
        let repeats = (64 * 1024) / pattern.len().max(1) + 2;
        Self {
            block: pattern.repeat(repeats),
            period: pattern.len(),
            offset: 0,
            remaining: length,
        }
    }
}

impl Read for Repeat {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let window = self.block.len() - self.offset;
        let count = out
            .len()
            .min(window)
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        out[..count].copy_from_slice(&self.block[self.offset..self.offset + count]);
        self.offset = (self.offset + count) % self.period.max(1);
        self.remaining -= count as u64;
        Ok(count)
    }
}

impl HttpExchange {
    fn response_head(&self) -> ResponseHead<'_> {
        head_of(self.status, &self.headers)
    }

    /// Returns the response head, and the body to decode in place, as two
    /// borrows that the crate can hold at once.
    fn head_and_body(&mut self) -> (ResponseHead<'_>, &mut [u8]) {
        (head_of(self.status, &self.headers), &mut self.body)
    }
}

fn head_of(status: u16, headers: &[(String, Vec<u8>)]) -> ResponseHead<'_> {
    ResponseHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_slice())),
    )
}

struct AdapterContext {
    http_agent: ureq::Agent,
    /// The origin of the endpoint's proxy, which takes a plain HTTP request
    /// as the host of its URL would, and the agent that sends it there.
    plain_proxy: Option<(String, ureq::Agent)>,
}

impl AdapterContext {
    /// An agent that hands every status back, follows no redirect, and goes
    /// through the endpoint's proxy if it names one.
    ///
    /// The grader names a proxy for an endpoint whose host it cannot serve,
    /// such as the zonal host of an S3 Express bucket. ureq would open a
    /// CONNECT tunnel to it, which the grader does not take, so a plain HTTP
    /// request goes to the proxy directly, with the `host` of its URL. A
    /// request over HTTPS goes through ureq's tunnel.
    fn for_endpoint(endpoint: &Value) -> Result<Self, AdapterError> {
        let agent_config = || {
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .max_redirects(0)
        };
        let Some(proxy_url) = optional_text(endpoint, "proxy_url") else {
            return Ok(Self {
                http_agent: agent_config().build().into(),
                plain_proxy: None,
            });
        };
        let proxy = ureq::Proxy::new(proxy_url)?;
        Ok(Self {
            http_agent: agent_config().proxy(Some(proxy)).build().into(),
            plain_proxy: Some((
                proxy_url.trim_end_matches('/').to_owned(),
                agent_config().build().into(),
            )),
        })
    }
}

fn error_kind_for_status(status: u16) -> &'static str {
    match status {
        404 => "not_found",
        412 => "precondition",
        304 => "not_modified",
        401 | 403 => "permission_denied",
        _ => "other",
    }
}

fn successful_result(value: Value) -> Value {
    json!({"outcome": "ok", "value": value})
}

fn unsupported_by_crate(reason: &str) -> Value {
    json!({"outcome": "unsupported", "scope": "sdk", "reason": reason})
}

fn unsupported_by_adapter(reason: &str) -> Value {
    json!({"outcome": "unsupported", "scope": "adapter", "reason": reason})
}

/// Writes `UnsupportedRange` as `unsupported_range`.
fn snake_case_name(debug_name: &str) -> String {
    let mut snake_case = String::new();
    for character in debug_name.chars() {
        if character.is_ascii_uppercase() {
            if !snake_case.is_empty() {
                snake_case.push('_');
            }
            snake_case.push(character.to_ascii_lowercase());
        } else {
            snake_case.push(character);
        }
    }
    snake_case
}

/// Returns a failed operation with `status`, and with the error code that
/// the service named, if it named one. `kind` is the error as the crate
/// classified it: a missing container is reported as such, never as a
/// missing object.
fn failed_result(status: u16, code: Option<&str>, kind: Option<ServiceErrorKind>) -> Value {
    let kind = match kind {
        Some(ServiceErrorKind::NoSuchContainer) => "container_not_found",
        _ => error_kind_for_status(status),
    };
    let mut result = json!({
        "outcome": "error",
        "status": status,
        "kind": kind,
    });
    if let Some(code) = code {
        result["code"] = json!(code);
    }
    result
}

/// The content properties that a write call names.
fn requested_properties(call: &Value) -> ContentProperties<'_> {
    ContentProperties {
        content_type: optional_text(call, "content_type"),
        content_encoding: optional_text(call, "content_encoding"),
        content_language: optional_text(call, "content_language"),
        content_disposition: optional_text(call, "content_disposition"),
        cache_control: optional_text(call, "cache_control"),
    }
}

/// The restore that a `restore` call names, or `None` for a priority this
/// adapter does not map.
/// The options and the object of the restore that a call names, with the
/// checksum of its body that S3 takes, or `None` for a priority that the
/// adapter does not map.
pub(crate) fn requested_restore_options(
    call: &Value,
    checksum: Option<ChecksumKind>,
) -> Option<(RestoreOptions<'_>, ObjectItem<'_>)> {
    let priority = match optional_text(call, "priority") {
        None | Some("standard") => RestorePriority::Standard,
        Some("high") => RestorePriority::High,
        Some("bulk") => RestorePriority::Bulk,
        Some(_) => return None,
    };
    let (revision, id) = requested_revision_parts(call);
    let options = RestoreOptions {
        priority,
        days: call
            .get("days")
            .and_then(Value::as_u64)
            .map(|days| u32::try_from(days).unwrap_or(u32::MAX)),
        tier: optional_text(call, "tier"),
        revision,
        checksum,
    };
    let item = ObjectItem {
        revision: id,
        ..ObjectItem::new(optional_text(call, "key").unwrap_or_default())
    };
    Some((options, item))
}

/// The result of a restore: whether it started, or the object was readable
/// already. `None` for an outcome that is a failure.
fn restore_value(outcome: &RestoreHeadOutcome<'_>) -> Option<Value> {
    match outcome {
        RestoreHeadOutcome::Started => Some(json!({"state": "started"})),
        RestoreHeadOutcome::Readable => Some(json!({"state": "readable"})),
        _ => None,
    }
}

/// The metadata pairs that a write call names.
fn requested_metadata(call: &Value) -> Vec<MetadataPair<'_>> {
    call.get("metadata")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, value)| MetadataPair {
            name,
            value: value.as_str().unwrap_or_default(),
        })
        .collect()
}

const SOURCE_CONDITION_FIELDS: [(&str, ConditionKind); 4] = [
    ("source_if_match", ConditionKind::IfMatch),
    ("source_if_none_match", ConditionKind::IfNoneMatch),
    ("source_if_modified_since", ConditionKind::IfModifiedSince),
    (
        "source_if_unmodified_since",
        ConditionKind::IfUnmodifiedSince,
    ),
];

/// The object that a copy call reads.
struct Source<'c> {
    /// The storage account that holds it, or `None` for the target's.
    endpoint: Option<&'c str>,
    /// Its container or bucket, or `None` for the target's.
    container: Option<&'c str>,
    /// Its key.
    key: &'c str,
    /// Its version, or `None` for the object as it is now.
    version: Option<&'c str>,
    /// The kind of the condition on it.
    condition: ConditionKind,
    /// What the condition compares against.
    condition_value: Option<ConditionValue<'c>>,
}

/// The object that a copy call reads, with `container_field` naming its
/// container or bucket, or `None` if the call names more than the one
/// condition on it that the crate takes.
fn requested_source<'c>(call: &'c Value, container_field: &str) -> Option<Source<'c>> {
    let mut given = SOURCE_CONDITION_FIELDS
        .iter()
        .filter_map(|(field, kind)| Some((*kind, optional_text(call, field)?)));
    let (condition, condition_value) = match (given.next(), given.next()) {
        (None, _) => (ConditionKind::None, None),
        (Some((kind, text)), None) => (kind, Some(condition_value(kind, text))),
        (Some(_), Some(_)) => return None,
    };
    Some(Source {
        endpoint: optional_text(call, "source_account_url"),
        container: optional_text(call, container_field),
        key: optional_text(call, "source_key").unwrap_or_default(),
        version: optional_text(call, "source_version"),
        condition,
        condition_value,
    })
}

/// The bytes of the source that a stage from a copy names.
/// The options and the item of the copy that a call names, or `None` if it
/// names more than one condition on the target or on the source. The
/// options borrow `metadata` and `tags`, which the caller read from the
/// call.
pub(crate) fn requested_copy<'c>(
    call: &'c Value,
    container_field: &str,
    class_field: &str,
    metadata: &'c [MetadataPair<'c>],
    tags: &'c [Tag<'c>],
) -> Result<Option<(CopyOptions<'c>, CopyItem<'c>)>, AdapterError> {
    let (Some((condition, condition_value)), Some(source)) = (
        requested_condition(call),
        requested_source(call, container_field),
    ) else {
        return Ok(None);
    };
    let source_range = if call.get("source_range").is_some() {
        requested_source_range(call)?
    } else {
        RequestedRange::Whole
    };
    let options = CopyOptions {
        source_endpoint: source.endpoint,
        source_container: source.container,
        source_revision: match source.version {
            Some(_) => RevisionKind::Version,
            None => RevisionKind::Current,
        },
        source_condition: source.condition,
        source_ranging: if source_range == RequestedRange::Whole {
            Ranging::Whole
        } else {
            Ranging::Ranged
        },
        condition,
        // A call that names no metadata or no tags keeps what the service
        // gives a copy.
        metadata: call.get("metadata").map(|_| metadata),
        tags: call.get("tags").map(|_| tags),
        options: WriteOptions {
            properties: requested_properties(call),
            storage_class: optional_text(call, class_field),
            ..WriteOptions::default()
        },
    };
    let item = CopyItem {
        key: optional_text(call, "key").unwrap_or_default(),
        source_key: source.key,
        source_revision: optional_text(call, "source_version"),
        condition_value,
        source_condition_value: source.condition_value,
        source_range,
    };
    Ok(Some((options, item)))
}

fn requested_source_range(call: &Value) -> Result<RequestedRange, AdapterError> {
    range_of(call.get("source_range"))
}

/// The snapshot or version that a read or a removal names.
fn requested_revision(call: &Value) -> Option<Revision<'_>> {
    optional_text(call, "snapshot")
        .map(Revision::Snapshot)
        .or_else(|| optional_text(call, "version").map(Revision::Version))
}

/// The kind of revision that a call names, and its identifier.
fn requested_revision_parts(call: &Value) -> (RevisionKind, Option<&str>) {
    match requested_revision(call) {
        Some(Revision::Snapshot(id)) => (RevisionKind::Snapshot, Some(id)),
        Some(Revision::Version(id)) => (RevisionKind::Version, Some(id)),
        _ => (RevisionKind::Current, None),
    }
}

/// The object that a call names, with the kinds of its condition and its
/// revision, or `None` if it names more than the one condition that the
/// crate takes.
pub(crate) fn requested_object(
    call: &Value,
) -> Option<(ConditionKind, RevisionKind, ObjectItem<'_>)> {
    let (condition, condition_value) = requested_condition(call)?;
    let (revision_kind, revision) = requested_revision_parts(call);
    let item = ObjectItem {
        key: optional_text(call, "key").unwrap_or_default(),
        condition_value,
        revision,
    };
    Some((condition, revision_kind, item))
}

/// The options and the item of the removal that a call names, or `None`
/// if it names more than the one condition that the crate takes.
pub(crate) fn requested_delete(
    call: &Value,
) -> Result<Option<(DeleteOptions, ObjectItem<'_>)>, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(None);
    };
    let kind = match optional_text(call, "snapshots") {
        None => DeleteKind::Object,
        Some("only") => DeleteKind::SnapshotsOnly,
        Some("include") => DeleteKind::ObjectAndSnapshots,
        Some(_) => return Err("unknown snapshots mode".into()),
    };
    let (revision_kind, revision) = requested_revision_parts(call);
    let options = DeleteOptions {
        kind,
        condition,
        revision: revision_kind,
    };
    let item = ObjectItem {
        key: optional_text(call, "key").unwrap_or_default(),
        condition_value,
        revision,
    };
    Ok(Some((options, item)))
}

/// The options and the item of the read that a call names, or `None` if it
/// names more than the one condition that the crate takes.
pub(crate) fn requested_get(
    call: &Value,
    kind: GetKind,
) -> Result<Option<(GetOptions, GetItem<'_>)>, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(None);
    };
    let range = if kind == GetKind::Head {
        RequestedRange::Whole
    } else {
        requested_range(call)?
    };
    let (revision_kind, revision) = requested_revision_parts(call);
    let options = GetOptions {
        kind,
        ranging: if range == RequestedRange::Whole {
            Ranging::Whole
        } else {
            Ranging::Ranged
        },
        condition,
        revision: revision_kind,
    };
    let item = GetItem {
        key: optional_text(call, "key").unwrap_or_default(),
        range,
        condition_value,
        revision,
    };
    Ok(Some((options, item)))
}

/// The tags that a call names: a list of keys and values, which may name a
/// key twice, or an object of them.
fn requested_tags(call: &Value) -> Vec<Tag<'_>> {
    match call.get("tags") {
        Some(Value::Array(tags)) => tags
            .iter()
            .map(|tag| Tag {
                key: optional_text(tag, "key").unwrap_or_default(),
                value: optional_text(tag, "value").unwrap_or_default(),
            })
            .collect(),
        Some(Value::Object(tags)) => tags
            .iter()
            .map(|(key, value)| Tag {
                key,
                value: value.as_str().unwrap_or_default(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The kind of checksum that a write call names, and its value if the call
/// gives one. `None` inside is an algorithm this adapter does not map.
pub(crate) fn requested_checksum_kind(
    call: &Value,
) -> Option<Option<(ChecksumKind, Option<&str>)>> {
    let algorithm = call
        .pointer("/checksum/algorithm")
        .and_then(Value::as_str)?;
    let value = call
        .pointer("/checksum/value_base64")
        .and_then(Value::as_str);
    let kind = match algorithm {
        "md5" => ChecksumKind::Md5,
        "azure_crc64" | "crc64nvme" => ChecksumKind::Crc64,
        "crc32" => ChecksumKind::Crc32,
        "crc32c" => ChecksumKind::Crc32c,
        "sha1" => ChecksumKind::Sha1,
        "sha256" => ChecksumKind::Sha256,
        _ => return Some(None),
    };
    Some(Some((kind, value)))
}

/// The checksum value of a write: the text the call gives, or the digest of
/// the body, which the crypto crate computes. The result to report is the
/// error for a generated body, which the adapter does not hold whole.
pub(crate) fn checksum_value<'a>(
    kind: ChecksumKind,
    text: Option<&'a str>,
    body: &CallBody,
) -> Result<ChecksumValue<'a>, Value> {
    if let Some(text) = text {
        return Ok(ChecksumValue::Base64(text));
    }
    let CallBody::Held(bytes) = body else {
        return Err(unsupported_by_adapter(
            "the adapter computes a checksum only over a body it holds",
        ));
    };
    Ok(ChecksumValue::Digest(digest_of(kind, bytes)))
}

/// The checksum of `kind` of `bytes`.
pub(crate) fn digest_of(kind: ChecksumKind, bytes: &[u8]) -> borink_object_storage_proto::Digest {
    fn digest<C: borink_object_storage_crypto::Checksum>(
        bytes: &[u8],
    ) -> borink_object_storage_proto::Digest {
        let mut checksum = C::default();
        checksum.update(bytes);
        checksum.finish()
    }
    use borink_object_storage_crypto::{
        Crc32, Crc32c, Crc64, Md5RustCrypto, Sha1RustCrypto, Sha256RustCrypto,
    };
    match kind {
        ChecksumKind::Md5 => digest::<Md5RustCrypto>(bytes),
        ChecksumKind::Crc64 => digest::<Crc64>(bytes),
        ChecksumKind::Crc32 => digest::<Crc32>(bytes),
        ChecksumKind::Crc32c => digest::<Crc32c>(bytes),
        ChecksumKind::Sha1 => digest::<Sha1RustCrypto>(bytes),
        _ => digest::<Sha256RustCrypto>(bytes),
    }
}

/// The refusal of a write that names two checksums: a request carries one, and
/// the services refuse a write with two as well.
fn two_checksums_refused() -> Value {
    json!({
        "outcome": "refused",
        "kind": "one_transactional_checksum",
        "parameter": "checksums",
    })
}

/// Adds the fields of a read that the response head states. Azure calls the
/// storage class an access tier, so `storage_class_field` names its field.
fn read_meta_fields(value: &mut Value, meta: &ObjectMeta<'_>, storage_class_field: &str) {
    if let Some(text) = text_of(meta.storage_class) {
        value[storage_class_field] = json!(text);
    }
    let fields = [
        ("content_type", meta.content_type),
        ("content_encoding", meta.content_encoding),
        ("content_md5_base64", meta.content_md5),
        ("content_language", meta.content_language),
        ("content_disposition", meta.content_disposition),
        ("cache_control", meta.cache_control),
        ("version", meta.version),
        ("restore_status", meta.restore_status),
    ];
    for (field, header) in fields {
        if let Some(text) = text_of(header) {
            value[field] = json!(text);
        }
    }
}

/// The window that a ranged read served, as `Content-Range` writes it:
/// `bytes 28-29/30`. A service clips a range that runs past the end, so
/// this can be shorter than the range the call asked for.
fn served_range(body: &BodyWindow) -> Option<String> {
    let len = body.expected_len.filter(|len| *len > 0)?;
    let last = body.object_offset + len - 1;
    Some(match body.object_size {
        Some(size) => format!("bytes {}-{last}/{size}", body.object_offset),
        None => format!("bytes {}-{last}/*", body.object_offset),
    })
}

// The call this process was started for, whose fields a refusal names.
static CURRENT_CALL: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

/// The call field that the crate's reason for refusing a request is about.
fn refused_call_parameter(reason_name: &str, call: &Value) -> Option<&'static str> {
    let named_field = |field: &'static str, alternative: &'static str| {
        if call.get(field).is_some() {
            field
        } else {
            alternative
        }
    };
    Some(match reason_name {
        "EmptyKey" | "UrlTooLong" | "RequestTooLarge" => "key",
        "Keys" => "keys",
        reason if reason.starts_with("Key") => "key",
        "Range" | "UnsupportedRange" | "RangedHead" => "range",
        "Condition" => CONDITION_FIELDS
            .iter()
            .map(|(field, _)| *field)
            .find(|field| call.get(field).is_some())
            .unwrap_or("if_match"),
        "PayloadTooLarge" => named_field("body", "body_base64"),
        // An Azure call names a part by its block ID, and an S3 call by its
        // part number. A commit names its parts in a list.
        "PartId" if call.get("block_id_base64").is_some() => "block_id_base64",
        "PartId" if call.get("part_number").is_some() => "part_number",
        "PartId" | "Parts" => named_field("blocks", "parts"),
        "UploadId" => "upload_id",
        "Tag" => "tags",
        "CopySource" => named_field("source_container", "source_bucket"),
        // A listing refuses an option it cannot list with.
        "Option" if call.get("include").is_some() => "include",
        "Revision" => ["snapshot", "version", "source_version"]
            .into_iter()
            .find(|field| call.get(field).is_some())?,
        // The field of the property that the call names.
        "ContentProperty" => [
            "content_type",
            "content_encoding",
            "content_language",
            "content_disposition",
            "cache_control",
            "storage_class",
            "tier",
        ]
        .into_iter()
        .find(|field| call.get(field).is_some())?,
        "Prefix" => "prefix",
        "Delimiter" => "delimiter",
        "Marker" => "continuation_token",
        "MaxResults" => "page_size",
        reason if reason.starts_with("Metadata") => "metadata",
        "Checksum" => "checksum",
        _ => return None,
    })
}

/// Returns the result for an error from the crate.
///
/// A request that the crate will not write becomes a refusal to send. If the crate
/// names the answer the account would give, the refusal carries it. Any other
/// error becomes a failed operation.
fn result_for_crate_error(error: CrateError) -> Value {
    match error {
        CrateError::InvalidRequest(invalid_request) => {
            let reason_name = format!("{invalid_request:?}");
            let mut result = json!({
                "outcome": "refused",
                "kind": snake_case_name(&reason_name),
            });
            let call = CURRENT_CALL.get().unwrap_or(&Value::Null);
            if let Some(parameter) = refused_call_parameter(&reason_name, call) {
                result["parameter"] = json!(parameter);
            }
            // Only an Azure call sets the namespace, and only Azure names a
            // rejection.
            if let Some(&namespace) = azure::ACCOUNT_NAMESPACE.get()
                && let Some(rejection) = invalid_request.azure_rejection(namespace)
            {
                result["status"] = json!(rejection.status);
                result["code"] = json!(rejection.code);
            }
            result
        }
        // The service wrote an error into the body of a success.
        CrateError::Service(_) => json!({
            "outcome": "error",
            "status": 200,
            "kind": error_kind_for_status(200),
            "reason": error.to_string(),
        }),
        other => json!({
            "outcome": "error",
            "kind": "other",
            "reason": format!("{other:?}"),
        }),
    }
}

macro_rules! crate_step {
    ($expression:expr) => {
        match $expression {
            Ok(value) => value,
            Err(error) => return Ok($crate::result_for_crate_error(error)),
        }
    };
}

macro_rules! transport_step {
    ($expression:expr) => {
        match $expression {
            Ok(value) => value,
            Err(error) => {
                return Ok(json!({
                    "outcome": "error",
                    "kind": "transport",
                    "reason": error.to_string(),
                }));
            }
        }
    };
}

/// Returns early from a page read, or from opening an S3 Express session,
/// with the result for an error from the crate, as [`crate_step`] does from a
/// whole operation.
macro_rules! page_step {
    ($expression:expr) => {
        match $expression {
            Ok(value) => value,
            Err(error) => return Ok(Err($crate::result_for_crate_error(error))),
        }
    };
}

/// The result for a request that could not be sent or answered.
fn transport_failure(error: &ureq::Error) -> Value {
    json!({
        "outcome": "error",
        "kind": "transport",
        "reason": error.to_string(),
    })
}

// After the macros, which the provider modules use.
mod azure;
mod listing;
mod s3;

fn current_timestamps() -> Timestamps {
    let seconds_since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    Timestamps::from_unix(seconds_since_epoch)
}

fn send_request(
    context: &AdapterContext,
    request: &impl Outgoing,
) -> Result<HttpExchange, ureq::Error> {
    send_request_with(context, request, None, BodyRead::Hold)
}

/// What the adapter sends of a request: its method, URL, fields and
/// content.
pub(crate) trait Outgoing {
    fn method(&self) -> borink_object_storage_proto::Method;
    fn url(&self) -> &str;
    fn fields(&self) -> Vec<(&str, &str)>;
    fn payload(&self) -> Payload<'_>;
}

impl Outgoing for WireRequest<'_> {
    fn method(&self) -> borink_object_storage_proto::Method {
        WireRequest::method(self)
    }
    fn url(&self) -> &str {
        WireRequest::url(self)
    }
    fn fields(&self) -> Vec<(&str, &str)> {
        self.headers().collect()
    }
    fn payload(&self) -> Payload<'_> {
        WireRequest::payload(self)
    }
}

/// The URL and the fields of a request, owned.
pub(crate) fn owned(request: &impl Outgoing) -> (String, Vec<(String, String)>) {
    (
        request.url().to_owned(),
        request
            .fields()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
    )
}

/// The bytes that `encode` needs: it refuses an empty buffer with the
/// number.
pub(crate) fn required<T>(
    encode: impl FnOnce(&mut [u8]) -> Result<T, CrateError>,
) -> Result<usize, CrateError> {
    match encode(&mut []) {
        Err(CrateError::Capacity(capacity)) => Ok(capacity.required),
        Err(error) => Err(error),
        Ok(_) => Ok(0),
    }
}

/// Sends a request whose streamed payload, if any, is `body`, and reads its
/// response as `read` says.
fn send_request_with(
    context: &AdapterContext,
    request: &impl Outgoing,
    body: Option<&CallBody>,
    read: BodyRead<'_>,
) -> Result<HttpExchange, ureq::Error> {
    let url = request.url();
    let mut builder = ureq::http::Request::builder().method(request.method().as_str());
    // See `AdapterContext::for_endpoint`.
    let agent = match context
        .plain_proxy
        .as_ref()
        .zip(url.strip_prefix("http://"))
    {
        Some(((proxy, agent), rest)) => {
            let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            builder = builder.uri(format!("{proxy}{path}")).header("host", host);
            agent
        }
        None => {
            builder = builder.uri(url);
            &context.http_agent
        }
    };
    for (name, value) in request.fields() {
        builder = builder.header(name, value);
    }

    // The request states the length of a streamed payload as its
    // Content-Length, which ureq keeps.
    let mut response = match (request.payload(), body) {
        (Payload::Slice(payload), _) => agent.run(builder.body(payload.to_vec())?)?,
        (Payload::Streamed { len }, Some(CallBody::Generated { pattern, .. })) => {
            let mut reader = Repeat::new(pattern, len);
            agent.run(builder.body(ureq::SendBody::from_reader(&mut reader))?)?
        }
        _ => agent.run(builder.body(())?)?,
    };

    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
        .collect();
    if let BodyRead::Sink(address) = read
        && (200..300).contains(&status)
    {
        let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
        let length = std::io::copy(&mut reader, &mut std::net::TcpStream::connect(address)?)?;
        return Ok(HttpExchange {
            status,
            headers,
            body: Vec::new(),
            sunk_length: Some(length),
        });
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BODY_BYTES)
        .read_to_vec()?;
    Ok(HttpExchange {
        status,
        headers,
        body,
        sunk_length: None,
    })
}

/// Reports the body that a read got: its bytes, or only its size when it
/// went to the grader's body sink.
fn read_body_fields(value: &mut Value, exchange: &HttpExchange) {
    match exchange.sunk_length {
        Some(length) => {
            value["size"] = json!(length);
        }
        None => {
            value["body_base64"] = json!(STANDARD.encode(&exchange.body));
            value["size"] = json!(exchange.body.len());
        }
    }
}

fn decode_base64_field(call: &Value, field: &str) -> Result<Vec<u8>, AdapterError> {
    let text = call
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {field}"))?;
    Ok(STANDARD.decode(text)?)
}

fn optional_text<'a>(call: &'a Value, field: &str) -> Option<&'a str> {
    call.get(field).and_then(Value::as_str)
}

fn text_of(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(|bytes| String::from_utf8_lossy(bytes).into_owned())
}

/// The call fields that carry a precondition, and the kind of each.
const CONDITION_FIELDS: [(&str, ConditionKind); 4] = [
    ("if_match", ConditionKind::IfMatch),
    ("if_none_match", ConditionKind::IfNoneMatch),
    ("if_modified_since", ConditionKind::IfModifiedSince),
    ("if_unmodified_since", ConditionKind::IfUnmodifiedSince),
];

/// The crate takes one precondition per request.
fn requested_condition(call: &Value) -> Option<(ConditionKind, Option<ConditionValue<'_>>)> {
    let mut given = CONDITION_FIELDS
        .iter()
        .filter_map(|(field, kind)| Some((*kind, optional_text(call, field)?)));
    match (given.next(), given.next()) {
        (None, _) => Some((ConditionKind::None, None)),
        (Some((kind, text)), None) => Some((kind, Some(condition_value(kind, text)))),
        (Some(_), Some(_)) => None,
    }
}

/// What a condition field compares against: an entity tag as the call names
/// it, or the instant that an HTTP date names. A date this adapter cannot
/// read becomes an instant past the last one an HTTP date writes, which the
/// crate refuses as an invalid condition, as it refused the text when it took
/// one.
fn condition_value(kind: ConditionKind, text: &str) -> ConditionValue<'_> {
    match kind {
        ConditionKind::IfModifiedSince | ConditionKind::IfUnmodifiedSince => ConditionValue::Time(
            borink_object_storage_proto::http_date_ms(text)
                .map_or(u64::MAX, |millis| millis / 1000),
        ),
        _ => ConditionValue::ETag(text.as_bytes()),
    }
}

fn requested_range(call: &Value) -> Result<RequestedRange, AdapterError> {
    range_of(call.get("range"))
}

fn range_of(range: Option<&Value>) -> Result<RequestedRange, AdapterError> {
    let Some(range) = range else {
        return Ok(RequestedRange::Whole);
    };

    if let Some(suffix_length) = range.get("suffix").and_then(Value::as_u64) {
        return Ok(RequestedRange::Suffix(suffix_length));
    }

    let start = range
        .get("start")
        .and_then(Value::as_u64)
        .ok_or("range without start")?;
    Ok(match range.get("end").and_then(Value::as_u64) {
        Some(end) => RequestedRange::Bounded { start, end },
        None => RequestedRange::Offset(start),
    })
}

/// Returns the first call field that `mapped_fields` lacks, as unsupported.
///
/// A field that `crate_limitation` names is unsupported by the crate, and
/// any other by this adapter. The adapter thus never sends a request without
/// something the case asked for.
fn unmapped_call_field(
    call: &Value,
    mapped_fields: &[&str],
    crate_limitation: fn(&str) -> Option<&'static str>,
) -> Option<Value> {
    let unmapped_field = call.as_object()?.keys().find(|field| {
        !matches!(field.as_str(), "op" | "credential_mode")
            && !mapped_fields.contains(&field.as_str())
    })?;
    Some(match crate_limitation(unmapped_field) {
        Some(reason) => unsupported_by_crate(reason),
        None => unsupported_by_adapter(&format!("call field {unmapped_field} is not mapped")),
    })
}

fn checksum_digest<C: Checksum>(body: &[u8]) -> Vec<u8> {
    let mut checksum = C::default();
    checksum.update(body);
    checksum.finish().as_bytes().to_vec()
}

/// Computes a digest with the checksum and SHA-256 types of
/// `borink-object-storage-crypto`.
fn compute_digest(call: &Value) -> Result<Value, AdapterError> {
    let body = decode_base64_field(call, "body_base64")?;
    let digest = match optional_text(call, "algorithm") {
        Some("md5") => checksum_digest::<Md5RustCrypto>(&body),
        Some("azure_crc64") => checksum_digest::<Crc64>(&body),
        Some("sha256") => sha256::<Sha256RustCrypto>(&body).to_vec(),
        _ => return Ok(unsupported_by_crate("digest algorithm")),
    };
    Ok(successful_result(
        json!({"digest_base64": STANDARD.encode(digest)}),
    ))
}

fn hmac_sha256(call: &Value) -> Result<Value, AdapterError> {
    let key = decode_base64_field(call, "key_base64")?;
    let body = decode_base64_field(call, "body_base64")?;
    Ok(successful_result(json!({
        "digest_base64": STANDARD.encode(hmac::<Sha256RustCrypto>(&key, &body)),
    })))
}

fn execute_operation(message: &Value) -> Result<Value, AdapterError> {
    let version = message.get("version").and_then(Value::as_u64);
    if version != Some(PROTOCOL_VERSION) {
        return Err(format!(
            "object-tests sent protocol version {}, and this adapter speaks {PROTOCOL_VERSION}",
            version.map_or_else(|| "none".to_owned(), |version| version.to_string()),
        )
        .into());
    }
    let call = message.get("call").ok_or("missing call")?;
    CURRENT_CALL.set(call.clone()).ok();
    // The digests are the same for every provider.
    match call.get("op").and_then(Value::as_str) {
        Some("digest") => return compute_digest(call),
        Some("hmac_sha256") => return hmac_sha256(call),
        _ => {}
    }
    match message.get("provider").and_then(Value::as_str) {
        Some("azure") => azure::execute_operation(message, call),
        Some("s3") => s3::execute_operation(message, call),
        _ => Ok(unsupported_by_crate(
            "the crate speaks Azure Blob and S3 only",
        )),
    }
}

/// Reads one message from standard input, performs its call and prints the
/// result. An error goes to standard error and exits with status 2.
pub fn run() {
    let mut input = String::new();
    let result = std::io::stdin()
        .read_to_string(&mut input)
        .map_err(AdapterError::from)
        .and_then(|_| Ok(serde_json::from_str::<Value>(&input)?))
        .and_then(|message| execute_operation(&message));

    match result {
        Ok(result) => println!("{result}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

/// Tags as the result reports them: an object of keys and values.
fn tags_value(tags: &[Tag<'_>]) -> Value {
    Value::Object(
        tags.iter()
            .map(|tag| (tag.key.to_owned(), json!(tag.value)))
            .collect(),
    )
}

/// The keys that a call names.
/// The objects that a removal of several names: each a key, or an object
/// of a key and the version or snapshot to remove.
fn requested_keys(call: &Value) -> Vec<DeleteTarget<'_>> {
    call.get("keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|object| match object.as_str() {
            Some(key) => Some(DeleteTarget::new(key)),
            None => Some(DeleteTarget {
                key: optional_text(object, "key")?,
                revision: requested_revision(object),
            }),
        })
        .collect()
}
