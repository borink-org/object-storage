//! The S3 half of the adapter: a template for each call, and a request
//! written and signed from it, including the SigV4 signatures of the vector
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
    AdapterContext, AdapterError, BodyRead, CallBody, HttpExchange, checksum_value,
    current_timestamps, decode_base64_field, failed_result, optional_text, owned, read_body_fields,
    read_meta_fields, requested_checksum_kind, requested_condition, requested_copy,
    requested_delete, requested_get, requested_keys, requested_metadata, requested_object,
    requested_properties, requested_restore_options, requested_tags, required, restore_value,
    send_request, send_request_with, served_range, successful_result, tags_value, text_of,
    transport_failure, two_checksums_refused, unmapped_call_field, unsupported_by_adapter,
    unsupported_by_crate,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use borink_object_storage_crypto::{Sha256RustCrypto, sigv4, wipe};
use borink_object_storage_proto::s3::{
    self, Addressing, Bucket, CreateUploadHeadOutcome, DeleteResult, ObjectProperty, Part, PartRef,
    PayloadSigning, PropertySet, Service, Session, SessionHeadOutcome, Template, Unsigned,
};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{
    ChecksumKind, Classification, CommitHeadOutcome, CopyHeadOutcome, DeleteHeadOutcome,
    DeleteManyHeadOutcome, DeleteOptions, EntryKind, Error as CrateError, GetHeadOutcome, GetItem,
    GetKind, GetOptions, ListEntry, ListHeadOutcome, ListInclude, ListItem, ListMarker,
    ListOptions, ListPartsHeadOutcome, Metadata, ObjectItem, Payload, PutHeadOutcome, PutItem,
    PutOptions, Ranging, RequestedRange, RestoreHeadOutcome, Revision, RevisionKind,
    StageHeadOutcome, StageOptions, Tag, TagsHeadOutcome, Timestamps, UpdateHeadOutcome,
    WireRequest, WriteOptions, iso8601_ms,
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

/// The bucket and the credentials of one case, and for a directory bucket
/// the credentials of a session, which are opened for the first request
/// that needs them.
struct Client<'a> {
    context: AdapterContext,
    bucket: Bucket<'a>,
    credentials: Credentials<'a>,
    directory: bool,
    session_credentials: OnceCell<Credentials<'static>>,
}

impl<'a> Client<'a> {
    /// Returns the credentials that sign requests to the objects: the
    /// case's own, or a session's for a directory bucket, which the first
    /// call opens. Call it after the crate checked the request with the
    /// case's own credentials, so that a refused request sends nothing, not
    /// even a CreateSession.
    fn signing_credentials(&self) -> Result<Result<Credentials<'_>, Value>, AdapterError> {
        if !self.directory {
            return Ok(Ok(self.credentials));
        }
        if let Some(credentials) = self.session_credentials.get() {
            return Ok(Ok(*credentials));
        }
        let open = || open_session(&self.context, &self.bucket, &self.credentials);
        let mut session = match open()? {
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
            session = match open()? {
                Ok(session) => session,
                Err(result) => return Ok(Err(result)),
            };
        }
        match session.credentials(wipe) {
            Ok(credentials) => Ok(Ok(*self.session_credentials.get_or_init(|| credentials))),
            Err(error) => Ok(Err(crate::result_for_crate_error(error))),
        }
    }

    /// Writes, signs and sends one request from the template that
    /// `prepare` writes and the request that `encode` writes from it.
    ///
    /// The crate checks the request against the case's own credentials
    /// first, so that a refused request sends nothing, not even a
    /// CreateSession.
    fn send<'c>(
        &self,
        prepare: impl Fn(&Bucket<'_>, &Credentials<'_>, &mut [u8]) -> Result<Template, CrateError>,
        encode: impl Fn(&Template, &[u8], &mut [u8]) -> Result<Unsigned<'c>, CrateError>,
        body: Option<&CallBody>,
        read: BodyRead<'_>,
    ) -> Result<Result<(Template, HttpExchange), Value>, AdapterError> {
        self.send_signed(false, prepare, encode, body, read)
    }

    /// Writes, signs and sends one request as `send` does, with the case's
    /// own credentials even for a directory bucket: AWS authorizes a copy
    /// by them, never by those of a session.
    fn send_own<'c>(
        &self,
        prepare: impl Fn(&Bucket<'_>, &Credentials<'_>, &mut [u8]) -> Result<Template, CrateError>,
        encode: impl Fn(&Template, &[u8], &mut [u8]) -> Result<Unsigned<'c>, CrateError>,
        body: Option<&CallBody>,
        read: BodyRead<'_>,
    ) -> Result<Result<(Template, HttpExchange), Value>, AdapterError> {
        self.send_signed(true, prepare, encode, body, read)
    }

    fn send_signed<'c>(
        &self,
        own: bool,
        prepare: impl Fn(&Bucket<'_>, &Credentials<'_>, &mut [u8]) -> Result<Template, CrateError>,
        encode: impl Fn(&Template, &[u8], &mut [u8]) -> Result<Unsigned<'c>, CrateError>,
        body: Option<&CallBody>,
        read: BodyRead<'_>,
    ) -> Result<Result<(Template, HttpExchange), Value>, AdapterError> {
        let now = current_timestamps();
        let mut scratch = Vec::new();
        let checked = signed(
            &self.bucket,
            &self.credentials,
            &now,
            &prepare,
            &encode,
            &mut scratch,
        )
        .map(drop);
        if let Err(error) = checked {
            return Ok(Err(crate::result_for_crate_error(error)));
        }
        let credentials = match own {
            true => self.credentials,
            false => match self.signing_credentials()? {
                Ok(credentials) => credentials,
                Err(result) => return Ok(Err(result)),
            },
        };
        let mut buf = Vec::new();
        let (template, request) = match signed(
            &self.bucket,
            &credentials,
            &now,
            &prepare,
            &encode,
            &mut buf,
        ) {
            Ok(signed) => signed,
            Err(error) => return Ok(Err(crate::result_for_crate_error(error))),
        };
        match send_request_with(&self.context, &request, body, read) {
            Ok(exchange) => Ok(Ok((template, exchange))),
            Err(error) => Ok(Err(transport_failure(&error))),
        }
    }
}

/// Writes the template that `prepare` writes, and the request that
/// `encode` writes from it into `buf`, signed with `credentials` at
/// `now`.
fn signed<'b, 'c: 'b>(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    now: &Timestamps,
    prepare: &impl Fn(&Bucket<'_>, &Credentials<'_>, &mut [u8]) -> Result<Template, CrateError>,
    encode: &impl Fn(&Template, &[u8], &mut [u8]) -> Result<Unsigned<'c>, CrateError>,
    buf: &'b mut Vec<u8>,
) -> Result<(Template, WireRequest<'b>), CrateError> {
    let write = |bytes: &mut [u8]| prepare(bucket, credentials, bytes);
    let mut bytes = vec![0; required(write)?];
    let template = write(&mut bytes)?;
    let key = sigv4::signing_key::<Sha256RustCrypto>(&template, &bytes, credentials, now)?;
    let stamp = sigv4::stamp(&key, &template, &bytes, now)?;
    *buf = vec![0; required(|out| encode(&template, &bytes, out))?];
    let unsigned = encode(&template, &bytes, buf)?;
    // The crate writes a body of its own with a placeholder for its
    // checksum, which the caller computes.
    if let Some(kind) = unsigned.body_checksum() {
        let digest = crate::digest_of(kind, unsigned.body(buf).unwrap_or_default());
        s3::set_body_checksum(&unsigned, buf, &digest)?;
    }
    Ok((template, sigv4::sign(&stamp, &unsigned, buf)))
}

/// Sends a CreateSession, signed with the case's own credentials, and reads
/// the credentials of the session, or returns the result to report instead.
///
/// The session borrows the response body, which is leaked: the process
/// grades one case and exits.
fn open_session(
    context: &AdapterContext,
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
) -> Result<Result<Session<'static>, Value>, AdapterError> {
    let now = current_timestamps();
    let mut buf = Vec::new();
    let (_, request) = page_step!(signed(
        bucket,
        credentials,
        &now,
        &|bucket, credentials, buf| s3::prepare_create_session(bucket, credentials, buf),
        &|template, bytes, buf| s3::encode_create_session(template, bytes, &now, buf),
        &mut buf
    ));
    let exchange = match send_request(context, &request) {
        Ok(exchange) => exchange,
        Err(error) => return Ok(Err(transport_failure(&error))),
    };
    let head_outcome = page_step!(s3::accept_create_session_head(exchange.response_head()));
    let outcome = match head_outcome {
        SessionHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_create_session_error_body(failure, &exchange.body)
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
    Ok(Ok(page_step!(s3::read_session(body))))
}

/// Unwraps what `Client::send` returns, or returns the result it reports
/// instead.
macro_rules! sent {
    ($expression:expr) => {
        match $expression? {
            Ok(sent) => sent,
            Err(result) => return Ok(result),
        }
    };
}

fn read_object(client: &Client<'_>, call: &Value, kind: GetKind) -> Result<Value, AdapterError> {
    let Some((options, item)) = requested_get(call, kind)? else {
        return Ok(unsupported_by_crate("a read carries one precondition"));
    };
    let now = current_timestamps();
    let (template, exchange) = match client.send(
        |bucket, credentials, buf| s3::prepare_get(bucket, credentials, &options, buf),
        |template, bytes, buf| s3::encode_get(template, bytes, &item, &now, buf),
        None,
        BodyRead::of(call),
    )? {
        Ok(sent) => sent,
        Err(result) => return Ok(result),
    };
    let shape = item.shape(&options);
    let range = item.range;
    let head_outcome = crate_step!(s3::accept_get_head(
        template.service(),
        shape,
        exchange.response_head()
    ));
    let outcome = match head_outcome {
        GetHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_get_error_body(shape, failure, &exchange.body)
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
                read_body_fields(&mut value, &exchange);
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
        return Ok(unsupported_by_crate("a write carries one precondition"));
    };
    let body = CallBody::of(call)?;
    // S3 takes every checksum that the crate has.
    let (checksum_kind, checksum) = match requested_checksum_kind(call) {
        None => (None, None),
        Some(Some((kind, text))) => match checksum_value(kind, text, &body) {
            Ok(value) => (Some(kind), Some(value)),
            Err(result) => return Ok(result),
        },
        Some(None) => return Ok(unsupported_by_adapter("checksum algorithm not mapped")),
    };
    let metadata_pairs = requested_metadata(call);
    let tags = requested_tags(call);
    let options = PutOptions {
        condition,
        metadata: &metadata_pairs,
        options: WriteOptions {
            properties: requested_properties(call),
            tags: &tags,
            storage_class: optional_text(call, "storage_class"),
        },
        checksum: checksum_kind,
    };
    let item = PutItem {
        key: optional_text(call, "key").unwrap_or_default(),
        content: body.payload(),
        condition_value,
        checksum,
    };
    // A held body is signed, and a generated one goes unsigned, as the
    // adapter writes it only as it sends it.
    let (signing, digest) = match &body {
        CallBody::Held(bytes) => (
            PayloadSigning::Signed,
            Some(sigv4::sha256::<Sha256RustCrypto>(bytes)),
        ),
        CallBody::Generated { .. } => (PayloadSigning::Unsigned, None),
    };

    let now = current_timestamps();
    let (_, exchange) = match client.send(
        |bucket, credentials, buf| s3::prepare_put(bucket, credentials, &options, signing, buf),
        |template, bytes, buf| s3::encode_put(template, bytes, &item, digest.as_ref(), &now, buf),
        Some(&body),
        BodyRead::Hold,
    )? {
        Ok(sent) => sent,
        Err(result) => return Ok(result),
    };

    let shape = item.shape(&options);
    let head_outcome = crate_step!(s3::accept_put_head(shape, exchange.response_head()));
    let outcome = match head_outcome {
        PutHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_put_error_body(shape, failure, &exchange.body)
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
    let Some((options, item)) = requested_delete(call)? else {
        return Ok(unsupported_by_crate("a removal carries one precondition"));
    };
    let now = current_timestamps();
    let (template, exchange) = match client.send(
        |bucket, credentials, buf| s3::prepare_delete(bucket, credentials, &options, buf),
        |template, bytes, buf| s3::encode_delete(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    )? {
        Ok(sent) => sent,
        Err(result) => return Ok(result),
    };
    let shape = options.shape();
    let head_outcome = crate_step!(s3::accept_delete_head(
        template.service(),
        shape,
        exchange.response_head()
    ));
    let outcome = match head_outcome {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_delete_error_body(shape, failure, &exchange.body)
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
    let item = ObjectItem::new(optional_text(call, "key").unwrap_or_default());
    let tags = requested_tags(call);
    let options = s3::CreateUploadOptions {
        options: WriteOptions {
            properties: requested_properties(call),
            tags: &tags,
            storage_class: optional_text(call, "storage_class"),
        },
        ..s3::CreateUploadOptions::default()
    };
    let now = current_timestamps();
    let (_, mut exchange) = sent!(client.send(
        |bucket, credentials, buf| s3::prepare_create_upload(bucket, credentials, &options, buf),
        |template, bytes, buf| s3::encode_create_upload(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));

    let head_outcome = crate_step!(s3::accept_create_upload_head(exchange.response_head()));
    let outcome = match head_outcome {
        CreateUploadHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_create_upload_error_body(failure, &exchange.body)
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

    let upload_id = crate_step!(s3::read_upload_id(&mut exchange.body));
    Ok(successful_result(json!({"upload_id": upload_id})))
}

fn stage_part(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    // A number that no `u32` holds is above every limit, as `u32::MAX` is.
    let number = call
        .get("part_number")
        .and_then(Value::as_u64)
        .ok_or("missing part_number")?;
    let body = decode_base64_field(call, "body_base64")?;
    let item = s3::StagePartItem::new(
        optional_text(call, "key").unwrap_or_default(),
        optional_text(call, "upload_id").unwrap_or_default(),
        u32::try_from(number).unwrap_or(u32::MAX),
        Payload::Slice(&body),
    );
    let digest = sigv4::sha256::<Sha256RustCrypto>(&body);
    let now = current_timestamps();
    let (_, exchange) = sent!(client.send(
        |bucket, credentials, buf| {
            s3::prepare_stage_part(
                bucket,
                credentials,
                &StageOptions::default(),
                PayloadSigning::Signed,
                buf,
            )
        },
        |template, bytes, buf| s3::encode_stage_part(
            template,
            bytes,
            &item,
            Some(&digest),
            &now,
            buf
        ),
        None,
        BodyRead::Hold,
    ));

    let head_outcome = crate_step!(s3::accept_stage_part_head(exchange.response_head()));
    let outcome = match head_outcome {
        StageHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_stage_part_error_body(failure, &exchange.body)
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
    let (metadata, tags) = (requested_metadata(call), requested_tags(call));
    let Some((options, item)) =
        requested_copy(call, "source_bucket", "storage_class", &metadata, &tags)?
    else {
        return Ok(unsupported_by_crate(
            "a copy carries one condition on the target and one on the source",
        ));
    };
    let now = current_timestamps();
    let (_, mut exchange) = sent!(client.send_own(
        |bucket, credentials, buf| s3::prepare_copy(bucket, credentials, &options, buf),
        |template, bytes, buf| s3::encode_copy(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));

    let shape = options.shape();
    let (head, body) = exchange.head_and_body();
    let outcome = match crate_step!(s3::accept_copy_head(shape, head)) {
        CopyHeadOutcome::NeedResultBody { .. } => {
            crate_step!(s3::accept_copy_body(shape, head, body))
        }
        CopyHeadOutcome::NeedErrorBody(failure) => s3::accept_copy_error_body(shape, failure, body),
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
    let Some((options, copy)) = requested_copy(call, "source_bucket", "storage_class", &[], &[])?
    else {
        return Ok(unsupported_by_crate(
            "a copy carries one condition on the source",
        ));
    };
    let number = call
        .get("part_number")
        .and_then(Value::as_u64)
        .ok_or("part copy without part_number")?;
    let item = s3::StagePartCopyItem {
        copy,
        upload_id: optional_text(call, "upload_id").unwrap_or_default(),
        number: u32::try_from(number).unwrap_or(u32::MAX),
    };
    let now = current_timestamps();
    let (_, mut exchange) = sent!(client.send_own(
        |bucket, credentials, buf| s3::prepare_stage_part_copy(bucket, credentials, &options, buf),
        |template, bytes, buf| s3::encode_stage_part_copy(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));

    let (head, body) = exchange.head_and_body();
    let outcome = match crate_step!(s3::accept_stage_part_copy_head(head)) {
        StageHeadOutcome::NeedResultBody { .. } => {
            crate_step!(s3::accept_stage_part_copy_body(head, body))
        }
        StageHeadOutcome::NeedErrorBody(failure) => s3::accept_stage_part_error_body(failure, body),
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
        return Ok(unsupported_by_crate("a commit carries one precondition"));
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

    let size = call.get("object_size").and_then(Value::as_u64);
    let options = s3::CommitOptions {
        condition,
        sized: size.is_some(),
        ..s3::CommitOptions::default()
    };
    let item = s3::CommitItem {
        condition_value,
        size,
        ..s3::CommitItem::new(
            optional_text(call, "key").unwrap_or_default(),
            optional_text(call, "upload_id").unwrap_or_default(),
            &part_references,
        )
    };
    let now = current_timestamps();
    let (_, mut exchange) = sent!(client.send(
        |bucket, credentials, buf| s3::prepare_commit_parts(bucket, credentials, &options, buf),
        |template, bytes, buf| s3::encode_commit_parts(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));

    let shape = item.shape(&options);
    let (head, body) = exchange.head_and_body();
    let outcome = match crate_step!(s3::accept_commit_parts_head(shape, head)) {
        CommitHeadOutcome::NeedResultBody { .. } => {
            crate_step!(s3::accept_commit_parts_body(shape, head, body))
        }
        CommitHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_commit_parts_error_body(shape, failure, body)
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
    let item = s3::UploadItem {
        key: optional_text(call, "key").unwrap_or_default(),
        upload_id: optional_text(call, "upload_id").unwrap_or_default(),
    };
    let now = current_timestamps();
    let (template, exchange) = sent!(client.send(
        s3::prepare_abort_upload,
        |template, bytes, buf| s3::encode_abort_upload(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));

    let head_outcome = crate_step!(s3::accept_abort_upload_head(
        template.service(),
        exchange.response_head()
    ));
    let outcome = match head_outcome {
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_abort_upload_error_body(failure, &exchange.body)
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
        let item = s3::ListPartsItem {
            key,
            upload_id,
            marker: marker.as_deref(),
        };
        let now = current_timestamps();
        let (_, mut exchange) = sent!(client.send(
            |bucket, credentials, buf| s3::prepare_list_parts(bucket, credentials, None, buf),
            |template, bytes, buf| s3::encode_list_parts(template, bytes, &item, &now, buf),
            None,
            BodyRead::Hold,
        ));

        let head_outcome = crate_step!(s3::accept_list_parts_head(exchange.response_head()));
        let outcome = match head_outcome {
            ListPartsHeadOutcome::NeedErrorBody(failure) => {
                s3::accept_list_parts_error_body(failure, &exchange.body)
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
        let page = crate_step!(s3::fill_parts(&mut exchange.body, &mut parts));
        listed.extend(
            parts[..page.filled]
                .iter()
                .map(|part| json!({"number": part.number, "size": part.size, "etag": part.e_tag})),
        );
        // A page that names itself as the next one would never end.
        match page.next_marker.and_then(ListMarker::text) {
            Some(next) if marker.as_deref() != Some(next) => marker = Some(next.to_owned()),
            Some(_) => return Err("ListParts named the same page twice".into()),
            None => break,
        }
    }
    Ok(successful_result(json!({"parts": listed})))
}

/// The pages of the bucket's listings.
impl PageSource for Client<'_> {
    fn read_page(&self, options: &ListOptions<'_>, item: &ListItem<'_>) -> PageRead {
        let now = current_timestamps();
        let (_, mut exchange) = match self.send(
            |bucket, credentials, buf| s3::prepare_list(bucket, credentials, options, buf),
            |template, bytes, buf| s3::encode_list(template, bytes, item, &now, buf),
            None,
            BodyRead::Hold,
        )? {
            Ok(sent) => sent,
            Err(result) => return Ok(Err(result)),
        };

        let head_outcome = page_step!(s3::accept_list_head(exchange.response_head()));
        let outcome = match head_outcome {
            ListHeadOutcome::NeedErrorBody(failure) => {
                s3::accept_list_error_body(failure, &exchange.body)
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

        let mut slots: Vec<ObjectWithProperties<'_>> = entry_slots(options, MAX_PAGE_ENTRIES);
        // These properties are read in the same pass as the page.
        let wanted = PropertySet::of(&[
            ObjectProperty::StorageClass,
            ObjectProperty::ChecksumAlgorithm,
            ObjectProperty::Owner,
        ]);
        let listing = page_step!(s3::fill_listing_with(
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
    if let Some(millis) = entry.last_modified.and_then(iso8601_ms) {
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

/// Decodes the `%XX` escapes of a URL's path or query value, or returns
/// `None` for an escape that is not one or text that is not UTF-8.
fn percent_decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// The decoded parameters of a URL's query, in the order sent, or `None`
/// if one is named twice or does not decode.
fn parsed_query(query: Option<&str>) -> Option<Vec<(String, String)>> {
    let mut parameters: Vec<(String, String)> = Vec::new();
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = percent_decoded(name)?;
        if parameters.iter().any(|(found, _)| *found == name) {
            return None;
        }
        parameters.push((name, percent_decoded(value)?));
    }
    Some(parameters)
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

/// Signs a GET, HEAD, PUT or DELETE from a template, and returns the
/// request's headers.
///
/// The URL names a virtual-hosted bucket. The case adds at most one header,
/// a range, which the request's item carries. The crate derives every other signed header itself, so a
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
    let (host, target) = rest.split_once('/').unwrap_or((rest, ""));
    let Some((bucket_name, service_host)) = host.split_once('.') else {
        return Ok(unsupported_by_adapter("url names no virtual-hosted bucket"));
    };
    let (raw_path, raw_query) = target
        .split_once('?')
        .map_or((target, None), |(path, query)| (path, Some(query)));
    let Some(key) = percent_decoded(raw_path) else {
        return Ok(unsupported_by_adapter("path not mapped"));
    };
    // An HTTP client resolves these segments out of the URL, so the crate
    // refuses a key that holds one rather than sign a path never sent.
    if key
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Ok(unsupported_by_crate(
            "the crate refuses a key with a . or .. segment, which an HTTP client resolves",
        ));
    }
    let Some(query) = parsed_query(raw_query) else {
        return Ok(unsupported_by_crate(
            "the crate sends no query parameter twice",
        ));
    };
    let param = |name: &str| {
        query
            .iter()
            .find(|(found, _)| found == name)
            .map(|(_, value)| value.as_str())
    };
    let only = |allowed: &[&str]| {
        query
            .iter()
            .all(|(name, _)| allowed.contains(&name.as_str()))
    };
    let signing = match optional_text(call, "payload_hash") {
        None => PayloadSigning::Signed,
        Some("UNSIGNED-PAYLOAD") => PayloadSigning::Unsigned,
        Some(_) => return Ok(unsupported_by_crate("the crate signs no streaming payload")),
    };
    let key = key.as_str();
    let version = param("versionId").map(Revision::Version);
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

    let bucket = crate_step!(Bucket::new_allowing_http(
        &endpoint,
        bucket_name,
        region,
        service
    ))
    .with_addressing(Addressing::VirtualHosted);
    let mut credentials = crate_step!(Credentials::new(key_id, secret, wipe));
    if let Some(token) = session_token
        && service == Service::Aws
    {
        credentials = crate_step!(credentials.with_session_token(token));
    }
    let body = STANDARD.decode(optional_text(call, "body_base64").unwrap_or_default())?;

    let method = optional_text(call, "method").unwrap_or("GET");
    let mut get_buf = Vec::new();
    // The credentials of the case are those of a session that CreateSession
    // returned.
    let signing_credentials = match session_token {
        Some(token) if service == Service::AwsDirectory => crate_step!(
            Session {
                key_id,
                secret,
                token,
                expires_at: None,
            }
            .credentials(wipe)
        ),
        _ => credentials,
    };
    let request: (String, Vec<(String, String)>) = match method {
        // A GetObjectTagging, of the object or of a version of it.
        "GET" if param("tagging").is_some() && only(&["tagging", "versionId"]) => {
            let revision = match version {
                Some(_) => RevisionKind::Version,
                None => RevisionKind::Current,
            };
            let item = ObjectItem {
                revision: param("versionId"),
                ..ObjectItem::new(key)
            };
            let (_, request) = crate_step!(signed(
                &bucket,
                &signing_credentials,
                &now,
                &|bucket, credentials, buf| s3::prepare_get_tagging(
                    bucket,
                    credentials,
                    revision,
                    buf
                ),
                &|template, bytes, buf| s3::encode_get_tagging(template, bytes, &item, &now, buf),
                &mut get_buf
            ));
            owned(&request)
        }
        // A ListObjectsV2, which asks for URL-encoded keys every time.
        "GET" if key.is_empty() && param("list-type").is_some() => {
            let listed = [
                "list-type",
                "encoding-type",
                "prefix",
                "delimiter",
                "max-keys",
                "continuation-token",
                "start-after",
                "fetch-owner",
            ];
            if !only(&listed) || param("list-type") != Some("2") {
                return Ok(unsupported_by_adapter("listing query not mapped"));
            }
            if param("encoding-type") != Some("url") {
                return Ok(unsupported_by_crate(
                    "the crate asks for encoding-type=url on every listing",
                ));
            }
            let Ok(max_results) = param("max-keys").map(str::parse::<u32>).transpose() else {
                return Ok(unsupported_by_adapter("max-keys not mapped"));
            };
            let include = match param("fetch-owner") {
                None => ListInclude::default(),
                Some("true") => ListInclude::OWNER,
                Some(_) => return Ok(unsupported_by_adapter("fetch-owner not mapped")),
            };
            let options = ListOptions {
                delimiter: param("delimiter"),
                max_results,
                include,
            };
            let item = ListItem {
                marker: param("continuation-token").map(ListMarker::Text),
                start_after: param("start-after"),
                ..ListItem::new(param("prefix").unwrap_or_default())
            };
            let (_, request) = crate_step!(signed(
                &bucket,
                &signing_credentials,
                &now,
                &|bucket, credentials, buf| s3::prepare_list(bucket, credentials, &options, buf),
                &|template, bytes, buf| s3::encode_list(template, bytes, &item, &now, buf),
                &mut get_buf
            ));
            owned(&request)
        }
        _ if !only(&["versionId"]) => {
            return Ok(unsupported_by_adapter("query not mapped"));
        }
        "GET" | "HEAD" => {
            let options = GetOptions {
                kind: if method == "GET" {
                    GetKind::Bytes
                } else {
                    GetKind::Head
                },
                ranging: if range == RequestedRange::Whole {
                    Ranging::Whole
                } else {
                    Ranging::Ranged
                },
                revision: if version.is_some() {
                    RevisionKind::Version
                } else {
                    RevisionKind::Current
                },
                ..GetOptions::new()
            };
            let item = GetItem {
                key,
                range,
                condition_value: None,
                revision: param("versionId"),
            };
            let (_, request) = crate_step!(signed(
                &bucket,
                &signing_credentials,
                &now,
                &|bucket, credentials, buf| s3::prepare_get(bucket, credentials, &options, buf),
                &|template, bytes, buf| s3::encode_get(template, bytes, &item, &now, buf),
                &mut get_buf
            ));
            owned(&request)
        }
        "PUT" if range == RequestedRange::Whole && version.is_none() => {
            let options = PutOptions::default();
            let item = PutItem::new(key, Payload::Slice(&body));
            let digest = match signing {
                PayloadSigning::Unsigned => None,
                PayloadSigning::Signed => Some(sigv4::sha256::<Sha256RustCrypto>(&body)),
            };
            let (_, request) = crate_step!(signed(
                &bucket,
                &signing_credentials,
                &now,
                &|bucket, credentials, buf| {
                    s3::prepare_put(bucket, credentials, &options, signing, buf)
                },
                &|template, bytes, buf| {
                    s3::encode_put(template, bytes, &item, digest.as_ref(), &now, buf)
                },
                &mut get_buf
            ));
            owned(&request)
        }
        "DELETE" if range == RequestedRange::Whole => {
            let options = DeleteOptions {
                revision: if version.is_some() {
                    RevisionKind::Version
                } else {
                    RevisionKind::Current
                },
                ..DeleteOptions::new()
            };
            let item = ObjectItem {
                revision: param("versionId"),
                ..ObjectItem::new(key)
            };
            let (_, request) = crate_step!(signed(
                &bucket,
                &signing_credentials,
                &now,
                &|bucket, credentials, buf| s3::prepare_delete(bucket, credentials, &options, buf),
                &|template, bytes, buf| s3::encode_delete(template, bytes, &item, &now, buf),
                &mut get_buf
            ));
            owned(&request)
        }
        _ => return Ok(unsupported_by_adapter("method not mapped")),
    };
    let (url, fields) = request;

    // The crate encodes the key itself. A URL whose path it would write
    // otherwise names a request that the crate does not send.
    let written_path = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map_or("", |(_, target)| {
            target.split('?').next().unwrap_or_default()
        });
    if written_path != raw_path {
        return Ok(unsupported_by_crate(
            "the crate encodes this path otherwise",
        ));
    }
    let headers: Map<String, Value> = fields
        .iter()
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
            "body_sink",
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
    let bucket = crate_step!(Bucket::new_allowing_http(
        endpoint_url,
        bucket_name,
        region,
        service
    ));
    let mut credentials = crate_step!(Credentials::new(&key_id, &secret, wipe));
    if let Some(token) = &session_token {
        credentials = crate_step!(credentials.with_session_token(token));
    }

    let context = AdapterContext::for_endpoint(endpoint)?;
    if operation == "s3.create_session" {
        return Ok(match open_session(&context, &bucket, &credentials)? {
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
        bucket,
        credentials,
        directory: service == Service::AwsDirectory,
        session_credentials: OnceCell::new(),
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
    let Some((options, item)) = requested_restore_options(call, None) else {
        return Ok(unsupported_by_adapter("priority not mapped"));
    };
    let now = current_timestamps();
    let (_, exchange) = sent!(client.send(
        |bucket, credentials, buf| s3::prepare_restore(bucket, credentials, &options, buf),
        |template, bytes, buf| s3::encode_restore(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = match crate_step!(s3::accept_restore_head(exchange.response_head())) {
        RestoreHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_restore_error_body(failure, &exchange.body)
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
    let Some((_, revision, item)) = requested_object(call) else {
        return Ok(unsupported_by_crate(
            "a PutObjectTagging carries no condition",
        ));
    };
    let tags = requested_tags(call);
    let now = current_timestamps();
    let (_, exchange) = sent!(client.send(
        |bucket, credentials, buf| {
            s3::prepare_put_tagging(
                bucket,
                credentials,
                &tags,
                revision,
                Some(ChecksumKind::Crc64),
                buf,
            )
        },
        |template, bytes, buf| s3::encode_put_tagging(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = match crate_step!(s3::accept_put_tagging_head(exchange.response_head())) {
        UpdateHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_update_error_body(failure, &exchange.body)
        }
        outcome => outcome,
    };
    Ok(match outcome {
        UpdateHeadOutcome::Updated { .. } => successful_result(json!({})),
        UpdateHeadOutcome::NotFound { .. } => error_result(&exchange, 404),
        UpdateHeadOutcome::NeedErrorBody(failure) | UpdateHeadOutcome::ServiceFailure(failure) => {
            error_result(&exchange, failure.status)
        }
        _ => error_result(&exchange, exchange.status),
    })
}

fn get_tagging(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let Some((_, revision, item)) = requested_object(call) else {
        return Ok(unsupported_by_crate(
            "a GetObjectTagging carries no condition",
        ));
    };
    let now = current_timestamps();
    let (_, mut exchange) = sent!(client.send(
        |bucket, credentials, buf| s3::prepare_get_tagging(bucket, credentials, revision, buf),
        |template, bytes, buf| s3::encode_get_tagging(template, bytes, &item, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = match crate_step!(s3::accept_get_tagging_head(exchange.response_head())) {
        TagsHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_get_tagging_error_body(failure, &exchange.body)
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
    let count = crate_step!(s3::fill_tags(&mut exchange.body, &mut tags));
    Ok(successful_result(
        json!({"tags": tags_value(&tags[..count])}),
    ))
}

fn delete_many(client: &Client<'_>, call: &Value) -> Result<Value, AdapterError> {
    let keys = requested_keys(call);
    let now = current_timestamps();
    let (_, mut exchange) = sent!(client.send(
        |bucket, credentials, buf| {
            s3::prepare_delete_many(bucket, credentials, Some(ChecksumKind::Crc64), buf)
        },
        |template, bytes, buf| s3::encode_delete_many(template, bytes, &keys, &now, buf),
        None,
        BodyRead::Hold,
    ));
    let outcome = match crate_step!(s3::accept_delete_many_head(exchange.response_head())) {
        DeleteManyHeadOutcome::NeedErrorBody(failure) => {
            s3::accept_delete_many_error_body(failure, &exchange.body)
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
    let count = crate_step!(s3::fill_delete_results(&mut exchange.body, &mut results));
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
