//! The Azure half of the adapter: `Blobs` for every operation it has.
//!
//! The crate authenticates with a bearer token only. Offline cases receive a
//! placeholder token. Live cases need `endpoint.auth = "bearer"`, and the
//! adapter reads the token from `AZURE_STORAGE_ACCESS_TOKEN`.

use crate::listing::{
    Listed, ListedPage, PageRead, PageSource, decoded_listing_text, entry_slots, list_all_keys,
    list_page,
};
use crate::{
    AdapterContext, AdapterError, HttpExchange, current_timestamps, decode_base64_field,
    failed_result, optional_text, request_buffers, requested_condition, requested_range,
    send_request, successful_result, text_of, transport_failure, unmapped_call_field,
    unsupported_by_adapter, unsupported_by_crate, unsupported_response_fields,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use borink_object_storage_proto::azure::{
    self, Block, BlockListKind, BlockRef, BlockSource, BlockState, PhysicalListBlocks,
    PhysicalStageBlock,
};
use borink_object_storage_proto::{
    AzureNamespace, Blobs, CommitHeadOutcome, Container, DeleteHeadOutcome, DeleteKind, EntryKind,
    GetHeadOutcome, GetKind, ListEntry, ListHeadOutcome, ListInclude, ListPartsHeadOutcome,
    MetadataPair, Payload, PhysicalCommit, PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut,
    PropertySet, PutHeadOutcome, RequestedRange, StageHeadOutcome, TransactionalChecksum,
    WriteOptions, layered,
};
use serde_json::{Map, Value, json};

// Azure returns at most 5,000 entries on one listing page.
const MAX_PAGE_ENTRIES: usize = 5_000;

// The account kind this process was started for, which a refusal reads.
pub(crate) static ACCOUNT_NAMESPACE: std::sync::OnceLock<AzureNamespace> =
    std::sync::OnceLock::new();

fn error_result(exchange: &HttpExchange, status: u16) -> Value {
    let code = azure::error_code(&exchange.response_head(), &exchange.body);
    failed_result(status, code.map(String::from_utf8_lossy).as_deref())
}

/// Returns `true` if the call selects a snapshot or a version, which the crate's plans cannot.
fn names_snapshot_or_version(call: &Value) -> bool {
    call.get("snapshot").is_some() || call.get("version").is_some()
}

fn metadata_from_headers(exchange: &HttpExchange) -> Map<String, Value> {
    exchange
        .headers
        .iter()
        .filter_map(|(name, value)| {
            azure::metadata_name(name)
                .map(|name| (name.to_owned(), json!(String::from_utf8_lossy(value))))
        })
        .collect()
}

fn read_object(
    context: &AdapterContext,
    blobs: &Blobs<'_>,
    call: &Value,
    kind: GetKind,
) -> Result<Value, AdapterError> {
    if names_snapshot_or_version(call) {
        return Ok(unsupported_by_crate(
            "PhysicalGet selects no snapshot or version",
        ));
    }
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
        layered::get_requirements(blobs, &get_plan, &now)
    ));
    let request =
        crate_step!(blobs.encode_get(&mut request_bytes, &mut header_spans, &get_plan, &now));
    let exchange = transport_step!(send_request(context, &request));

    let head_outcome =
        crate_step!(blobs.accept_get_head(get_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        GetHeadOutcome::NeedErrorBody(failure) => {
            blobs.accept_get_error_body(get_plan.shape(), failure, &exchange.body)
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
    blobs: &Blobs<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    // A plan holds one transactional checksum, so the crate cannot send a write
    // that names two. Azure refuses such a write too.
    if call.get("checksums").is_some() {
        return Ok(json!({
            "outcome": "refused",
            "kind": "one_transactional_checksum",
            "parameter": "checksums",
        }));
    }
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
        Some("azure_crc64") => Some(TransactionalChecksum::Crc64(checksum_value)),
        Some(_) => return Ok(unsupported_by_crate("checksum algorithm")),
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
        layered::put_requirements(blobs, &put_plan, payload, &now)
    ));
    let request = crate_step!(blobs.encode_put(
        &mut request_bytes,
        &mut header_spans,
        &put_plan,
        payload,
        &now
    ));
    let exchange = transport_step!(send_request(context, &request));

    let head_outcome =
        crate_step!(blobs.accept_put_head(put_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        PutHeadOutcome::NeedErrorBody(failure) => {
            blobs.accept_put_error_body(put_plan.shape(), failure, &exchange.body)
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
    blobs: &Blobs<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    if names_snapshot_or_version(call) {
        return Ok(unsupported_by_crate(
            "PhysicalDelete selects no snapshot or version",
        ));
    }
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate(
            "PhysicalDelete carries one precondition",
        ));
    };
    let delete_kind = match optional_text(call, "snapshots") {
        None => DeleteKind::Object,
        Some("only") => DeleteKind::SnapshotsOnly,
        Some("include") => DeleteKind::ObjectAndSnapshots,
        Some(_) => return Err("unknown snapshots mode".into()),
    };

    let key = optional_text(call, "key").unwrap_or_default();
    let delete_plan = PhysicalDelete {
        key,
        kind: delete_kind,
        condition,
        condition_value,
    };

    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::delete_requirements(blobs, &delete_plan, &now)
    ));
    let request =
        crate_step!(blobs.encode_delete(&mut request_bytes, &mut header_spans, &delete_plan, &now));
    let exchange = transport_step!(send_request(context, &request));

    let head_outcome =
        crate_step!(blobs.accept_delete_head(delete_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            blobs.accept_delete_error_body(delete_plan.shape(), failure, &exchange.body)
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

fn listed_entry_value(entry: &ListEntry<'_>, metadata_requested: bool) -> Value {
    let mut value = json!({
        "key": entry.key,
        "size": entry.size.unwrap_or(0),
        "etag": entry.e_tag.unwrap_or_default(),
    });

    if let Some(metadata) = entry.metadata() {
        let pairs: Map<String, Value> = metadata
            .map(|(name, value)| {
                (
                    decoded_listing_text(name),
                    json!(decoded_listing_text(value)),
                )
            })
            .collect();
        value["metadata"] = Value::Object(pairs);
    } else if metadata_requested {
        value["metadata"] = json!({});
    }

    if entry.kind == EntryKind::Directory {
        value["directory"] = json!(true);
    }

    let listed_properties = [
        ("snapshot", "Snapshot"),
        ("version", "VersionId"),
        ("content_md5_base64", "Content-MD5"),
        ("content_crc64_base64", "Content-CRC64"),
    ];
    for (field, property_name) in listed_properties {
        if let Some(property) = entry
            .property(property_name)
            .filter(|bytes| !bytes.is_empty())
        {
            value[field] = json!(decoded_listing_text(property));
        }
    }
    if let Some(is_current_version) = entry.property("IsCurrentVersion") {
        value["is_current_version"] = json!(is_current_version == b"true");
    }
    value
}

/// The pages of one container's listings, read with a `Blobs` client.
struct BlobPages<'a> {
    context: &'a AdapterContext,
    blobs: &'a Blobs<'a>,
}

impl PageSource for BlobPages<'_> {
    fn read_page(&self, list_plan: &PhysicalList<'_>) -> PageRead {
        let (context, blobs) = (self.context, self.blobs);
        let now = current_timestamps();
        let (mut request_bytes, mut header_spans) = request_buffers(page_step!(
            layered::list_requirements(blobs, list_plan, &now)
        ));
        let request =
            page_step!(blobs.encode_list(&mut request_bytes, &mut header_spans, list_plan, &now));
        let mut exchange = match send_request(context, &request) {
            Ok(exchange) => exchange,
            Err(error) => return Ok(Err(transport_failure(&error))),
        };

        let head_outcome = page_step!(blobs.accept_list_head(exchange.response_head()));
        let outcome = match head_outcome {
            ListHeadOutcome::NeedErrorBody(failure) => {
                blobs.accept_list_error_body(failure, &exchange.body)
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

        let mut slots: Vec<ListedBlob<'_>> = entry_slots(list_plan, MAX_PAGE_ENTRIES);
        let metadata_requested = list_plan.include.contains(ListInclude::METADATA);
        let listing = page_step!(blobs.fill_listing_with(
            &mut exchange.body,
            &mut slots,
            PropertySet::default(),
            |entry, _| ListedBlob {
                entry,
                metadata_requested,
            }
        ));
        Ok(Ok(ListedPage::read(&slots, listing)))
    }
}

/// A listed blob, and whether the plan asked for its metadata, which the
/// result reports as an empty set when the blob has none.
#[derive(Clone, Copy, Default)]
struct ListedBlob<'b> {
    entry: ListEntry<'b>,
    metadata_requested: bool,
}

impl Listed for ListedBlob<'_> {
    fn entry(&self) -> &ListEntry<'_> {
        &self.entry
    }

    fn value(&self) -> Value {
        listed_entry_value(&self.entry, self.metadata_requested)
    }
}

fn stage_block(
    context: &AdapterContext,
    blobs: &Blobs<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let key = optional_text(call, "key").unwrap_or_default();
    let block_id = optional_text(call, "block_id_base64").unwrap_or_default();
    let body = decode_base64_field(call, "body_base64")?;
    let stage_plan = PhysicalStageBlock {
        key,
        id: block_id,
        options: WriteOptions::default(),
    };
    let payload = Payload::Slice(&body);

    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::stage_block_requirements(blobs, &stage_plan, payload, &now)
    ));
    let request = crate_step!(blobs.encode_stage_block(
        &mut request_bytes,
        &mut header_spans,
        &stage_plan,
        payload,
        &now
    ));
    let exchange = transport_step!(send_request(context, &request));

    let head_outcome = crate_step!(blobs.accept_stage_block_head(exchange.response_head()));
    let outcome = match head_outcome {
        StageHeadOutcome::NeedErrorBody(failure) => {
            blobs.accept_stage_block_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };

    Ok(match outcome {
        StageHeadOutcome::Staged { .. } => successful_result(json!({})),
        StageHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        StageHeadOutcome::NeedErrorBody(failure) | StageHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn commit_blocks(
    context: &AdapterContext,
    blobs: &Blobs<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate(
            "PhysicalCommit carries one precondition",
        ));
    };

    let mut block_references = Vec::new();
    for block in call
        .get("blocks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let source = match block.get("kind").and_then(Value::as_str) {
            Some("latest") => BlockSource::Latest,
            Some("committed") => BlockSource::Committed,
            Some("uncommitted") => BlockSource::Uncommitted,
            _ => return Ok(unsupported_by_adapter("block kind not mapped")),
        };
        block_references.push(BlockRef {
            id: block
                .get("id_base64")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            source,
        });
    }

    let key = optional_text(call, "key").unwrap_or_default();
    let commit_plan = PhysicalCommit {
        key,
        condition,
        condition_value,
        metadata: &[],
        options: WriteOptions {
            declared_md5: optional_text(call, "content_md5_base64"),
            ..WriteOptions::default()
        },
    };

    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::commit_blocks_requirements(blobs, &commit_plan, &block_references, &now)
    ));
    let request = crate_step!(blobs.encode_commit_blocks(
        &mut request_bytes,
        &mut header_spans,
        &commit_plan,
        &block_references,
        &now
    ));
    let exchange = transport_step!(send_request(context, &request));

    let head_outcome =
        crate_step!(blobs.accept_commit_blocks_head(commit_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        CommitHeadOutcome::NeedErrorBody(failure) => {
            blobs.accept_commit_blocks_error_body(commit_plan.shape(), failure, &exchange.body)
        }
        outcome => outcome,
    };

    Ok(match outcome {
        CommitHeadOutcome::Committed { meta, .. } => {
            let mut value = json!({"etag": text_of(meta.e_tag).unwrap_or_default()});
            if let Some(version) = text_of(meta.version) {
                value["version"] = json!(version);
            }
            successful_result(value)
        }
        CommitHeadOutcome::PreconditionFailed => error_result(&exchange, exchange.status),
        CommitHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        CommitHeadOutcome::NeedErrorBody(failure) | CommitHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn list_blocks(
    context: &AdapterContext,
    blobs: &Blobs<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let block_list_kind = match optional_text(call, "kind").unwrap_or("all") {
        "all" => BlockListKind::All,
        "committed" => BlockListKind::Committed,
        "uncommitted" => BlockListKind::Staged,
        _ => return Err("unknown block list kind".into()),
    };

    let key = optional_text(call, "key").unwrap_or_default();
    let list_blocks_plan = PhysicalListBlocks::new(key, block_list_kind);

    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::list_blocks_requirements(blobs, &list_blocks_plan, &now)
    ));
    let request = crate_step!(blobs.encode_list_blocks(
        &mut request_bytes,
        &mut header_spans,
        &list_blocks_plan,
        &now
    ));
    let mut exchange = transport_step!(send_request(context, &request));

    let head_outcome = crate_step!(blobs.accept_list_blocks_head(exchange.response_head()));
    let outcome = match head_outcome {
        ListPartsHeadOutcome::NeedErrorBody(failure) => {
            blobs.accept_list_blocks_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    let failed_status = match outcome {
        ListPartsHeadOutcome::Parts { .. } => None,
        ListPartsHeadOutcome::NotFound { .. } => Some(404),
        ListPartsHeadOutcome::NeedErrorBody(failure)
        | ListPartsHeadOutcome::ServiceFailure(failure) => Some(failure.status),
        _ => Some(exchange.status),
    };
    if let Some(status) = failed_status {
        return Ok(error_result(&exchange, status));
    }

    // The crate bounds the count from the body's own length.
    let mut blocks = vec![Block::default(); layered::max_blocks_in(exchange.body.len())];
    let listing = crate_step!(blobs.fill_blocks(&mut exchange.body, &mut blocks));

    let mut committed = Vec::new();
    let mut uncommitted = Vec::new();
    for block in &blocks[..listing.filled] {
        let block_value = json!({"id_base64": block.id, "size": block.size});
        match block.state {
            BlockState::Committed => committed.push(block_value),
            _ => uncommitted.push(block_value),
        }
    }
    Ok(successful_result(json!({
        "committed": committed,
        "uncommitted": uncommitted,
    })))
}

/// The account kind: from the endpoint, unless the command line names one.
/// `--namespace unknown` measures a client that was told nothing.
fn account_namespace(endpoint: &Value) -> AzureNamespace {
    let command_line_arguments: Vec<String> = std::env::args().collect();
    let named_namespace = command_line_arguments
        .windows(2)
        .find(|pair| pair[0] == "--namespace")
        .map(|pair| pair[1].clone());
    match named_namespace.as_deref() {
        Some("unknown") => AzureNamespace::Unknown,
        Some("flat") => AzureNamespace::Flat,
        Some("hierarchical") => AzureNamespace::Hierarchical,
        _ if endpoint
            .get("hierarchical_namespace")
            .and_then(Value::as_bool)
            .unwrap_or(false) =>
        {
            AzureNamespace::Hierarchical
        }
        _ => AzureNamespace::Flat,
    }
}

// The call fields this adapter maps, by operation. `unmapped_call_field`
// reports any other field as unsupported.
fn mapped_call_fields(operation: &str) -> &'static [&'static str] {
    match operation {
        "get" => &[
            "key",
            "range",
            "if_match",
            "if_none_match",
            "version",
            "snapshot",
        ],
        "head" => &["key", "if_match", "if_none_match", "version", "snapshot"],
        "put" => &[
            "key",
            "body_base64",
            "if_match",
            "if_none_match",
            "metadata",
            "checksum",
            "checksums",
        ],
        "delete" => &[
            "key",
            "if_match",
            "if_none_match",
            "snapshots",
            "snapshot",
            "version",
        ],
        "list" => &["prefix", "page_size"],
        "list_page" => &[
            "prefix",
            "continuation_token",
            "delimiter",
            "page_size",
            "include",
        ],
        "azure.stage_block" => &["key", "block_id_base64", "body_base64"],
        "azure.commit_blocks" => &[
            "key",
            "blocks",
            "content_md5_base64",
            "if_match",
            "if_none_match",
        ],
        "azure.list_blocks" => &["key", "kind"],
        _ => &[],
    }
}

/// Returns the reason the crate cannot express a call field, for the fields it lacks.
fn crate_limitation(field: &str) -> Option<&'static str> {
    match field {
        "if_modified_since" | "if_unmodified_since" => Some("ConditionKind has no date conditions"),
        "content_type"
        | "content_encoding"
        | "content_language"
        | "content_disposition"
        | "cache_control" => Some("PhysicalPut sets no content properties"),
        "tier" | "tags" => Some("PhysicalPut sets no access tier or tags"),
        "lease_id" => Some("the crate's plans carry no lease ID"),
        _ => None,
    }
}

pub(crate) fn execute_operation(message: &Value, call: &Value) -> Result<Value, AdapterError> {
    let operation = call.get("op").and_then(Value::as_str).ok_or("missing op")?;
    match operation {
        "azure.sign" | "azure.string_to_sign" => {
            return Ok(unsupported_by_crate(
                "the crate authenticates with a bearer token and implements no Shared Key signing",
            ));
        }
        "azure.snapshot" => return Ok(unsupported_by_crate("the crate has no snapshot operation")),
        _ => {}
    }
    if mapped_call_fields(operation).is_empty() {
        return Ok(unsupported_by_crate(&format!(
            "the crate has no {operation} operation"
        )));
    }
    if let Some(unsupported) =
        unmapped_call_field(call, mapped_call_fields(operation), crate_limitation)
    {
        return Ok(unsupported);
    }

    let endpoint = message.get("endpoint").ok_or("missing endpoint")?;
    let is_live = message.get("mode").and_then(Value::as_str) == Some("live");
    let invalid_credentials =
        call.get("credential_mode").and_then(Value::as_str) == Some("invalid");
    let token = if !is_live {
        "offline-placeholder-token".to_owned()
    } else if endpoint.get("auth").and_then(Value::as_str) != Some("bearer") {
        return Ok(unsupported_by_crate(
            "the crate authenticates with a bearer token only",
        ));
    } else if invalid_credentials {
        "invalid".to_owned()
    } else {
        std::env::var("AZURE_STORAGE_ACCESS_TOKEN")?
    };

    let endpoint_url = optional_text(endpoint, "url").ok_or("missing endpoint url")?;
    let container_name = optional_text(endpoint, "bucket").ok_or("missing endpoint bucket")?;
    let namespace = account_namespace(endpoint);
    ACCOUNT_NAMESPACE.set(namespace).ok();
    let container = crate_step!(Container::new(endpoint_url, container_name));
    let blobs = crate_step!(Blobs::new(container, &token)).with_namespace(namespace);

    let context = AdapterContext::for_endpoint(endpoint)?;
    let pages = BlobPages {
        context: &context,
        blobs: &blobs,
    };

    match operation {
        "get" => read_object(&context, &blobs, call, GetKind::Bytes),
        "head" => read_object(&context, &blobs, call, GetKind::Head),
        "put" => write_object(&context, &blobs, call),
        "delete" => delete_object(&context, &blobs, call),
        "list" => list_all_keys(call, &pages),
        "list_page" => list_page(call, &pages),
        "azure.stage_block" => stage_block(&context, &blobs, call),
        "azure.commit_blocks" => commit_blocks(&context, &blobs, call),
        "azure.list_blocks" => list_blocks(&context, &blobs, call),
        _ => Ok(unsupported_by_adapter("operation not mapped")),
    }
}
