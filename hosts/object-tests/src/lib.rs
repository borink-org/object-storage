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
    ConditionKind, Error as CrateError, HeaderSpan, RequestSize, RequestedRange, ResponseHead,
    Timestamps, WireRequest,
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
        ResponseHead::from_headers(
            self.status,
            self.headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_slice())),
        )
    }
}

struct AdapterContext {
    http_agent: ureq::Agent,
}

impl AdapterContext {
    /// An agent that hands every status back, follows no redirect, and goes
    /// through the endpoint's proxy if it names one.
    fn for_endpoint(endpoint: &Value) -> Result<Self, AdapterError> {
        let mut agent_config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0);
        if let Some(proxy_url) = optional_text(endpoint, "proxy_url") {
            agent_config = agent_config.proxy(Some(ureq::Proxy::new(proxy_url)?));
        }
        Ok(Self {
            http_agent: agent_config.build().into(),
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
/// the service named, if it named one.
fn failed_result(status: u16, code: Option<&str>) -> Value {
    let mut result = json!({
        "outcome": "error",
        "status": status,
        "kind": error_kind_for_status(status),
    });
    if let Some(code) = code {
        result["code"] = json!(code);
    }
    result
}

/// Declares the response fields that the crate reads from no head, each
/// named by its result field and its header.
fn unsupported_response_fields(fields: &[(&str, &str)]) -> Value {
    fields
        .iter()
        .map(|(field, header)| {
            json!({
                "at": format!("/value/{field}"),
                "scope": "sdk",
                "reason": format!("ResponseHead and ObjectMeta carry no {header}"),
            })
        })
        .collect()
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
        reason if reason.starts_with("Key") => "key",
        "Range" | "UnsupportedRange" | "RangedHead" => "range",
        "Condition" => named_field("if_match", "if_none_match"),
        "PayloadTooLarge" => "body_base64",
        "BlockId" => named_field("block_id_base64", "blocks"),
        "Blocks" => "blocks",
        "Prefix" => "prefix",
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

// After the macros, which the provider modules use.
mod azure;
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
    let mut builder = ureq::http::Request::builder()
        .method(request.method().as_str())
        .uri(request.url());
    for (name, value) in request.headers() {
        builder = builder.header(name, value);
    }

    let mut response = match request.payload().bytes() {
        Some(payload) => context.http_agent.run(builder.body(payload.to_vec())?)?,
        None => context.http_agent.run(builder.body(())?)?,
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

/// The crate takes one precondition per request.
fn requested_condition(call: &Value) -> Option<(ConditionKind, Option<&[u8]>)> {
    match (
        optional_text(call, "if_match"),
        optional_text(call, "if_none_match"),
    ) {
        (None, None) => Some((ConditionKind::None, None)),
        (Some(tag), None) => Some((ConditionKind::IfMatch, Some(tag.as_bytes()))),
        (None, Some(tag)) => Some((ConditionKind::IfNoneMatch, Some(tag.as_bytes()))),
        (Some(_), Some(_)) => None,
    }
}

fn requested_range(call: &Value) -> Result<RequestedRange, AdapterError> {
    let Some(range) = call.get("range") else {
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
