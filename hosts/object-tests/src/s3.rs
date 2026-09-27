//! The S3 half of the adapter: `s3::Objects` for GET, HEAD, PUT, DELETE and
//! listing, and for the SigV4 signatures of the vector suite.
//!
//! The crate signs with long-lived or temporary credentials. Offline cases
//! receive placeholder keys, and live cases read `AWS_ACCESS_KEY_ID`,
//! `AWS_SECRET_ACCESS_KEY` and, if set, `AWS_SESSION_TOKEN`.

use crate::listing::{
    Listed, ListedPage, PageRead, decoded_listing_text, entry_slots, list_all_keys, list_page,
};
use crate::{
    AdapterContext, AdapterError, HttpExchange, current_timestamps, decode_base64_field,
    failed_result, optional_text, request_buffers, requested_condition, requested_range,
    send_request, successful_result, text_of, transport_failure, unmapped_call_field,
    unsupported_by_adapter, unsupported_by_crate, unsupported_response_fields,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use borink_object_storage_crypto::{MD5_RUSTCRYPTO, SHA256_RUSTCRYPTO, wipe};
use borink_object_storage_proto::s3::{
    self, Addressing, Bucket, ObjectProperty, Objects, PayloadHash, PropertySet, Service,
};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{
    DeleteHeadOutcome, DeleteKind, GetHeadOutcome, GetKind, ListEntry, ListHeadOutcome, Metadata,
    MetadataPair, Payload, PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut, PutHeadOutcome,
    RequestedRange, Timestamps, TransactionalChecksum, WriteOptions, layered,
};
use serde_json::{Map, Value, json};

// AWS reports at most 1,000 entries on one listing page.
const MAX_PAGE_ENTRIES: usize = 1_000;

fn error_result(exchange: &HttpExchange, status: u16) -> Value {
    failed_result(status, s3::error_code(&exchange.body))
}

fn metadata_from_headers(exchange: &HttpExchange) -> Map<String, Value> {
    exchange
        .headers
        .iter()
        .filter_map(|(name, value)| {
            let name = s3::metadata_name(name)?;
            // The text is never longer than the header value.
            let mut text = vec![0; value.len()];
            let text = s3::metadata_value(value, &mut text)?;
            Some((name.to_owned(), json!(String::from_utf8_lossy(text))))
        })
        .collect()
}

fn read_object(
    context: &AdapterContext,
    objects: &Objects<'_>,
    call: &Value,
    kind: GetKind,
) -> Result<Value, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate("PhysicalGet carries one precondition"));
    };
    let key = optional_text(call, "key").unwrap_or_default();
    let range = if kind == GetKind::Head {
        RequestedRange::Whole
    } else {
        requested_range(call)?
    };
    let get_plan = PhysicalGet {
        key,
        kind,
        range,
        condition,
        condition_value,
    };

    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::get_requirements(objects, &get_plan, &now)
    ));
    let request =
        crate_step!(objects.encode_get(&mut request_bytes, &mut header_spans, &get_plan, &now));
    let exchange = transport_step!(send_request(context, &request));

    let head_outcome =
        crate_step!(objects.accept_get_head(get_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        GetHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_error_body(failure.status, failure.request_id, &exchange.body)
        }
        outcome => outcome,
    };

    Ok(match outcome {
        GetHeadOutcome::Body { meta, .. } | GetHeadOutcome::Complete { meta } => {
            // The crate reads these response headers from no head.
            let unsupported_fields = unsupported_response_fields(&[
                ("content_md5_base64", "Content-MD5"),
                ("content_language", "Content-Language"),
                ("content_disposition", "Content-Disposition"),
                ("cache_control", "Cache-Control"),
                ("storage_class", "x-amz-storage-class"),
            ]);
            let mut value = json!({
                "etag": text_of(meta.e_tag).unwrap_or_default(),
                "metadata": metadata_from_headers(&exchange),
            });
            if let Some(content_type) = text_of(meta.content_type) {
                value["content_type"] = json!(content_type);
            }
            if let Some(content_encoding) = text_of(meta.content_encoding) {
                value["content_encoding"] = json!(content_encoding);
            }
            if let Some(version) = text_of(meta.version) {
                value["version"] = json!(version);
            }
            if kind == GetKind::Head {
                value["size"] = json!(meta.size.unwrap_or(0));
            } else {
                value["body_base64"] = json!(STANDARD.encode(&exchange.body));
                value["size"] = json!(exchange.body.len());
            }
            let mut result = successful_result(value);
            result["unsupported_fields"] = unsupported_fields;
            result
        }
        GetHeadOutcome::NotModified { .. } => error_result(&exchange, 304),
        GetHeadOutcome::PreconditionFailed => error_result(&exchange, 412),
        GetHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        GetHeadOutcome::RangeNotSatisfiable { .. } => error_result(&exchange, 416),
        GetHeadOutcome::NeedErrorBody(failure) | GetHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn write_object(
    context: &AdapterContext,
    objects: &Objects<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate("PhysicalPut carries one precondition"));
    };
    let checksum_algorithm = call.pointer("/checksum/algorithm").and_then(Value::as_str);
    let checksum_value = call
        .pointer("/checksum/value_base64")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let checksum = match checksum_algorithm {
        None => None,
        Some("md5") => Some(TransactionalChecksum::Md5(checksum_value)),
        Some(_) => {
            return Ok(unsupported_by_crate(
                "an S3 write carries no checksum but Content-MD5",
            ));
        }
    };

    let metadata_pairs: Vec<MetadataPair<'_>> = call
        .get("metadata")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, value)| MetadataPair {
            name,
            value: value.as_str().unwrap_or_default(),
        })
        .collect();

    let key = optional_text(call, "key").unwrap_or_default();
    let body = decode_base64_field(call, "body_base64")?;
    let put_plan = PhysicalPut {
        key,
        condition,
        condition_value,
        metadata: &metadata_pairs,
        options: WriteOptions {
            checksum,
            ..WriteOptions::default()
        },
    };
    let payload = Payload::Slice(&body);

    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::put_requirements(objects, &put_plan, payload, PayloadHash::Compute, &now)
    ));
    let request = crate_step!(objects.encode_put(
        &mut request_bytes,
        &mut header_spans,
        &put_plan,
        payload,
        PayloadHash::Compute,
        &now
    ));
    let exchange = transport_step!(send_request(context, &request));

    let head_outcome =
        crate_step!(objects.accept_put_head(put_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        PutHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_put_error_body(failure.status, failure.request_id, &exchange.body)
        }
        outcome => outcome,
    };

    Ok(match outcome {
        PutHeadOutcome::Created { meta, .. } => {
            let mut value = json!({"etag": text_of(meta.e_tag).unwrap_or_default()});
            if let Some(version) = text_of(meta.version) {
                value["version"] = json!(version);
            }
            successful_result(value)
        }
        PutHeadOutcome::PreconditionFailed => error_result(&exchange, exchange.status),
        PutHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        PutHeadOutcome::NeedErrorBody(failure) | PutHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn delete_object(
    context: &AdapterContext,
    objects: &Objects<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate(
            "PhysicalDelete carries one precondition",
        ));
    };
    let key = optional_text(call, "key").unwrap_or_default();
    let delete_plan = PhysicalDelete {
        key,
        kind: DeleteKind::Object,
        condition,
        condition_value,
    };

    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::delete_requirements(objects, &delete_plan, &now)
    ));
    let request = crate_step!(objects.encode_delete(
        &mut request_bytes,
        &mut header_spans,
        &delete_plan,
        &now
    ));
    let exchange = transport_step!(send_request(context, &request));

    let head_outcome =
        crate_step!(objects.accept_delete_head(delete_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_delete_error_body(failure.status, failure.request_id, &exchange.body)
        }
        outcome => outcome,
    };

    Ok(match outcome {
        DeleteHeadOutcome::Accepted => successful_result(json!({})),
        DeleteHeadOutcome::PreconditionFailed => error_result(&exchange, exchange.status),
        DeleteHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        DeleteHeadOutcome::NeedErrorBody(failure) | DeleteHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

/// Requests one page and reads it, or returns the result to report instead.
fn read_page(
    context: &AdapterContext,
    objects: &Objects<'_>,
    list_plan: &PhysicalList<'_>,
) -> PageRead {
    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(page_step!(
        layered::s3::list_requirements(objects, list_plan, &now)
    ));
    let request =
        page_step!(objects.encode_list(&mut request_bytes, &mut header_spans, list_plan, &now));
    let mut exchange = match send_request(context, &request) {
        Ok(exchange) => exchange,
        Err(error) => return Ok(Err(transport_failure(&error))),
    };

    let head_outcome = page_step!(objects.accept_list_head(exchange.response_head()));
    let outcome = match head_outcome {
        ListHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_list_error_body(failure.status, failure.request_id, &exchange.body)
        }
        outcome => outcome,
    };
    let failed_status = match outcome {
        ListHeadOutcome::Page { .. } => None,
        ListHeadOutcome::NotFound { .. } => Some(404),
        ListHeadOutcome::NeedErrorBody(failure) | ListHeadOutcome::ServiceFailure(failure) => {
            Some(failure.status)
        }
        _ => Some(exchange.status),
    };
    if let Some(status) = failed_status {
        return Ok(Err(error_result(&exchange, status)));
    }

    let mut slots: Vec<ObjectWithProperties<'_>> = entry_slots(list_plan, MAX_PAGE_ENTRIES);
    // The storage class and the owner are read in the same pass as the page.
    let wanted = PropertySet::of(&[ObjectProperty::StorageClass, ObjectProperty::Owner]);
    let listing = page_step!(objects.fill_listing_with(
        &mut exchange.body,
        &mut slots,
        wanted,
        |entry, values| ObjectWithProperties {
            entry,
            storage_class: values.get(ObjectProperty::StorageClass),
            owner: values.get(ObjectProperty::Owner),
        }
    ));
    Ok(Ok(ListedPage::read(&slots, listing, listed_entry_value)))
}

/// A listed object, with the properties that the page was read for.
#[derive(Clone, Copy, Default)]
struct ObjectWithProperties<'b> {
    entry: ListEntry<'b>,
    storage_class: Option<&'b [u8]>,
    owner: Option<&'b [u8]>,
}

impl Listed for ObjectWithProperties<'_> {
    fn entry(&self) -> &ListEntry<'_> {
        &self.entry
    }
}

fn listed_entry_value(listed: &ObjectWithProperties<'_>) -> Value {
    let entry = &listed.entry;
    let mut value = json!({
        "key": entry.key,
        "size": entry.size.unwrap_or(0),
        "etag": entry.e_tag.unwrap_or_default(),
    });
    if let Some(millis) = entry.last_modified.and_then(layered::iso8601_ms) {
        value["last_modified"] = json!(rfc3339(millis / 1000));
    }
    if let Some(storage_class) = listed.storage_class {
        value["storage_class"] = json!(decoded_listing_text(storage_class));
    }
    // The owner holds its ID as an element of its own.
    if let Some((_, id)) = listed
        .owner
        .and_then(|owner| Metadata::new(owner).find(|(name, _)| *name == b"ID"))
    {
        value["owner_id"] = json!(decoded_listing_text(id));
    }
    value
}

/// Writes a time as `2024-01-02T03:04:05Z`, from the basic form that
/// `Timestamps::iso8601` writes.
fn rfc3339(unix_seconds: u64) -> String {
    let time = Timestamps::from_unix(unix_seconds);
    let basic = time.iso8601();
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        &basic[..4],
        &basic[4..6],
        &basic[6..8],
        &basic[9..11],
        &basic[11..13],
        &basic[13..15],
    )
}

/// Reads `2013-05-24T00:00:00Z` or `20130524T000000Z`.
fn parse_clock(clock: &str) -> Option<Timestamps> {
    let digits: String = clock.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 14 || !clock.ends_with('Z') {
        return None;
    }
    let number = |range: std::ops::Range<usize>| digits[range].parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(4..6)?, number(6..8)?);
    let (hour, minute, second) = (number(8..10)?, number(10..12)?, number(12..14)?);
    // Howard Hinnant's `days_from_civil`.
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    Some(Timestamps::from_unix(u64::try_from(seconds).ok()?))
}

/// Reads `bytes=A-B`, `bytes=A-` or `bytes=-N`.
fn parse_range_header(value: &str) -> Option<RequestedRange> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    Some(match (start, end) {
        ("", suffix) => RequestedRange::Suffix(suffix.parse().ok()?),
        (start, "") => RequestedRange::Offset(start.parse().ok()?),
        (start, last) => RequestedRange::Bounded {
            start: start.parse().ok()?,
            end: last.parse::<u64>().ok()?.checked_add(1)?,
        },
    })
}

/// Signs a GET, HEAD, PUT or DELETE the way `Objects` signs it, and returns
/// the request's headers.
///
/// The URL names a virtual-hosted bucket, and the plan carries the one header
/// the case adds. The crate derives every other signed header itself, so a
/// header it does not write is unsupported.
fn sign(call: &Value) -> Result<Value, AdapterError> {
    if optional_text(call, "signing_algorithm").is_some_and(|algorithm| algorithm != "sigv4") {
        return Ok(unsupported_by_crate(
            "the crate signs with SigV4 alone, not with an S3 Express session",
        ));
    }
    if optional_text(call, "service").is_some_and(|service| service != "s3") {
        return Ok(unsupported_by_crate(
            "the crate signs for the s3 service only",
        ));
    }
    let Some(now) = optional_text(call, "clock").and_then(parse_clock) else {
        return Ok(unsupported_by_adapter("clock not mapped"));
    };
    let url = optional_text(call, "url").ok_or("missing url")?;
    let Some((scheme, rest)) = url.split_once("://") else {
        return Ok(unsupported_by_adapter("url not mapped"));
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let Some((bucket_name, service_host)) = host.split_once('.') else {
        return Ok(unsupported_by_adapter("url names no virtual-hosted bucket"));
    };
    // The crate percent-encodes the key itself, so a path it would write
    // differently is not this key.
    if path.contains('%') {
        return Ok(unsupported_by_adapter("percent-encoded path not mapped"));
    }
    let endpoint = format!("{scheme}://{service_host}");
    let region = optional_text(call, "region").ok_or("missing region")?;
    let key_id = call
        .pointer("/credentials/access_key_id")
        .and_then(Value::as_str)
        .ok_or("missing access key")?;
    let secret = call
        .pointer("/credentials/secret_access_key")
        .and_then(Value::as_str)
        .ok_or("missing secret key")?;
    let session_token = call
        .pointer("/credentials/session_token")
        .and_then(Value::as_str);

    let mut range = RequestedRange::Whole;
    for (name, value) in call
        .get("headers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        match (name.to_ascii_lowercase().as_str(), value.as_str()) {
            ("range", Some(value)) => match parse_range_header(value) {
                Some(parsed) => range = parsed,
                None => return Ok(unsupported_by_adapter("range header not mapped")),
            },
            _ => {
                return Ok(unsupported_by_crate(&format!(
                    "the crate writes no {name} header of the caller's"
                )));
            }
        }
    }

    let bucket = crate_step!(Bucket::new(&endpoint, bucket_name, region, Service::Aws))
        .with_addressing(Addressing::VirtualHosted);
    let mut credentials = crate_step!(Credentials::new(key_id, secret, wipe));
    if let Some(token) = session_token {
        credentials = crate_step!(credentials.with_session_token(token));
    }
    let objects = Objects::new(bucket, credentials, SHA256_RUSTCRYPTO);
    let body = STANDARD.decode(optional_text(call, "body_base64").unwrap_or_default())?;

    let method = optional_text(call, "method").unwrap_or("GET");
    let (mut request_bytes, mut header_spans);
    let request = match method {
        "GET" | "HEAD" => {
            let get_plan = PhysicalGet {
                kind: if method == "GET" {
                    GetKind::Bytes
                } else {
                    GetKind::Head
                },
                range,
                ..PhysicalGet::new(path)
            };
            (request_bytes, header_spans) = request_buffers(crate_step!(
                layered::s3::get_requirements(&objects, &get_plan, &now)
            ));
            crate_step!(objects.encode_get(&mut request_bytes, &mut header_spans, &get_plan, &now))
        }
        "PUT" if range == RequestedRange::Whole => {
            let put_plan = PhysicalPut::new(path);
            let payload = Payload::Slice(&body);
            (request_bytes, header_spans) =
                request_buffers(crate_step!(layered::s3::put_requirements(
                    &objects,
                    &put_plan,
                    payload,
                    PayloadHash::Compute,
                    &now
                )));
            crate_step!(objects.encode_put(
                &mut request_bytes,
                &mut header_spans,
                &put_plan,
                payload,
                PayloadHash::Compute,
                &now
            ))
        }
        "DELETE" if range == RequestedRange::Whole => {
            let delete_plan = PhysicalDelete::new(path);
            (request_bytes, header_spans) = request_buffers(crate_step!(
                layered::s3::delete_requirements(&objects, &delete_plan, &now)
            ));
            crate_step!(objects.encode_delete(
                &mut request_bytes,
                &mut header_spans,
                &delete_plan,
                &now
            ))
        }
        _ => return Ok(unsupported_by_adapter("method not mapped")),
    };

    let headers: Map<String, Value> = request
        .headers()
        .map(|(name, value)| (name.to_ascii_lowercase(), json!(value)))
        .collect();
    let authorization = headers.get("authorization").cloned().unwrap_or_default();
    Ok(successful_result(json!({
        "authorization": authorization,
        "headers": headers,
    })))
}

// The call fields this adapter maps, by operation. `unmapped_call_field`
// reports any other field as unsupported.
fn mapped_call_fields(operation: &str) -> &'static [&'static str] {
    match operation {
        "get" => &["key", "range", "if_match", "if_none_match"],
        "head" => &["key", "if_match", "if_none_match"],
        "put" => &[
            "key",
            "body_base64",
            "if_match",
            "if_none_match",
            "metadata",
            "checksum",
        ],
        "delete" => &["key", "if_match", "if_none_match"],
        "list" => &["prefix", "page_size"],
        "list_page" => &[
            "prefix",
            "continuation_token",
            "start_after",
            "delimiter",
            "page_size",
            "fetch_owner",
        ],
        _ => &[],
    }
}

/// Returns the reason the crate cannot express an S3 call field, for the
/// fields it lacks.
fn crate_limitation(field: &str) -> Option<&'static str> {
    match field {
        "version" => Some("an S3 plan selects no version"),
        "if_modified_since" | "if_unmodified_since" => Some("ConditionKind has no date conditions"),
        "checksums" => Some("an S3 write carries no checksum but Content-MD5"),
        "content_type"
        | "content_encoding"
        | "content_language"
        | "content_disposition"
        | "cache_control"
        | "expires" => Some("PhysicalPut sets no content properties"),
        "tags" | "tagging" | "storage_class" => Some("PhysicalPut sets no storage class or tags"),
        _ => None,
    }
}

pub(crate) fn execute_operation(message: &Value, call: &Value) -> Result<Value, AdapterError> {
    let operation = call.get("op").and_then(Value::as_str).ok_or("missing op")?;
    match operation {
        "s3.sign" => return sign(call),
        "s3.canonical_request" => {
            return Ok(unsupported_by_crate(
                "the crate writes a canonical request only as part of a signed request",
            ));
        }
        _ => {}
    }
    if mapped_call_fields(operation).is_empty() {
        return Ok(unsupported_by_crate(&format!(
            "the crate has no S3 {operation} operation"
        )));
    }
    if let Some(unsupported) =
        unmapped_call_field(call, mapped_call_fields(operation), crate_limitation)
    {
        return Ok(unsupported);
    }

    let endpoint = message.get("endpoint").ok_or("missing endpoint")?;
    if endpoint
        .get("directory_bucket")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(unsupported_by_crate(
            "the crate signs no S3 Express session for a directory bucket",
        ));
    }
    let is_live = message.get("mode").and_then(Value::as_str) == Some("live");
    let invalid_credentials =
        call.get("credential_mode").and_then(Value::as_str) == Some("invalid");
    let (key_id, secret, session_token) = if !is_live {
        (
            "AKIDOFFLINEPLACEHOLDER".to_owned(),
            "offline-placeholder-secret".to_owned(),
            None,
        )
    } else if invalid_credentials {
        ("AKIDINVALID".to_owned(), "invalid".to_owned(), None)
    } else {
        (
            std::env::var("AWS_ACCESS_KEY_ID")?,
            std::env::var("AWS_SECRET_ACCESS_KEY")?,
            std::env::var("AWS_SESSION_TOKEN").ok(),
        )
    };

    let endpoint_url = optional_text(endpoint, "url").ok_or("missing endpoint url")?;
    let bucket_name = optional_text(endpoint, "bucket").ok_or("missing endpoint bucket")?;
    let region = optional_text(endpoint, "region").unwrap_or("us-east-1");
    let bucket = crate_step!(Bucket::new(endpoint_url, bucket_name, region, Service::Aws));
    let mut credentials = crate_step!(Credentials::new(&key_id, &secret, wipe));
    if let Some(token) = &session_token {
        credentials = crate_step!(credentials.with_session_token(token));
    }
    let now = current_timestamps();
    let objects = Objects::new(bucket, credentials, SHA256_RUSTCRYPTO)
        .with_signing_key(&now)
        .with_checksum(MD5_RUSTCRYPTO);

    let context = AdapterContext::for_endpoint(endpoint)?;
    match operation {
        "get" => read_object(&context, &objects, call, GetKind::Bytes),
        "head" => read_object(&context, &objects, call, GetKind::Head),
        "put" => write_object(&context, &objects, call),
        "delete" => delete_object(&context, &objects, call),
        "list" => list_all_keys(call, |plan| read_page(&context, &objects, plan)),
        "list_page" => list_page(call, |plan| read_page(&context, &objects, plan)),
        _ => Ok(unsupported_by_adapter("operation not mapped")),
    }
}
