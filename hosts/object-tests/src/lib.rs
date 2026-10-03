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
use borink_object_storage_crypto::{Checksum, Crc64, Md5RustCrypto, SHA256_RUSTCRYPTO};
use borink_object_storage_proto::{
    BodyWindow, ChecksumKind, ConditionKind, ConditionValue, ContentProperties, CopySource,
    DeleteTarget, Error as CrateError, HeaderSpan, MetadataPair, ObjectMeta, PhysicalRestore,
    RequestSize, RequestedRange, ResponseHead, RestoreHeadOutcome, RestorePriority, Revision,
    ServiceErrorKind, Tag, Timestamps, TransactionalChecksum, WireRequest,
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
fn requested_restore(call: &Value) -> Option<PhysicalRestore<'_>> {
    let priority = match optional_text(call, "priority") {
        None | Some("standard") => RestorePriority::Standard,
        Some("high") => RestorePriority::High,
        Some("bulk") => RestorePriority::Bulk,
        Some(_) => return None,
    };
    Some(PhysicalRestore {
        revision: requested_revision(call),
        priority,
        days: call
            .get("days")
            .and_then(Value::as_u64)
            .map(|days| u32::try_from(days).unwrap_or(u32::MAX)),
        tier: optional_text(call, "tier"),
        ..PhysicalRestore::new(optional_text(call, "key").unwrap_or_default())
    })
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

/// The object that a copy call reads, with `container_field` naming its
/// container or bucket, or `None` if the call names more than the one
/// condition on it that the crate takes.
fn requested_source<'c>(call: &'c Value, container_field: &str) -> Option<CopySource<'c>> {
    let mut given = SOURCE_CONDITION_FIELDS
        .iter()
        .filter_map(|(field, kind)| Some((*kind, optional_text(call, field)?)));
    let (condition, condition_value) = match (given.next(), given.next()) {
        (None, _) => (ConditionKind::None, None),
        (Some((kind, text)), None) => (kind, Some(condition_value(kind, text))),
        (Some(_), Some(_)) => return None,
    };
    Some(CopySource {
        endpoint: optional_text(call, "source_account_url"),
        container: optional_text(call, container_field),
        key: optional_text(call, "source_key").unwrap_or_default(),
        revision: optional_text(call, "source_version").map(Revision::Version),
        condition,
        condition_value,
    })
}

/// The bytes of the source that a stage from a copy names.
fn requested_source_range(call: &Value) -> Result<RequestedRange, AdapterError> {
    range_of(call.get("source_range"))
}

/// The snapshot or version that a read or a removal names.
fn requested_revision(call: &Value) -> Option<Revision<'_>> {
    optional_text(call, "snapshot")
        .map(Revision::Snapshot)
        .or_else(|| optional_text(call, "version").map(Revision::Version))
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

/// The checksum that a write call names: its value, or the algorithm alone
/// for the crate to compute. `None` inside is an algorithm this adapter does
/// not map.
fn requested_checksum(call: &Value) -> Option<Option<TransactionalChecksum<'_>>> {
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
    Some(Some(match value {
        None => TransactionalChecksum::Compute(kind),
        Some(text) => match kind {
            ChecksumKind::Md5 => TransactionalChecksum::Md5(text),
            ChecksumKind::Crc64 => TransactionalChecksum::Crc64(text),
            ChecksumKind::Crc32 => TransactionalChecksum::Crc32(text),
            ChecksumKind::Crc32c => TransactionalChecksum::Crc32c(text),
            ChecksumKind::Sha1 => TransactionalChecksum::Sha1(text),
            _ => TransactionalChecksum::Sha256(text),
        },
    }))
}

/// The refusal of a write that names two checksums: a plan holds one, and
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

/// The call field that the crate's reason for refusing a plan is about.
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
        "PayloadTooLarge" => "body_base64",
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
/// A plan that the crate will not encode becomes a refusal to send. If the crate
/// names the answer the account would give, the refusal carries it. Any other
/// error becomes a failed operation.
fn result_for_crate_error(error: CrateError) -> Value {
    match error {
        CrateError::InvalidPlan(invalid_plan) => {
            let reason_name = format!("{invalid_plan:?}");
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
                && let Some(rejection) = invalid_plan.azure_rejection(namespace)
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

// The crate sizes a request as bytes and header slots, and writes into both.
fn request_buffers(size: RequestSize) -> (Vec<u8>, Vec<HeaderSpan>) {
    (
        vec![0; size.bytes],
        vec![HeaderSpan::default(); size.headers],
    )
}

fn send_request(
    context: &AdapterContext,
    request: &WireRequest<'_>,
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
    for (name, value) in request.headers() {
        builder = builder.header(name, value);
    }

    let mut response = match request.payload().bytes() {
        Some(payload) => agent.run(builder.body(payload.to_vec())?)?,
        None => agent.run(builder.body(())?)?,
    };

    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
        .collect();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BODY_BYTES)
        .read_to_vec()?;
    Ok(HttpExchange {
        status,
        headers,
        body,
    })
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
            borink_object_storage_proto::layered::http_date_ms(text)
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

/// Computes a digest with the providers of `borink-object-storage-crypto`.
fn compute_digest(call: &Value) -> Result<Value, AdapterError> {
    let body = decode_base64_field(call, "body_base64")?;
    let digest = match optional_text(call, "algorithm") {
        Some("md5") => checksum_digest::<Md5RustCrypto>(&body),
        Some("azure_crc64") => checksum_digest::<Crc64>(&body),
        Some("sha256") => SHA256_RUSTCRYPTO.hash(&body).to_vec(),
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
        "digest_base64": STANDARD.encode(SHA256_RUSTCRYPTO.hmac(&key, &body)),
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
