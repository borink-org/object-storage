//! The S3 half of the adapter: `s3::Objects` for GET, HEAD, PUT, DELETE,
//! listing and uploads in parts, and for the SigV4 signatures of the vector
//! suite.
//!
//! The crate signs with long-lived or temporary credentials. Offline cases
//! receive placeholder keys, and live cases read `AWS_ACCESS_KEY_ID`,
//! `AWS_SECRET_ACCESS_KEY` and, if set, `AWS_SESSION_TOKEN`.

use crate::listing::{
    Listed, ListedPage, PageRead, PageSource, decoded_listing_text, entry_slots, list_all_keys,
    list_page,
};
use crate::{
    AdapterContext, AdapterError, HttpExchange, current_timestamps, decode_base64_field,
    failed_result, optional_text, read_meta_fields, request_buffers, requested_checksum,
    requested_condition, requested_keys, requested_metadata, requested_properties, requested_range,
    requested_restore, requested_revision, requested_source, requested_source_range,
    requested_tags, restore_value, send_request, served_range, successful_result, tags_value,
    text_of, transport_failure, two_checksums_refused, unmapped_call_field, unsupported_by_adapter,
    unsupported_by_crate,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use borink_object_storage_crypto::{
    CRC32, CRC32C, CRC64, MD5_RUSTCRYPTO, SHA1_RUSTCRYPTO, SHA256_CHECKSUM_RUSTCRYPTO,
    SHA256_RUSTCRYPTO, wipe,
};
use borink_object_storage_proto::s3::{
    self, Addressing, Bucket, CreateUploadHeadOutcome, DeleteResult, ObjectProperty, Objects, Part,
    PartRef, PayloadHash, PhysicalAbortUpload, PhysicalCreateUpload, PhysicalListParts,
    PhysicalStagePart, PhysicalStagePartCopy, PropertySet, Service, Session, SessionHeadOutcome,
};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{
    ChecksumKind, Classification, CommitHeadOutcome, CopyHeadOutcome, DeleteHeadOutcome,
    DeleteKind, DeleteManyHeadOutcome, EntryKind, GetHeadOutcome, GetKind, ListEntry,
    ListHeadOutcome, ListPartsHeadOutcome, Metadata, Payload, PhysicalCommit, PhysicalCopy,
    PhysicalDelete, PhysicalDeleteMany, PhysicalGet, PhysicalList, PhysicalPut, PhysicalSetTags,
    PutHeadOutcome, RequestedRange, RestoreHeadOutcome, StageHeadOutcome, Tag, TagsHeadOutcome,
    Timestamps, UpdateHeadOutcome, WriteOptions, layered,
};
use serde_json::{Map, Value, json};
use std::cell::OnceCell;

// AWS reports at most 1,000 entries on one listing page, and 1,000 parts on
// one page of a ListParts.
const MAX_PAGE_ENTRIES: usize = 1_000;

fn error_result(exchange: &HttpExchange, status: u16) -> Value {
    let kind = match s3::classify_error(&exchange.body, false) {
        Classification::Classified(kind) => Some(kind),
        _ => None,
    };
    failed_result(status, s3::error_code(&exchange.body), kind)
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

/// The clients of one case: the one of the case's own credentials, and for a
/// directory bucket the one of a session, which is opened for the first
/// request that needs it.
struct Client<'a> {
    context: AdapterContext,
    objects: Objects<'a>,
    directory: bool,
    session: OnceCell<Objects<'a>>,
}

impl<'a> Client<'a> {
    /// Returns the client that signs requests to the objects, or the result
    /// to report if no session could be opened.
    ///
    /// Call it after the crate checked the plan with `self.objects`, so that
    /// a refused plan sends no request, not even a CreateSession.
    fn signer(&self) -> Result<Result<Objects<'a>, Value>, AdapterError> {
        if !self.directory {
            return Ok(Ok(self.objects));
        }
        if let Some(objects) = self.session.get() {
            return Ok(Ok(*objects));
        }
        let mut session = match open_session(&self.context, &self.objects)? {
            Ok(session) => session,
            Err(result) => return Ok(Err(result)),
        };
        // A session whose expiration has passed signs nothing that S3
        // takes, so ask for another, once.
        let now = current_timestamps().unix();
        if session
            .expires_at
            .is_some_and(|expires_at| expires_at <= now)
        {
            session = match open_session(&self.context, &self.objects)? {
                Ok(session) => session,
                Err(result) => return Ok(Err(result)),
            };
        }
        let objects = match self.objects.with_session(session) {
            Ok(objects) => objects.with_signing_key(&current_timestamps()),
            Err(error) => return Ok(Err(crate::result_for_crate_error(error))),
        };
        Ok(Ok(*self.session.get_or_init(|| objects)))
    }
}

/// Sends a CreateSession and reads the credentials of the session, or
/// returns the result to report instead.
///
/// The session borrows the response body, which is leaked: the process
/// grades one case and exits.
fn open_session(
    context: &AdapterContext,
    objects: &Objects<'_>,
) -> Result<Result<Session<'static>, Value>, AdapterError> {
    let now = current_timestamps();
    let (mut request_bytes, mut header_spans) = request_buffers(page_step!(
        layered::s3::create_session_requirements(objects, &now)
    ));
    let request =
        page_step!(objects.encode_create_session(&mut request_bytes, &mut header_spans, &now));
    let exchange = match send_request(context, &request) {
        Ok(exchange) => exchange,
        Err(error) => return Ok(Err(transport_failure(&error))),
    };
    let head_outcome = page_step!(objects.accept_create_session_head(exchange.response_head()));
    let outcome = match head_outcome {
        SessionHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_create_session_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    let failed_status = match outcome {
        SessionHeadOutcome::Session { .. } => None,
        SessionHeadOutcome::NeedErrorBody(failure)
        | SessionHeadOutcome::ServiceFailure(failure) => Some(failure.status),
        _ => Some(exchange.status),
    };
    if let Some(status) = failed_status {
        return Ok(Err(error_result(&exchange, status)));
    }
    let body = Vec::leak(exchange.body);
    Ok(Ok(page_step!(objects.read_session(body))))
}

/// Returns early with the result to report if no session could be opened.
macro_rules! signer_step {
    ($client:expr) => {
        match $client.signer()? {
            Ok(objects) => objects,
            Err(result) => return Ok(result),
        }
    };
}

fn read_object(client: &Client<'_>, call: &Value, kind: GetKind) -> Result<Value, AdapterError> {
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
        revision: requested_revision(call),
    };

    let now = current_timestamps();
    crate_step!(layered::s3::get_requirements(
        &client.objects,
        &get_plan,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::get_requirements(objects, &get_plan, &now)
    ));
    let request =
        crate_step!(objects.encode_get(&mut request_bytes, &mut header_spans, &get_plan, &now));
    let exchange = transport_step!(send_request(&client.context, &request));

    let head_outcome =
        crate_step!(objects.accept_get_head(get_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        GetHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_get_error_body(get_plan.shape(), failure, &exchange.body)
        }
        outcome => outcome,
    };

    Ok(match outcome {
        GetHeadOutcome::Body { meta, .. } | GetHeadOutcome::Complete { meta } => {
            let mut value = json!({
                "etag": text_of(meta.e_tag).unwrap_or_default(),
                "metadata": metadata_from_headers(&exchange),
            });
            read_meta_fields(&mut value, &meta, "storage_class");
            if let GetHeadOutcome::Body { body, .. } = outcome
                && range != RequestedRange::Whole
                && let Some(served) = served_range(&body)
            {
                value["content_range"] = json!(served);
            }
            if kind == GetKind::Head {
                value["size"] = json!(meta.size.unwrap_or(0));
            } else {
                value["body_base64"] = json!(STANDARD.encode(&exchange.body));
                value["size"] = json!(exchange.body.len());
            }
            successful_result(value)
        }
        GetHeadOutcome::NotModified { .. } => error_result(&exchange, 304),
        GetHeadOutcome::PreconditionFailed => error_result(&exchange, 412),
        // S3 answers a HEAD in a missing bucket with the bare 404 that it
        // answers for a missing key, so which is missing is not known.
        GetHeadOutcome::NotFound { kind: None } if kind == GetKind::Head => {
            let mut result = error_result(&exchange, 404);
            result["kind"] = json!("missing");
            result
        }
        GetHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        GetHeadOutcome::RangeNotSatisfiable { .. } => error_result(&exchange, 416),
        GetHeadOutcome::NeedErrorBody(failure) | GetHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn write_object(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    if call.get("checksums").is_some() {
        return Ok(two_checksums_refused());
    }
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate("PhysicalPut carries one precondition"));
    };
    // S3 takes every checksum that the crate has, as text or computed.
    let checksum = match requested_checksum(call) {
        None => None,
        Some(Some(checksum)) => Some(checksum),
        Some(None) => return Ok(unsupported_by_adapter("checksum algorithm not mapped")),
    };

    let metadata_pairs = requested_metadata(call);

    let key = optional_text(call, "key").unwrap_or_default();
    let body = decode_base64_field(call, "body_base64")?;
    let tags = requested_tags(call);
    let put_plan = PhysicalPut {
        key,
        condition,
        condition_value,
        metadata: &metadata_pairs,
        options: WriteOptions {
            checksum,
            properties: requested_properties(call),
            tags: &tags,
            storage_class: optional_text(call, "storage_class"),
            ..WriteOptions::default()
        },
    };
    let payload = Payload::Slice(&body);

    let now = current_timestamps();
    crate_step!(layered::s3::put_requirements(
        &client.objects,
        &put_plan,
        payload,
        PayloadHash::Compute,
        &now
    ));
    let objects = &signer_step!(client);
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
    let exchange = transport_step!(send_request(&client.context, &request));

    let head_outcome =
        crate_step!(objects.accept_put_head(put_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        PutHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_put_error_body(put_plan.shape(), failure, &exchange.body)
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

fn delete_object(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
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
        revision: requested_revision(call),
    };

    let now = current_timestamps();
    crate_step!(layered::s3::delete_requirements(
        &client.objects,
        &delete_plan,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::delete_requirements(objects, &delete_plan, &now)
    ));
    let request = crate_step!(objects.encode_delete(
        &mut request_bytes,
        &mut header_spans,
        &delete_plan,
        &now
    ));
    let exchange = transport_step!(send_request(&client.context, &request));

    let head_outcome =
        crate_step!(objects.accept_delete_head(delete_plan.shape(), exchange.response_head()));
    let outcome = match head_outcome {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_delete_error_body(delete_plan.shape(), failure, &exchange.body)
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

fn create_upload(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let key = optional_text(call, "key").unwrap_or_default();
    let tags = requested_tags(call);
    let create_plan = PhysicalCreateUpload {
        options: WriteOptions {
            properties: requested_properties(call),
            tags: &tags,
            storage_class: optional_text(call, "storage_class"),
            ..WriteOptions::default()
        },
        ..PhysicalCreateUpload::new(key)
    };

    let now = current_timestamps();
    crate_step!(layered::s3::create_upload_requirements(
        &client.objects,
        &create_plan,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::create_upload_requirements(objects, &create_plan, &now)
    ));
    let request = crate_step!(objects.encode_create_upload(
        &mut request_bytes,
        &mut header_spans,
        &create_plan,
        &now
    ));
    let mut exchange = transport_step!(send_request(&client.context, &request));

    let head_outcome = crate_step!(objects.accept_create_upload_head(exchange.response_head()));
    let outcome = match head_outcome {
        CreateUploadHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_create_upload_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    let failed_status = match outcome {
        CreateUploadHeadOutcome::Created { .. } => None,
        CreateUploadHeadOutcome::NeedErrorBody(failure)
        | CreateUploadHeadOutcome::ServiceFailure(failure) => Some(failure.status),
        _ => Some(exchange.status),
    };
    if let Some(status) = failed_status {
        return Ok(error_result(&exchange, status));
    }

    let upload_id = crate_step!(objects.read_upload_id(&mut exchange.body));
    Ok(successful_result(json!({"upload_id": upload_id})))
}

fn stage_part(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let key = optional_text(call, "key").unwrap_or_default();
    let upload_id = optional_text(call, "upload_id").unwrap_or_default();
    // A number that no `u32` holds is above every limit, as `u32::MAX` is.
    let number = call
        .get("part_number")
        .and_then(Value::as_u64)
        .ok_or("missing part_number")?;
    let stage_plan =
        PhysicalStagePart::new(key, upload_id, u32::try_from(number).unwrap_or(u32::MAX));
    let body = decode_base64_field(call, "body_base64")?;
    let payload = Payload::Slice(&body);

    let now = current_timestamps();
    crate_step!(layered::s3::stage_part_requirements(
        &client.objects,
        &stage_plan,
        payload,
        PayloadHash::Compute,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) =
        request_buffers(crate_step!(layered::s3::stage_part_requirements(
            objects,
            &stage_plan,
            payload,
            PayloadHash::Compute,
            &now
        )));
    let request = crate_step!(objects.encode_stage_part(
        &mut request_bytes,
        &mut header_spans,
        &stage_plan,
        payload,
        PayloadHash::Compute,
        &now
    ));
    let exchange = transport_step!(send_request(&client.context, &request));

    let head_outcome = crate_step!(objects.accept_stage_part_head(exchange.response_head()));
    let outcome = match head_outcome {
        StageHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_stage_part_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };

    Ok(match outcome {
        StageHeadOutcome::Staged { e_tag } => {
            successful_result(json!({"etag": text_of(e_tag).unwrap_or_default()}))
        }
        StageHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        StageHeadOutcome::NeedErrorBody(failure) | StageHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn copy(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate(
            "PhysicalCopy carries one condition on the target",
        ));
    };
    let Some(source) = requested_source(call, "source_bucket") else {
        return Ok(unsupported_by_crate("CopySource carries one condition"));
    };
    let (metadata, tags) = (requested_metadata(call), requested_tags(call));
    let plan = PhysicalCopy {
        condition,
        condition_value,
        metadata: &metadata,
        options: WriteOptions {
            properties: requested_properties(call),
            tags: &tags,
            storage_class: optional_text(call, "storage_class"),
            ..WriteOptions::default()
        },
        ..PhysicalCopy::new(optional_text(call, "key").unwrap_or_default(), source)
    };

    let now = current_timestamps();
    // AWS authorizes a copy by the caller's own credentials, never those of
    // a session.
    let objects = &client.objects;
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::copy_requirements(objects, &plan, &now)
    ));
    let request =
        crate_step!(objects.encode_copy(&mut request_bytes, &mut header_spans, &plan, &now));
    let mut exchange = transport_step!(send_request(&client.context, &request));

    let shape = plan.shape();
    let (head, body) = exchange.head_and_body();
    let outcome = match crate_step!(objects.accept_copy_head(shape, head)) {
        CopyHeadOutcome::NeedResultBody { .. } => {
            crate_step!(objects.accept_copy_body(shape, head, body))
        }
        CopyHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_copy_error_body(shape, failure, body)
        }
        outcome => outcome,
    };

    // S3 can refuse a copy under status 200, which the result reports. The
    // outcome borrows the body, so the status of a failure is taken first.
    let copied = match outcome {
        CopyHeadOutcome::Copied { meta } => {
            let mut value = json!({"etag": text_of(meta.e_tag).unwrap_or_default()});
            if let Some(version) = text_of(meta.version) {
                value["version"] = json!(version);
            }
            Ok(value)
        }
        CopyHeadOutcome::PreconditionFailed => Err(Some(412)),
        CopyHeadOutcome::NotFound { .. } => Err(Some(404)),
        CopyHeadOutcome::NeedErrorBody(failure) | CopyHeadOutcome::ServiceFailure(failure) => {
            Err(Some(failure.status))
        }
        _ => Err(None),
    };
    Ok(match copied {
        Ok(value) => successful_result(value),
        Err(status) => error_result(&exchange, status.unwrap_or(exchange.status)),
    })
}

fn stage_part_copy(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let Some(source) = requested_source(call, "source_bucket") else {
        return Ok(unsupported_by_crate("CopySource carries one condition"));
    };
    let number = call
        .get("part_number")
        .and_then(Value::as_u64)
        .ok_or("part copy without part_number")?;
    let plan = PhysicalStagePartCopy {
        range: requested_source_range(call)?,
        ..PhysicalStagePartCopy::new(
            optional_text(call, "key").unwrap_or_default(),
            optional_text(call, "upload_id").unwrap_or_default(),
            u32::try_from(number).unwrap_or(u32::MAX),
            source,
        )
    };

    let now = current_timestamps();
    // AWS authorizes a copy by the caller's own credentials, never those of
    // a session.
    let objects = &client.objects;
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::stage_part_copy_requirements(objects, &plan, &now)
    ));
    let request = crate_step!(objects.encode_stage_part_copy(
        &mut request_bytes,
        &mut header_spans,
        &plan,
        &now
    ));
    let mut exchange = transport_step!(send_request(&client.context, &request));

    let (head, body) = exchange.head_and_body();
    let outcome = match crate_step!(objects.accept_stage_part_copy_head(head)) {
        StageHeadOutcome::NeedResultBody { .. } => {
            crate_step!(objects.accept_stage_part_copy_body(head, body))
        }
        StageHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_stage_part_error_body(failure, body)
        }
        outcome => outcome,
    };

    // The outcome borrows the body, so the status of a failure is taken
    // first.
    let staged = match outcome {
        StageHeadOutcome::Staged { e_tag } => {
            Ok(json!({"etag": text_of(e_tag).unwrap_or_default()}))
        }
        StageHeadOutcome::NotFound { .. } => Err(Some(404)),
        StageHeadOutcome::NeedErrorBody(failure) | StageHeadOutcome::ServiceFailure(failure) => {
            Err(Some(failure.status))
        }
        _ => Err(None),
    };
    Ok(match staged {
        Ok(value) => successful_result(value),
        Err(status) => error_result(&exchange, status.unwrap_or(exchange.status)),
    })
}

fn commit_parts(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let Some((condition, condition_value)) = requested_condition(call) else {
        return Ok(unsupported_by_crate(
            "PhysicalCommit carries one precondition",
        ));
    };
    let mut part_references = Vec::new();
    for part in call
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let number = part
            .get("number")
            .and_then(Value::as_u64)
            .ok_or("part without number")?;
        part_references.push(PartRef {
            number: u32::try_from(number).unwrap_or(u32::MAX),
            e_tag: optional_text(part, "etag").unwrap_or_default().as_bytes(),
        });
    }

    let key = optional_text(call, "key").unwrap_or_default();
    let upload_id = optional_text(call, "upload_id").unwrap_or_default();
    let commit_plan = PhysicalCommit {
        condition,
        condition_value,
        size: call.get("object_size").and_then(Value::as_u64),
        ..PhysicalCommit::new(key)
    };

    let now = current_timestamps();
    crate_step!(layered::s3::commit_parts_requirements(
        &client.objects,
        &commit_plan,
        upload_id,
        &part_references,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) =
        request_buffers(crate_step!(layered::s3::commit_parts_requirements(
            objects,
            &commit_plan,
            upload_id,
            &part_references,
            &now
        )));
    let request = crate_step!(objects.encode_commit_parts(
        &mut request_bytes,
        &mut header_spans,
        &commit_plan,
        upload_id,
        &part_references,
        &now
    ));
    let mut exchange = transport_step!(send_request(&client.context, &request));

    let shape = commit_plan.shape();
    let (head, body) = exchange.head_and_body();
    let outcome = match crate_step!(objects.accept_commit_parts_head(shape, head)) {
        CommitHeadOutcome::NeedResultBody { .. } => {
            crate_step!(objects.accept_commit_parts_body(shape, head, body))
        }
        CommitHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_commit_parts_error_body(shape, failure, body)
        }
        outcome => outcome,
    };

    // S3 can refuse a commit under status 200, which the result reports.
    Ok(match outcome {
        CommitHeadOutcome::Committed { meta } => {
            let mut value = json!({"etag": text_of(meta.e_tag).unwrap_or_default()});
            if let Some(version) = text_of(meta.version) {
                value["version"] = json!(version);
            }
            successful_result(value)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn abort_upload(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let key = optional_text(call, "key").unwrap_or_default();
    let upload_id = optional_text(call, "upload_id").unwrap_or_default();
    let abort_plan = PhysicalAbortUpload::new(key, upload_id);

    let now = current_timestamps();
    crate_step!(layered::s3::abort_upload_requirements(
        &client.objects,
        &abort_plan,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::abort_upload_requirements(objects, &abort_plan, &now)
    ));
    let request = crate_step!(objects.encode_abort_upload(
        &mut request_bytes,
        &mut header_spans,
        &abort_plan,
        &now
    ));
    let exchange = transport_step!(send_request(&client.context, &request));

    let head_outcome = crate_step!(objects.accept_abort_upload_head(exchange.response_head()));
    let outcome = match head_outcome {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_abort_upload_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };

    Ok(match outcome {
        DeleteHeadOutcome::Accepted => successful_result(json!({})),
        DeleteHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        DeleteHeadOutcome::NeedErrorBody(failure) | DeleteHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

/// Lists every part of an upload, one page after another.
fn list_parts(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let key = optional_text(call, "key").unwrap_or_default();
    let upload_id = optional_text(call, "upload_id").unwrap_or_default();

    let mut listed = Vec::new();
    let mut marker: Option<String> = None;
    loop {
        let list_plan = PhysicalListParts {
            marker: marker.as_deref(),
            ..PhysicalListParts::new(key, upload_id)
        };
        let now = current_timestamps();
        crate_step!(layered::s3::list_parts_requirements(
            &client.objects,
            &list_plan,
            &now
        ));
        let objects = &signer_step!(client);
        let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
            layered::s3::list_parts_requirements(objects, &list_plan, &now)
        ));
        let request = crate_step!(objects.encode_list_parts(
            &mut request_bytes,
            &mut header_spans,
            &list_plan,
            &now
        ));
        let mut exchange = transport_step!(send_request(&client.context, &request));

        let head_outcome = crate_step!(objects.accept_list_parts_head(exchange.response_head()));
        let outcome = match head_outcome {
            ListPartsHeadOutcome::NeedErrorBody(failure) => {
                objects.accept_list_parts_error_body(failure, &exchange.body)
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

        let mut parts = vec![Part::default(); MAX_PAGE_ENTRIES];
        let page = crate_step!(objects.fill_parts(&mut exchange.body, &mut parts));
        listed.extend(
            parts[..page.filled]
                .iter()
                .map(|part| json!({"number": part.number, "size": part.size, "etag": part.e_tag})),
        );
        // A page that names itself as the next one would never end.
        match page.next_marker {
            Some(next) if marker.as_deref() != Some(next) => marker = Some(next.to_owned()),
            Some(_) => return Err("ListParts named the same page twice".into()),
            None => break,
        }
    }
    Ok(successful_result(json!({"parts": listed})))
}

/// The pages of the bucket's listings.
impl PageSource for Client<'_> {
    fn read_page(&self, list_plan: &PhysicalList<'_>) -> PageRead {
        let now = current_timestamps();
        page_step!(layered::s3::list_requirements(
            &self.objects,
            list_plan,
            &now
        ));
        let objects = &match self.signer()? {
            Ok(objects) => objects,
            Err(result) => return Ok(Err(result)),
        };
        let context = &self.context;
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
                objects.accept_list_error_body(failure, &exchange.body)
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

        let mut slots: Vec<ObjectWithProperties<'_>> = entry_slots(list_plan, MAX_PAGE_ENTRIES);
        // These properties are read in the same pass as the page.
        let wanted = PropertySet::of(&[
            ObjectProperty::StorageClass,
            ObjectProperty::ChecksumAlgorithm,
            ObjectProperty::Owner,
        ]);
        let listing = page_step!(objects.fill_listing_with(
            &mut exchange.body,
            &mut slots,
            wanted,
            |entry, values| ObjectWithProperties {
                entry,
                storage_class: values.get(ObjectProperty::StorageClass),
                checksum_algorithm: values.get(ObjectProperty::ChecksumAlgorithm),
                owner: values.get(ObjectProperty::Owner),
            }
        ));
        Ok(Ok(ListedPage::read(&slots, listing)))
    }
}

/// A listed object, with the properties that the page was read for.
#[derive(Clone, Copy, Default)]
struct ObjectWithProperties<'b> {
    entry: ListEntry<'b>,
    storage_class: Option<&'b [u8]>,
    checksum_algorithm: Option<&'b [u8]>,
    owner: Option<&'b [u8]>,
}

impl Listed for ObjectWithProperties<'_> {
    fn entry(&self) -> &ListEntry<'_> {
        &self.entry
    }

    fn value(&self) -> Value {
        listed_entry_value(self)
    }
}

fn listed_entry_value(listed: &ObjectWithProperties<'_>) -> Value {
    let entry = &listed.entry;
    let mut value = json!({
        "key": entry.key,
        "size": entry.size.unwrap_or(0),
        "etag": entry.e_tag.unwrap_or_default(),
    });
    // An entry of a listing of versions names its version, and whether it
    // is the latest or a delete marker.
    if let Some(version) = entry.version() {
        value["version"] = json!(decoded_listing_text(version));
        value["is_current_version"] = json!(entry.is_current_version());
    }
    if entry.kind == EntryKind::DeleteMarker {
        value["delete_marker"] = json!(true);
    }
    if let Some(millis) = entry.last_modified.and_then(layered::iso8601_ms) {
        value["last_modified"] = json!(rfc3339(millis / 1000));
    }
    if let Some(storage_class) = listed.storage_class {
        value["storage_class"] = json!(decoded_listing_text(storage_class));
    }
    if let Some(algorithm) = listed.checksum_algorithm {
        value["checksum_algorithm"] = json!(decoded_listing_text(algorithm));
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
    // An S3 Express session signs for a directory bucket, and the crate
    // derives the service name from the bucket.
    let service = match (
        optional_text(call, "signing_algorithm"),
        optional_text(call, "service"),
    ) {
        (None | Some("sigv4"), None | Some("s3")) => Service::Aws,
        (Some("sigv4_s3express"), None | Some("s3express")) => Service::AwsDirectory,
        _ => return Ok(unsupported_by_adapter("signing algorithm not mapped")),
    };
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

    let bucket = crate_step!(Bucket::new(&endpoint, bucket_name, region, service))
        .with_addressing(Addressing::VirtualHosted);
    let mut credentials = crate_step!(Credentials::new(key_id, secret, wipe));
    if let Some(token) = session_token
        && service == Service::Aws
    {
        credentials = crate_step!(credentials.with_session_token(token));
    }
    let objects = Objects::new(bucket, credentials, SHA256_RUSTCRYPTO);
    // The credentials of the case are those of a session that CreateSession
    // returned.
    let objects = match session_token {
        Some(token) if service == Service::AwsDirectory => {
            crate_step!(objects.with_session(Session {
                key_id,
                secret,
                token,
                expires_at: None,
            }))
        }
        _ => objects,
    };
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
        "get" => &[
            "key",
            "range",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "version",
        ],
        "head" => &[
            "key",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "version",
        ],
        "put" => &[
            "key",
            "body_base64",
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
            "storage_class",
        ],
        "delete" => &[
            "key",
            "if_match",
            "if_none_match",
            "if_modified_since",
            "if_unmodified_since",
            "version",
        ],
        "list" => &["prefix", "page_size"],
        "list_page" => &[
            "prefix",
            "continuation_token",
            "version_marker",
            "include",
            "start_after",
            "delimiter",
            "page_size",
            "fetch_owner",
        ],
        "s3.create_multipart" => &[
            "key",
            "content_type",
            "content_encoding",
            "content_language",
            "content_disposition",
            "cache_control",
            "tags",
            "storage_class",
        ],
        "s3.upload_part" => &["key", "upload_id", "part_number", "body_base64"],
        "copy" => &[
            "key",
            "source_key",
            "source_bucket",
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
            "storage_class",
        ],
        "s3.upload_part_copy" => &[
            "key",
            "upload_id",
            "part_number",
            "source_range",
            "source_key",
            "source_bucket",
            "source_version",
            "source_if_match",
            "source_if_none_match",
            "source_if_modified_since",
            "source_if_unmodified_since",
        ],
        "s3.complete_multipart" => &[
            "key",
            "upload_id",
            "parts",
            "if_match",
            "if_none_match",
            "object_size",
        ],
        "s3.abort_multipart" => &["key", "upload_id"],
        "s3.list_parts" => &["key", "upload_id"],
        "s3.put_tagging" => &["key", "tags", "version"],
        "s3.get_tagging" => &["key", "version"],
        "restore" => &["key", "version", "priority", "days", "tier"],
        "delete_many" => &["keys"],
        _ => &[],
    }
}

/// Returns the reason the crate cannot express an S3 call field, for the
/// fields it lacks.
fn crate_limitation(field: &str) -> Option<&'static str> {
    match field {
        "expires" => Some("ContentProperties carries no Expires"),
        "tagging" => Some("Tag holds a tag as a key and a value"),
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
    if operation != "s3.create_session" && mapped_call_fields(operation).is_empty() {
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
    let service = if endpoint
        .get("directory_bucket")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        Service::AwsDirectory
    } else if operation == "s3.create_session" {
        return Ok(unsupported_by_crate(
            "only a directory bucket creates a session",
        ));
    } else {
        Service::Aws
    };
    let is_live = message.get("mode").and_then(Value::as_str) == Some("live");
    let invalid_credentials =
        call.get("credential_mode").and_then(Value::as_str) == Some("invalid");
    let (key_id, secret, session_token) = if !is_live {
        // The S3 Express cases expect this key ID.
        (
            "offline".to_owned(),
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
    let bucket = crate_step!(Bucket::new(endpoint_url, bucket_name, region, service));
    let mut credentials = crate_step!(Credentials::new(&key_id, &secret, wipe));
    if let Some(token) = &session_token {
        credentials = crate_step!(credentials.with_session_token(token));
    }
    let now = current_timestamps();
    let objects = Objects::new(bucket, credentials, SHA256_RUSTCRYPTO)
        .with_signing_key(&now)
        .with_checksum(MD5_RUSTCRYPTO)
        .with_checksum(CRC64)
        .with_checksum(CRC32)
        .with_checksum(CRC32C)
        .with_checksum(SHA1_RUSTCRYPTO)
        .with_checksum(SHA256_CHECKSUM_RUSTCRYPTO);

    let context = AdapterContext::for_endpoint(endpoint)?;
    if operation == "s3.create_session" {
        return Ok(match open_session(&context, &objects)? {
            Ok(session) => successful_result(json!({
                "has_access_key": !session.key_id.is_empty(),
                "has_secret_key": !session.secret.is_empty(),
                "has_session_token": !session.token.is_empty(),
            })),
            Err(result) => result,
        });
    }
    let client = Client {
        context,
        objects,
        directory: service == Service::AwsDirectory,
        session: OnceCell::new(),
    };
    match operation {
        "get" => read_object(&client, call, GetKind::Bytes),
        "head" => read_object(&client, call, GetKind::Head),
        "put" => write_object(&client, call),
        "delete" => delete_object(&client, call),
        "list" => list_all_keys(call, &client),
        "list_page" => list_page(call, &client),
        "s3.create_multipart" => create_upload(&client, call),
        "s3.upload_part" => stage_part(&client, call),
        "copy" => copy(&client, call),
        "s3.upload_part_copy" => stage_part_copy(&client, call),
        "s3.complete_multipart" => commit_parts(&client, call),
        "s3.abort_multipart" => abort_upload(&client, call),
        "s3.list_parts" => list_parts(&client, call),
        "s3.put_tagging" => put_tagging(&client, call),
        "s3.get_tagging" => get_tagging(&client, call),
        "restore" => restore(&client, call),
        "delete_many" => delete_many(&client, call),
        _ => Ok(unsupported_by_adapter("operation not mapped")),
    }
}

fn restore(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let Some(plan) = requested_restore(call) else {
        return Ok(unsupported_by_adapter("priority not mapped"));
    };
    let now = current_timestamps();
    crate_step!(layered::s3::restore_requirements(
        &client.objects,
        &plan,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::restore_requirements(objects, &plan, &now)
    ));
    let request =
        crate_step!(objects.encode_restore(&mut request_bytes, &mut header_spans, &plan, &now));
    let exchange = transport_step!(send_request(&client.context, &request));
    let outcome = match crate_step!(objects.accept_restore_head(exchange.response_head())) {
        RestoreHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_restore_error_body(failure, &exchange.body)
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

fn put_tagging(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let key = optional_text(call, "key").unwrap_or_default();
    let tags = requested_tags(call);
    let plan = PhysicalSetTags {
        checksum: Some(ChecksumKind::Crc64),
        revision: requested_revision(call),
        ..PhysicalSetTags::new(key, &tags)
    };
    let now = current_timestamps();
    crate_step!(layered::s3::put_tagging_requirements(
        &client.objects,
        &plan,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::put_tagging_requirements(objects, &plan, &now)
    ));
    let request =
        crate_step!(objects.encode_put_tagging(&mut request_bytes, &mut header_spans, &plan, &now));
    let exchange = transport_step!(send_request(&client.context, &request));
    let outcome = match crate_step!(objects.accept_put_tagging_head(exchange.response_head())) {
        UpdateHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_update_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    Ok(match outcome {
        UpdateHeadOutcome::Updated => successful_result(json!({})),
        UpdateHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        UpdateHeadOutcome::NeedErrorBody(failure) | UpdateHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn get_tagging(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let key = optional_text(call, "key").unwrap_or_default();
    let now = current_timestamps();
    let revision = requested_revision(call);
    crate_step!(layered::s3::get_tagging_requirements(
        &client.objects,
        key,
        revision,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::get_tagging_requirements(objects, key, revision, &now)
    ));
    let request = crate_step!(objects.encode_get_tagging(
        &mut request_bytes,
        &mut header_spans,
        key,
        revision,
        &now
    ));
    let mut exchange = transport_step!(send_request(&client.context, &request));
    let outcome = match crate_step!(objects.accept_get_tagging_head(exchange.response_head())) {
        TagsHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_get_tagging_error_body(failure, &exchange.body)
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
    // AWS holds at most ten tags on an object.
    let mut tags = [Tag::default(); 10];
    let count = crate_step!(objects.fill_tags(&mut exchange.body, &mut tags));
    Ok(successful_result(
        json!({"tags": tags_value(&tags[..count])}),
    ))
}

fn delete_many(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let keys = requested_keys(call);
    let plan = PhysicalDeleteMany {
        checksum: Some(ChecksumKind::Crc64),
        ..PhysicalDeleteMany::new(&keys)
    };
    let now = current_timestamps();
    crate_step!(layered::s3::delete_many_requirements(
        &client.objects,
        &plan,
        &now
    ));
    let objects = &signer_step!(client);
    let (mut request_bytes, mut header_spans) = request_buffers(crate_step!(
        layered::s3::delete_many_requirements(objects, &plan, &now)
    ));
    let request =
        crate_step!(objects.encode_delete_many(&mut request_bytes, &mut header_spans, &plan, &now));
    let mut exchange = transport_step!(send_request(&client.context, &request));
    let outcome = match crate_step!(objects.accept_delete_many_head(exchange.response_head())) {
        DeleteManyHeadOutcome::NeedErrorBody(failure) => {
            objects.accept_delete_many_error_body(failure, &exchange.body)
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
    let mut results = vec![DeleteResult::default(); keys.len()];
    let count = crate_step!(objects.fill_delete_results(&mut exchange.body, &mut results));
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for result in &results[..count] {
        // A plain key reports as its key, and a removal that names or
        // writes a version as an object.
        let versioned = result.version.is_some() || result.delete_marker;
        let mut value = json!({"key": result.key});
        if let Some(version) = result.version {
            value["version"] = json!(version);
        }
        if result.delete_marker {
            value["delete_marker"] = json!(true);
        }
        if let Some(version) = result.delete_marker_version {
            value["delete_marker_version"] = json!(version);
        }
        match result.code {
            None if versioned => deleted.push(value),
            None => deleted.push(json!(result.key)),
            Some(code) => {
                value["code"] = json!(code);
                errors.push(value);
            }
        }
    }
    Ok(successful_result(
        json!({"deleted": deleted, "errors": errors}),
    ))
}
