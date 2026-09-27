//! S3 requests, sent with an [`Objects`] client and signed with AWS
//! Signature Version 4.
//!
//! Create the client with the SHA-256 provider and the `wipe` function of
//! `borink-object-storage-crypto`, as the `s3_get` example does.

use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage_proto::s3::{Objects, PayloadHash};
use borink_object_storage_proto::{
    DeleteHeadOutcome, GetHeadOutcome, HeaderSpan, ListEntry, ListHeadOutcome, Listing, Payload,
    PhysicalDelete, PhysicalGet, PhysicalList, PhysicalPut, PutHeadOutcome, ResponseHead,
    Timestamps, layered,
};

use crate::{MAX_ERROR_BODY, MAX_PAGE};

/// Builds and executes one GET request, returning an owned response body.
pub fn get(objects: &Objects<'_>, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let get = PhysicalGet::new(key);
    let size = layered::s3::get_requirements(objects, &get, &now)?;
    let mut buf = vec![0; size.bytes];
    let mut headers = vec![HeaderSpan::default(); size.headers];
    let request = objects.encode_get(&mut buf, &mut headers, &get, &now)?;

    let mut outgoing = ureq::get(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    // As for Azure, this host returns the stored bytes and never decompresses
    // them. See the `ureq` dependency in Cargo.toml.
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let status = incoming.status().as_u16();
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match objects.accept_get_head(get.shape(), head)? {
        GetHeadOutcome::Body { .. } => incoming.body_mut().read_to_vec().map_err(Into::into),
        GetHeadOutcome::Complete { .. } => Ok(Vec::new()),
        // S3 names every error in the body. An error body that does not arrive
        // costs the name of the error, not the outcome.
        GetHeadOutcome::NeedErrorBody(failure) => {
            let body = incoming
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BODY)
                .read_to_vec()
                .unwrap_or_default();
            Err(no_object(objects.accept_error_body(
                failure.status,
                failure.request_id,
                &body,
            )))
        }
        outcome => Err(no_object(outcome)),
    }
}

fn no_object(outcome: GetHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 returned no object: {outcome}").into()
}

/// Builds and executes one PUT request, storing `content` as the whole object.
///
/// The request signs the SHA-256 of `content`, which this function computes
/// with the client's provider.
pub fn put(
    objects: &Objects<'_>,
    key: &str,
    content: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let put = PhysicalPut::new(key);
    let content = Payload::Slice(content);
    let hash = PayloadHash::Compute;
    let size = layered::s3::put_requirements(objects, &put, content, hash, &now)?;
    let mut buf = vec![0; size.bytes];
    let mut headers = vec![HeaderSpan::default(); size.headers];
    let request = objects.encode_put(&mut buf, &mut headers, &put, content, hash, &now)?;

    let mut outgoing = ureq::put(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        // This host writes from memory, so it always has the bytes to send.
        .send(request.payload().bytes().unwrap_or_default())?;
    let status = incoming.status().as_u16();
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match objects.accept_put_head(put.shape(), head)? {
        PutHeadOutcome::Created { .. } => Ok(()),
        PutHeadOutcome::NeedErrorBody(failure) => {
            let body = incoming
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BODY)
                .read_to_vec()
                .unwrap_or_default();
            Err(not_stored(objects.accept_put_error_body(
                failure.status,
                failure.request_id,
                &body,
            )))
        }
        outcome => Err(not_stored(outcome)),
    }
}

fn not_stored(outcome: PutHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 stored no object: {outcome}").into()
}

/// Builds and executes one DELETE request, removing the whole object.
///
/// S3 answers the removal of a missing object as it answers any other
/// removal, so this function cannot report one.
pub fn delete(objects: &Objects<'_>, key: &str) -> Result<(), Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let delete = PhysicalDelete::new(key);
    let size = layered::s3::delete_requirements(objects, &delete, &now)?;
    let mut buf = vec![0; size.bytes];
    let mut headers = vec![HeaderSpan::default(); size.headers];
    let request = objects.encode_delete(&mut buf, &mut headers, &delete, &now)?;

    let mut outgoing = ureq::delete(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let status = incoming.status().as_u16();
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match objects.accept_delete_head(delete.shape(), head)? {
        DeleteHeadOutcome::Accepted => Ok(()),
        DeleteHeadOutcome::NeedErrorBody(failure) => {
            let body = incoming
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BODY)
                .read_to_vec()
                .unwrap_or_default();
            Err(not_removed(objects.accept_delete_error_body(
                failure.status,
                failure.request_id,
                &body,
            )))
        }
        outcome => Err(not_removed(outcome)),
    }
}

fn not_removed(outcome: DeleteHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 removed no object: {outcome}").into()
}

/// Builds and executes one listing request, and reads the page it answered.
///
/// This function reads the page into `body`, and the entries it writes into
/// `into` borrow those bytes. An array of `max_results` entries always holds a
/// whole page, and so does one of 1,000 entries for AWS. A smaller one is
/// refused with the number of entries the page holds.
///
/// # Errors
///
/// Returns an error if the request could not be sent, or if S3 listed
/// nothing.
pub fn list<'b>(
    objects: &Objects<'_>,
    plan: &PhysicalList<'_>,
    body: &'b mut Vec<u8>,
    into: &mut [ListEntry<'b>],
) -> Result<Listing<'b>, Box<dyn std::error::Error>> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let size = layered::s3::list_requirements(objects, plan, &now)?;
    let mut buf = vec![0; size.bytes];
    let mut headers = vec![HeaderSpan::default(); size.headers];
    let request = objects.encode_list(&mut buf, &mut headers, plan, &now)?;

    let mut outgoing = ureq::get(request.url());
    for (name, value) in request.headers() {
        outgoing = outgoing.header(name, value);
    }
    let mut incoming = outgoing
        .config()
        .http_status_as_error(false)
        .build()
        .call()?;
    let status = incoming.status().as_u16();
    let headers = incoming.headers().clone();
    let head = ResponseHead::from_headers(
        status,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match objects.accept_list_head(head)? {
        ListHeadOutcome::Page { .. } => {
            // S3 often sends a page without its length, so the read is
            // capped whatever the head says.
            *body = incoming
                .body_mut()
                .with_config()
                .limit(MAX_PAGE)
                .read_to_vec()?;
            objects.fill_listing(body, into).map_err(Into::into)
        }
        ListHeadOutcome::NeedErrorBody(failure) => {
            let error = incoming
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BODY)
                .read_to_vec()
                .unwrap_or_default();
            Err(not_listed(objects.accept_list_error_body(
                failure.status,
                failure.request_id,
                &error,
            )))
        }
        outcome => Err(not_listed(outcome)),
    }
}

fn not_listed(outcome: ListHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 listed no keys: {outcome}").into()
}
