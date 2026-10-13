//! The Azure half of the adapter: a template for each call, and a request
//! written from it.
//!
//! The crate authenticates with a bearer token only. Offline cases receive a
//! placeholder token. Live cases need `endpoint.auth = "bearer"`, and the
//! adapter reads the token from `AZURE_STORAGE_ACCESS_TOKEN`.

use crate::listing::{
    Listed, ListedPage, PageRead, PageSource, decoded_listing_text, entry_slots, list_all_keys,
    list_page,
};
use crate::{
    AdapterContext, AdapterError, BodyRead, CallBody, HttpExchange, checksum_value,
    current_timestamps, failed_result, optional_text, read_body_fields, read_meta_fields,
    requested_checksum_kind, requested_condition, requested_copy, requested_delete, requested_get,
    requested_keys, requested_metadata, requested_object, requested_properties,
    requested_restore_options, requested_tags, required, restore_value, send_request,
    send_request_with, served_range, successful_result, tags_value, text_of, transport_failure,
    two_checksums_refused, unmapped_call_field, unsupported_by_adapter, unsupported_by_crate,
};
use borink_object_storage_proto::azure::{
    self, BatchResult, Block, BlockListKind, BlockRef, BlockSource, BlockState, SnapshotHeadOutcome,
};
use borink_object_storage_proto::{
    AzureNamespace, ChecksumKind, Classification, CommitHeadOutcome, Container, CopyHeadOutcome,
    DeleteHeadOutcome, DeleteManyHeadOutcome, EntryKind, Error as CrateError, GetHeadOutcome,
    GetKind, ListEntry, ListHeadOutcome, ListInclude, ListItem, ListOptions, ListPartsHeadOutcome,
    ObjectItem, PropertySet, PutHeadOutcome, PutItem, PutOptions, RequestedRange,
    RestoreHeadOutcome, Revision, RevisionKind, StageHeadOutcome, Tag, TagsHeadOutcome,
    UpdateHeadOutcome, WireRequest, WriteOptions,
};
use serde_json::{Map, Value, json};

// Azure returns at most 5,000 entries on one listing page.
const MAX_PAGE_ENTRIES: usize = 5_000;

// The account kind this process was started for, which a refusal reads.
pub(crate) static ACCOUNT_NAMESPACE: std::sync::OnceLock<AzureNamespace> =
    std::sync::OnceLock::new();

fn error_result(exchange: &HttpExchange, status: u16) -> Value {
    let code = azure::error_code(&exchange.response_head(), &exchange.body);
    let kind = match azure::classify_error(&exchange.response_head(), &exchange.body, false) {
        Classification::Classified(kind) => Some(kind),
        _ => None,
    };
    failed_result(status, code.map(String::from_utf8_lossy).as_deref(), kind)
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

/// The account and the credentials of one case, which a template is written
/// from.
struct Account<'a> {
    container: Container<'a>,
    token: &'a str,
    namespace: AzureNamespace,
}

impl Account<'_> {
    /// Writes the template that `prepare` writes, into a buffer of the size
    /// it needs.
    fn template(
        &self,
        prepare: impl Fn(
            &Container<'_>,
            &str,
            AzureNamespace,
            &mut [u8],
        ) -> Result<azure::Template, CrateError>,
    ) -> Result<(azure::Template, Vec<u8>), CrateError> {
        let write = |buf: &mut [u8]| prepare(&self.container, self.token, self.namespace, buf);
        let mut bytes = vec![0; required(write)?];
        let template = write(&mut bytes)?;
        Ok((template, bytes))
    }

    /// Writes the template that `prepare` writes and the request that
    /// `encode` writes from it, and sends that request with `body`. Returns
    /// the exchange, or the result to report instead.
    fn send(
        &self,
        context: &AdapterContext,
        prepare: impl Fn(
            &Container<'_>,
            &str,
            AzureNamespace,
            &mut [u8],
        ) -> Result<azure::Template, CrateError>,
        encode: impl for<'r> Fn(
            &azure::Template,
            &[u8],
            &'r mut [u8],
        ) -> Result<WireRequest<'r>, CrateError>,
        body: Option<&CallBody>,
        read: BodyRead<'_>,
    ) -> Result<Result<HttpExchange, Value>, AdapterError> {
        let written = self.template(prepare).and_then(|(template, bytes)| {
            let mut buf = vec![0; required(|buf| encode(&template, &bytes, buf).map(drop))?];
            let request = encode(&template, &bytes, &mut buf)?;
            Ok(send_request_with(context, &request, body, read))
        });
        match written {
            Ok(Ok(exchange)) => Ok(Ok(exchange)),
            Ok(Err(error)) => Ok(Err(transport_failure(&error))),
            Err(error) => Ok(Err(crate::result_for_crate_error(error))),
        }
    }
}

/// Unwraps what `Account::send` returns, or returns the result it reports
/// instead.
macro_rules! sent {
    ($expression:expr) => {
        match $expression? {
            Ok(exchange) => exchange,
            Err(result) => return Ok(result),
        }
    };
}

fn read_object(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
    kind: GetKind,
) -> Result<Value, AdapterError> {
    let Some((options, item)) = requested_get(call, kind)? else {
        return Ok(unsupported_by_crate("a read carries one precondition"));
    };
    let now = current_timestamps();
    let (template, bytes) = crate_step!(account.template(|container, token, namespace, buf| {
        azure::prepare_get(container, token, namespace, &options, buf)
    }));
    let encode = |buf: &mut [u8]| azure::encode_get(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; crate_step!(required(encode))];
    let request = crate_step!(azure::encode_get(&template, &bytes, &item, &now, &mut buf));
    let exchange = transport_step!(send_request_with(
        context,
        &request,
        None,
        BodyRead::of(call)
    ));

    let shape = item.shape(&options);
    let head_outcome = crate_step!(azure::accept_get_head(shape, exchange.response_head()));
    let outcome = match head_outcome {
        GetHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_get_error_body(shape, failure, &exchange.body)
        }
        outcome => outcome,
    };
    let range = item.range;

    Ok(match outcome {
        GetHeadOutcome::Body { meta, .. } | GetHeadOutcome::Complete { meta } => {
            let mut value = json!({
                "etag": text_of(meta.e_tag).unwrap_or_default(),
                "metadata": metadata_from_headers(&exchange),
            });
            read_meta_fields(&mut value, &meta, "access_tier");
            if let GetHeadOutcome::Body { body, .. } = outcome
                && range != RequestedRange::Whole
                && let Some(served) = served_range(&body)
            {
                value["content_range"] = json!(served);
            }

            if kind == GetKind::Head {
                value["size"] = json!(meta.size.unwrap_or(0));
            } else {
                read_body_fields(&mut value, &exchange);
            }
            if let Some(copy_status) = text_of(meta.copy_status) {
                value["copy_status"] = json!(copy_status);
            }
            if let Some(copy_id) = text_of(meta.copy_id) {
                value["copy_id"] = json!(copy_id);
            }
            successful_result(value)
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
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    // A template sends one transactional checksum, so the crate cannot send a
    // write that names two. Azure refuses such a write too.
    if call.get("checksums").is_some() {
        return Ok(two_checksums_refused());
    }
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate("a write carries one precondition"));
    };
    let body = CallBody::of(call)?;
    // Azure takes an MD5 and a CRC64.
    let (checksum_kind, checksum) = match requested_checksum_kind(call) {
        None => (None, None),
        Some(Some((kind @ (ChecksumKind::Md5 | ChecksumKind::Crc64), text))) => {
            match checksum_value(kind, text, &body) {
                Ok(value) => (Some(kind), Some(value)),
                Err(result) => return Ok(result),
            }
        }
        Some(_) => return Ok(unsupported_by_crate("checksum algorithm")),
    };
    let metadata_pairs = requested_metadata(call);
    let tags = requested_tags(call);
    let options = PutOptions {
        condition,
        metadata: &metadata_pairs,
        options: WriteOptions {
            properties: requested_properties(call),
            tags: &tags,
            storage_class: optional_text(call, "tier"),
        },
        checksum: checksum_kind,
    };
    let item = PutItem {
        key: optional_text(call, "key").unwrap_or_default(),
        content: body.payload(),
        condition_value,
        checksum,
    };

    let now = current_timestamps();
    let (template, bytes) = crate_step!(account.template(|container, token, namespace, buf| {
        azure::prepare_put(container, token, namespace, &options, buf)
    }));
    let encode = |buf: &mut [u8]| azure::encode_put(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; crate_step!(required(encode))];
    let request = crate_step!(azure::encode_put(&template, &bytes, &item, &now, &mut buf));
    let exchange = transport_step!(send_request_with(
        context,
        &request,
        Some(&body),
        BodyRead::Hold
    ));

    let shape = item.shape(&options);
    let head_outcome = crate_step!(azure::accept_put_head(shape, exchange.response_head()));
    let outcome = match head_outcome {
        PutHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_put_error_body(shape, failure, &exchange.body)
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
        PutHeadOutcome::NeedErrorBody(failure) | PutHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn delete_object(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((options, item)) = requested_delete(call)? else {
        return Ok(unsupported_by_crate("a removal carries one precondition"));
    };

    let now = current_timestamps();
    let (template, bytes) = crate_step!(account.template(|container, token, namespace, buf| {
        azure::prepare_delete(container, token, namespace, &options, buf)
    }));
    let encode =
        |buf: &mut [u8]| azure::encode_delete(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; crate_step!(required(encode))];
    let request = crate_step!(azure::encode_delete(
        &template, &bytes, &item, &now, &mut buf
    ));
    let exchange = transport_step!(send_request(context, &request));

    let shape = options.shape();
    let head_outcome = crate_step!(azure::accept_delete_head(shape, exchange.response_head()));
    let outcome = match head_outcome {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_delete_error_body(shape, failure, &exchange.body)
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

/// The pages of one container's listings.
struct BlobPages<'a> {
    context: &'a AdapterContext,
    account: &'a Account<'a>,
}

impl PageSource for BlobPages<'_> {
    fn read_page(&self, options: &ListOptions<'_>, item: &ListItem<'_>) -> PageRead {
        let now = current_timestamps();
        let (template, bytes) =
            page_step!(self.account.template(|container, token, namespace, buf| {
                azure::prepare_list(container, token, namespace, options, buf)
            }));
        let encode =
            |buf: &mut [u8]| azure::encode_list(&template, &bytes, item, &now, buf).map(drop);
        let mut buf = vec![0; page_step!(required(encode))];
        let request = page_step!(azure::encode_list(&template, &bytes, item, &now, &mut buf));
        let mut exchange = match send_request(self.context, &request) {
            Ok(exchange) => exchange,
            Err(error) => return Ok(Err(transport_failure(&error))),
        };

        let head_outcome = page_step!(azure::accept_list_head(exchange.response_head()));
        let outcome = match head_outcome {
            ListHeadOutcome::NeedErrorBody(failure) => {
                azure::accept_list_error_body(failure, &exchange.body)
            }
            outcome => outcome,
        };
        let failed_status = match outcome {
            ListHeadOutcome::Page { .. } => None,
            ListHeadOutcome::NeedErrorBody(failure) | ListHeadOutcome::ServiceFailure(failure) => {
                Some(failure.status)
            }
            _ => Some(exchange.status),
        };
        if let Some(status) = failed_status {
            return Ok(Err(error_result(&exchange, status)));
        }

        let mut slots: Vec<ListedBlob<'_>> = entry_slots(options, MAX_PAGE_ENTRIES);
        let metadata_requested = options.include.contains(ListInclude::METADATA);
        let listing = page_step!(azure::fill_listing_with(
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
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let body = CallBody::of(call)?;
    let item = azure::StageBlockItem::new(
        optional_text(call, "key").unwrap_or_default(),
        optional_text(call, "block_id_base64").unwrap_or_default(),
        body.payload(),
    );
    let now = current_timestamps();
    let (template, bytes) = crate_step!(account.template(|container, token, namespace, buf| {
        azure::prepare_stage_block(
            container,
            token,
            namespace,
            &borink_object_storage_proto::StageOptions::default(),
            buf,
        )
    }));
    let encode =
        |buf: &mut [u8]| azure::encode_stage_block(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; crate_step!(required(encode))];
    let request = crate_step!(azure::encode_stage_block(
        &template, &bytes, &item, &now, &mut buf
    ));
    let exchange = transport_step!(send_request_with(
        context,
        &request,
        Some(&body),
        BodyRead::Hold
    ));

    let head_outcome = crate_step!(azure::accept_stage_block_head(exchange.response_head()));
    let outcome = match head_outcome {
        StageHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_stage_block_error_body(failure, &exchange.body)
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
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate("a commit carries one precondition"));
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

    let tags = requested_tags(call);
    let content_md5 = optional_text(call, "content_md5_base64");
    let options = azure::CommitOptions {
        condition,
        options: WriteOptions {
            properties: requested_properties(call),
            tags: &tags,
            storage_class: optional_text(call, "tier"),
        },
        content_md5: content_md5.is_some(),
        ..azure::CommitOptions::default()
    };
    let item = azure::CommitItem {
        condition_value,
        content_md5,
        ..azure::CommitItem::new(
            optional_text(call, "key").unwrap_or_default(),
            &block_references,
        )
    };

    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_commit_blocks(container, token, namespace, &options, buf)
        },
        |template, bytes, buf| azure::encode_commit_blocks(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));

    let shape = item.shape(&options);
    let head_outcome = crate_step!(azure::accept_commit_blocks_head(
        shape,
        exchange.response_head()
    ));
    let outcome = match head_outcome {
        CommitHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_commit_blocks_error_body(shape, failure, &exchange.body)
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
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let block_list_kind = match optional_text(call, "kind").unwrap_or("all") {
        "all" => BlockListKind::All,
        "committed" => BlockListKind::Committed,
        "uncommitted" => BlockListKind::Staged,
        _ => return Err("unknown block list kind".into()),
    };

    let item = ObjectItem::new(optional_text(call, "key").unwrap_or_default());
    let now = current_timestamps();
    let mut exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_list_blocks(
                container,
                token,
                namespace,
                block_list_kind,
                RevisionKind::Current,
                buf,
            )
        },
        |template, bytes, buf| azure::encode_list_blocks(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));

    let head_outcome = crate_step!(azure::accept_list_blocks_head(exchange.response_head()));
    let outcome = match head_outcome {
        ListPartsHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_list_blocks_error_body(failure, &exchange.body)
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
    let mut blocks = vec![Block::default(); azure::max_blocks_in(exchange.body.len())];
    let listing = crate_step!(azure::fill_blocks(&mut exchange.body, &mut blocks));

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
            "body_sink",
            "key",
            "range",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "version",
            "snapshot",
        ],
        "head" => &[
            "key",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "version",
            "snapshot",
        ],
        "put" => &[
            "key",
            "body_base64",
            "body",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "metadata",
            "checksum",
            "checksums",
            "content_type",
            "content_encoding",
            "content_language",
            "content_disposition",
            "cache_control",
            "tags",
            "tier",
        ],
        "delete" => &[
            "key",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "snapshots",
            "snapshot",
            "version",
        ],
        "list" => &["prefix", "page_size"],
        "list_page" => &[
            "prefix",
            "continuation_token",
            "version_marker",
            "delimiter",
            "page_size",
            "include",
        ],
        "azure.stage_block" => &["key", "block_id_base64", "body_base64", "body"],
        "azure.commit_blocks" => &[
            "key",
            "blocks",
            "content_md5_base64",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "content_type",
            "content_encoding",
            "content_language",
            "content_disposition",
            "cache_control",
            "tags",
            "tier",
        ],
        "azure.list_blocks" => &["key", "kind"],
        "azure.set_tier" => &["key", "tier"],
        "azure.snapshot" => &["key"],
        "azure.set_metadata" => &[
            "key",
            "metadata",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
        ],
        "azure.set_properties" => &[
            "key",
            "content_type",
            "content_encoding",
            "content_language",
            "content_disposition",
            "cache_control",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
        ],
        "copy" => &[
            "key",
            "source_key",
            "source_account_url",
            "source_container",
            "source_version",
            "source_if_match",
            "source_if_none_match",
            "source_if_modified_since",
            "source_if_unmodified_since",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "metadata",
            "content_type",
            "content_encoding",
            "content_language",
            "content_disposition",
            "cache_control",
            "tags",
            "tier",
            "sync",
        ],
        "azure.put_from_url" => &[
            "key",
            "source_key",
            "source_account_url",
            "source_container",
            "source_version",
            "source_if_match",
            "source_if_none_match",
            "source_if_modified_since",
            "source_if_unmodified_since",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "metadata",
            "content_type",
            "content_encoding",
            "content_language",
            "content_disposition",
            "cache_control",
            "tags",
            "tier",
        ],
        "azure.stage_block_from_url" => &[
            "key",
            "block_id_base64",
            "source_range",
            "source_key",
            "source_account_url",
            "source_container",
            "source_version",
            "source_if_match",
            "source_if_none_match",
            "source_if_modified_since",
            "source_if_unmodified_since",
        ],
        "azure.abort_copy" => &["key", "copy_id"],
        "azure.set_tags" => &["key", "tags", "version", "snapshot"],
        "azure.get_tags" => &["key", "version", "snapshot"],
        "restore" => &["key", "version", "snapshot", "priority", "days", "tier"],
        "delete_many" => &["keys"],
        _ => &[],
    }
}

/// Returns the reason the crate cannot express a call field, for the fields it lacks.
fn crate_limitation(field: &str) -> Option<&'static str> {
    match field {
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
    let container = crate_step!(Container::new_allowing_http(endpoint_url, container_name));
    let account = Account {
        container,
        token: &token,
        namespace,
    };

    let context = AdapterContext::for_endpoint(endpoint)?;
    let pages = BlobPages {
        context: &context,
        account: &account,
    };

    match operation {
        "get" => read_object(&context, &account, call, GetKind::Bytes),
        "head" => read_object(&context, &account, call, GetKind::Head),
        "put" => write_object(&context, &account, call),
        "delete" => delete_object(&context, &account, call),
        "list" => list_all_keys(call, &pages),
        "list_page" => list_page(call, &pages),
        "azure.stage_block" => stage_block(&context, &account, call),
        "azure.commit_blocks" => commit_blocks(&context, &account, call),
        "azure.list_blocks" => list_blocks(&context, &account, call),
        "azure.set_tier" => set_tier(&context, &account, call),
        "azure.snapshot" => snapshot(&context, &account, call),
        "azure.set_metadata" => set_metadata(&context, &account, call),
        "azure.set_properties" => set_properties(&context, &account, call),
        "restore" => restore(&context, &account, call),
        "copy" => copy(&context, &account, call),
        "azure.put_from_url" => put_from_url(&context, &account, call),
        "azure.stage_block_from_url" => stage_block_from_url(&context, &account, call),
        "azure.abort_copy" => abort_copy(&context, &account, call),
        "azure.set_tags" => set_tags(&context, &account, call),
        "azure.get_tags" => get_tags(&context, &account, call),
        "delete_many" => delete_many(&context, &account, call),
        _ => Ok(unsupported_by_adapter("operation not mapped")),
    }
}

/// The result of a request that changes the object and returns nothing.
fn update_result(exchange: &HttpExchange, outcome: UpdateHeadOutcome<'_>) -> Value {
    let outcome = match outcome {
        UpdateHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_update_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    match outcome {
        UpdateHeadOutcome::Updated { meta } => match text_of(meta.e_tag) {
            Some(e_tag) => successful_result(json!({"etag": e_tag})),
            None => successful_result(json!({})),
        },
        UpdateHeadOutcome::PreconditionFailed => error_result(exchange, 412),
        UpdateHeadOutcome::NotFound { .. } => error_result(exchange, 404),
        UpdateHeadOutcome::NeedErrorBody(failure) | UpdateHeadOutcome::ServiceFailure(failure) => {
            error_result(exchange, failure.status)
        }
        _ => error_result(exchange, exchange.status),
    }
}

fn set_tier(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let item = ObjectItem::new(optional_text(call, "key").unwrap_or_default());
    let tier = optional_text(call, "tier").unwrap_or_default();
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_set_tier(container, token, namespace, tier, buf)
        },
        |template, bytes, buf| azure::encode_set_tier(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = crate_step!(azure::accept_set_tier_head(exchange.response_head()));
    Ok(update_result(&exchange, outcome))
}

fn snapshot(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let item = ObjectItem::new(optional_text(call, "key").unwrap_or_default());
    let options = azure::SnapshotOptions::default();
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_snapshot(container, token, namespace, &options, buf)
        },
        |template, bytes, buf| azure::encode_snapshot(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let condition = options.condition;
    let outcome = match crate_step!(azure::accept_snapshot_head(
        condition,
        exchange.response_head()
    )) {
        SnapshotHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_snapshot_error_body(condition, failure, &exchange.body)
        }
        outcome => outcome,
    };
    Ok(match outcome {
        SnapshotHeadOutcome::Created { snapshot, meta } => {
            let mut value = json!({
                "snapshot": snapshot,
                "etag": text_of(meta.e_tag).unwrap_or_default(),
            });
            if let Some(version) = text_of(meta.version) {
                value["version"] = json!(version);
            }
            successful_result(value)
        }
        SnapshotHeadOutcome::PreconditionFailed => error_result(&exchange, 412),
        SnapshotHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        SnapshotHeadOutcome::NeedErrorBody(failure)
        | SnapshotHeadOutcome::ServiceFailure(failure) => error_result(&exchange, failure.status),
        _ => error_result(&exchange, exchange.status),
    })
}

fn copy(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let (metadata, tags) = (requested_metadata(call), requested_tags(call));
    let Some((options, item)) = requested_copy(call, "source_container", "tier", &metadata, &tags)?
    else {
        return Ok(unsupported_by_crate(
            "a copy carries one condition on the target and one on the source",
        ));
    };
    // Copy Blob From URL answers once the copy is written.
    let sync = call.get("sync").and_then(Value::as_bool) == Some(true);
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| if sync {
            azure::prepare_copy_from_url(container, token, namespace, &options, buf)
        } else {
            azure::prepare_copy(container, token, namespace, &options, buf)
        },
        |template, bytes, buf| if sync {
            azure::encode_copy_from_url(template, bytes, &item, &now, buf)
        } else {
            azure::encode_copy(template, bytes, &item, &now, buf)
        },
        None,
        BodyRead::Hold,
    ));
    let shape = options.shape();
    let outcome = match crate_step!(azure::accept_copy_head(shape, exchange.response_head())) {
        CopyHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_copy_error_body(shape, failure, &exchange.body)
        }
        outcome => outcome,
    };
    Ok(match outcome {
        CopyHeadOutcome::Copied { meta } | CopyHeadOutcome::Pending { meta } => {
            let mut value = json!({"etag": text_of(meta.e_tag).unwrap_or_default()});
            if let Some(copy_status) = text_of(meta.copy_status) {
                value["copy_status"] = json!(copy_status);
            }
            if let Some(copy_id) = text_of(meta.copy_id) {
                value["copy_id"] = json!(copy_id);
            }
            if let Some(version) = text_of(meta.version) {
                value["version"] = json!(version);
            }
            successful_result(value)
        }
        CopyHeadOutcome::PreconditionFailed => error_result(&exchange, exchange.status),
        CopyHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        CopyHeadOutcome::NeedErrorBody(failure) | CopyHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn put_from_url(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let (metadata, tags) = (requested_metadata(call), requested_tags(call));
    let Some((options, item)) = requested_copy(call, "source_container", "tier", &metadata, &tags)?
    else {
        return Ok(unsupported_by_crate(
            "a copy carries one condition on the target and one on the source",
        ));
    };
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_put_from_url(container, token, namespace, &options, buf)
        },
        |template, bytes, buf| azure::encode_put_from_url(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let shape = options.shape();
    let outcome = match crate_step!(azure::accept_put_from_url_head(
        shape,
        exchange.response_head()
    )) {
        PutHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_put_from_url_error_body(shape, failure, &exchange.body)
        }
        outcome => outcome,
    };
    Ok(match outcome {
        PutHeadOutcome::Created { meta } => {
            let mut value = json!({"etag": text_of(meta.e_tag).unwrap_or_default()});
            if let Some(version) = text_of(meta.version) {
                value["version"] = json!(version);
            }
            successful_result(value)
        }
        PutHeadOutcome::PreconditionFailed => error_result(&exchange, exchange.status),
        PutHeadOutcome::NeedErrorBody(failure) | PutHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn stage_block_from_url(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((options, copy)) = requested_copy(call, "source_container", "tier", &[], &[])? else {
        return Ok(unsupported_by_crate(
            "a copy carries one condition on the source",
        ));
    };
    let item = azure::StageBlockFromUrlItem {
        copy,
        id: optional_text(call, "block_id_base64").unwrap_or_default(),
    };
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_stage_block_from_url(container, token, namespace, &options, buf)
        },
        |template, bytes, buf| {
            azure::encode_stage_block_from_url(template, bytes, &item, &now, buf)
        },
        None,
        BodyRead::Hold,
    ));
    let outcome = match crate_step!(azure::accept_stage_block_head(exchange.response_head())) {
        StageHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_stage_block_error_body(failure, &exchange.body)
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

fn abort_copy(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let item = azure::AbortCopyItem {
        key: optional_text(call, "key").unwrap_or_default(),
        copy_id: optional_text(call, "copy_id").unwrap_or_default(),
    };
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_abort_copy(container, token, namespace, buf)
        },
        |template, bytes, buf| azure::encode_abort_copy(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = crate_step!(azure::accept_abort_copy_head(exchange.response_head()));
    Ok(update_result(&exchange, outcome))
}

fn restore(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((options, item)) = requested_restore_options(call, None) else {
        return Ok(unsupported_by_adapter("priority not mapped"));
    };
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_restore(container, token, namespace, &options, buf)
        },
        |template, bytes, buf| azure::encode_restore(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = match crate_step!(azure::accept_restore_head(exchange.response_head())) {
        RestoreHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_restore_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    Ok(match (restore_value(&outcome), outcome) {
        (Some(value), _) => successful_result(value),
        (None, RestoreHeadOutcome::NotFound { .. }) => error_result(&exchange, 404),
        (
            None,
            RestoreHeadOutcome::NeedErrorBody(failure)
            | RestoreHeadOutcome::ServiceFailure(failure),
        ) => error_result(&exchange, failure.status),
        _ => error_result(&exchange, exchange.status),
    })
}

fn set_metadata(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((condition, _, item)) = requested_object(call) else {
        return Ok(unsupported_by_crate(
            "a Set Blob Metadata carries one precondition",
        ));
    };
    let metadata = requested_metadata(call);
    let options = azure::SetMetadataOptions {
        metadata: &metadata,
        condition,
    };
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_set_metadata(container, token, namespace, &options, buf)
        },
        |template, bytes, buf| azure::encode_set_metadata(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = crate_step!(azure::accept_set_metadata_head(
        condition,
        exchange.response_head()
    ));
    Ok(update_result(&exchange, outcome))
}

fn set_properties(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((condition, _, item)) = requested_object(call) else {
        return Ok(unsupported_by_crate(
            "a Set Blob Properties carries one precondition",
        ));
    };
    let options = azure::SetPropertiesOptions {
        properties: requested_properties(call),
        condition,
    };
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_set_properties(container, token, namespace, &options, buf)
        },
        |template, bytes, buf| azure::encode_set_properties(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = crate_step!(azure::accept_set_properties_head(
        condition,
        exchange.response_head()
    ));
    Ok(update_result(&exchange, outcome))
}

fn set_tags(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((_, revision, item)) = requested_object(call) else {
        return Ok(unsupported_by_crate("a Set Blob Tags carries no condition"));
    };
    let tags = requested_tags(call);
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_set_tags(container, token, namespace, &tags, revision, buf)
        },
        |template, bytes, buf| azure::encode_set_tags(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = crate_step!(azure::accept_set_tags_head(exchange.response_head()));
    Ok(update_result(&exchange, outcome))
}

fn get_tags(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let Some((_, revision, item)) = requested_object(call) else {
        return Ok(unsupported_by_crate("a Get Blob Tags carries no condition"));
    };
    let now = current_timestamps();
    let mut exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_get_tags(container, token, namespace, revision, buf)
        },
        |template, bytes, buf| azure::encode_get_tags(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = match crate_step!(azure::accept_get_tags_head(exchange.response_head())) {
        TagsHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_get_tags_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    let failed_status = match outcome {
        TagsHeadOutcome::Tags { .. } => None,
        TagsHeadOutcome::NotFound { .. } => Some(404),
        TagsHeadOutcome::NeedErrorBody(failure) | TagsHeadOutcome::ServiceFailure(failure) => {
            Some(failure.status)
        }
        _ => Some(exchange.status),
    };
    if let Some(status) = failed_status {
        return Ok(error_result(&exchange, status));
    }
    // An object holds at most ten tags.
    let mut tags = [Tag::default(); 10];
    let count = crate_step!(azure::fill_tags(&mut exchange.body, &mut tags));
    Ok(successful_result(
        json!({"tags": tags_value(&tags[..count])}),
    ))
}

fn delete_many(
    context: &AdapterContext,
    account: &Account<'_>,
    call: &Value,
) -> Result<Value, AdapterError> {
    let keys = requested_keys(call);
    let now = current_timestamps();
    let exchange = sent!(account.send(
        context,
        |container, token, namespace, buf| {
            azure::prepare_delete_many(container, token, namespace, buf)
        },
        |template, bytes, buf| azure::encode_delete_many(template, bytes, &keys, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = match crate_step!(azure::accept_delete_many_head(exchange.response_head())) {
        DeleteManyHeadOutcome::NeedErrorBody(failure) => {
            azure::accept_delete_many_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    let failed_status = match outcome {
        DeleteManyHeadOutcome::Results { .. } => None,
        DeleteManyHeadOutcome::NeedErrorBody(failure)
        | DeleteManyHeadOutcome::ServiceFailure(failure) => Some(failure.status),
        _ => Some(exchange.status),
    };
    if let Some(status) = failed_status {
        return Ok(error_result(&exchange, status));
    }
    let mut results = vec![BatchResult::default(); keys.len()];
    let count = crate_step!(azure::fill_delete_results(
        keys.len(),
        exchange.response_head(),
        &exchange.body,
        &mut results
    ));
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for (object, result) in keys.iter().zip(&results[..count]) {
        // Each key reports as the call named it: a plain key as its text,
        // and one with a version or a snapshot as an object.
        let mut named = json!({"key": object.key});
        match object.revision {
            Some(Revision::Version(version)) => named["version"] = json!(version),
            Some(Revision::Snapshot(snapshot)) => named["snapshot"] = json!(snapshot),
            _ => {}
        }
        if result.outcome == DeleteHeadOutcome::Accepted {
            deleted.push(match object.revision {
                Some(_) => named,
                None => json!(object.key),
            });
        } else {
            let code = azure::error_code(&result.head, result.body);
            named["status"] = json!(result.head.status);
            named["code"] = json!(code.map(String::from_utf8_lossy));
            errors.push(named);
        }
    }
    Ok(successful_result(
        json!({"deleted": deleted, "errors": errors}),
    ))
}
