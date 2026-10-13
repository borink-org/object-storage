//! S3 requests, written from templates and signed with AWS Signature
//! Version 4.
//!
//! Create the credentials with the `wipe` function of
//! `borink-object-storage-crypto`, as the `s3_get` example does.

use borink_object_storage_crypto::{Sha256RustCrypto, sigv4};
use borink_object_storage_proto::s3::{self, Bucket, PayloadSigning, Template};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{
    DeleteHeadOutcome, DeleteOptions, Error, GetHeadOutcome, GetItem, GetOptions, ListEntry,
    ListHeadOutcome, ListItem, Listing, ObjectItem, Payload, PutHeadOutcome, PutItem, PutOptions,
    Timestamps,
};

// The day's signing key and the start of the string to sign at `now`,
// for the requests of `template`. A host that sends many requests derives
// the key once a day.
fn stamp(
    template: &Template,
    bytes: &[u8],
    credentials: &Credentials<'_>,
    now: &Timestamps,
) -> Result<sigv4::Stamp<Sha256RustCrypto>, Error> {
    let signing_key = sigv4::signing_key::<Sha256RustCrypto>(template, bytes, credentials, now)?;
    sigv4::stamp(&signing_key, template, bytes, now)
}

/// Builds and executes one GET request, returning an owned response body.
///
/// The template and the signing key are made for this one read. A host
/// that reads many objects makes the template once and the key once a day.
pub fn get(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    key: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let now = crate::now()?;
    let options = GetOptions::new();
    let prepare = |buf: &mut [u8]| s3::prepare_get(bucket, credentials, &options, buf);
    let (template, bytes) = crate::template(prepare)?;
    let stamp = stamp(&template, &bytes, credentials, &now)?;
    let item = GetItem::new(key);
    let encode = |buf: &mut [u8]| s3::encode_get(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; crate::required(encode)?];
    let unsigned = s3::encode_get(&template, &bytes, &item, &now, &mut buf)?;
    let request = sigv4::sign(&stamp, &unsigned, &mut buf);

    let (parts, mut body) = crate::send(&request, None)?;
    let service = template.service();
    let shape = item.shape(&options);
    match s3::accept_get_head(service, shape, crate::head(&parts))? {
        GetHeadOutcome::Body { .. } => body.read_to_vec().map_err(Into::into),
        GetHeadOutcome::Complete { .. } => Ok(Vec::new()),
        // S3 names every error in the body.
        GetHeadOutcome::NeedErrorBody(failure) => Err(no_object(s3::accept_get_error_body(
            shape,
            failure,
            &crate::error_body(&mut body),
        ))),
        outcome => Err(no_object(outcome)),
    }
}

fn no_object(outcome: GetHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 returned no object: {outcome}").into()
}

/// Builds and executes one PUT request, storing `content` as the whole object.
///
/// The request signs the SHA-256 of `content`, which this function
/// computes.
pub fn put(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    key: &str,
    content: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let now = crate::now()?;
    let options = PutOptions::default();
    let signing = PayloadSigning::Signed;
    let prepare = |buf: &mut [u8]| s3::prepare_put(bucket, credentials, &options, signing, buf);
    let (template, bytes) = crate::template(prepare)?;
    let stamp = stamp(&template, &bytes, credentials, &now)?;
    let item = PutItem::new(key, Payload::Slice(content));
    let digest = sigv4::sha256::<Sha256RustCrypto>(content);
    let encode = |buf: &mut [u8]| {
        s3::encode_put(&template, &bytes, &item, Some(&digest), &now, buf).map(drop)
    };
    let mut buf = vec![0; crate::required(encode)?];
    let unsigned = s3::encode_put(&template, &bytes, &item, Some(&digest), &now, &mut buf)?;
    let request = sigv4::sign(&stamp, &unsigned, &mut buf);

    let (parts, mut body) = crate::send(&request, None)?;
    let shape = item.shape(&options);
    match s3::accept_put_head(shape, crate::head(&parts))? {
        PutHeadOutcome::Created { .. } => Ok(()),
        PutHeadOutcome::NeedErrorBody(failure) => Err(not_stored(s3::accept_put_error_body(
            shape,
            failure,
            &crate::error_body(&mut body),
        ))),
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
pub fn delete(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = crate::now()?;
    let options = DeleteOptions::new();
    let prepare = |buf: &mut [u8]| s3::prepare_delete(bucket, credentials, &options, buf);
    let (template, bytes) = crate::template(prepare)?;
    let stamp = stamp(&template, &bytes, credentials, &now)?;
    let item = ObjectItem::new(key);
    let encode = |buf: &mut [u8]| s3::encode_delete(&template, &bytes, &item, &now, buf).map(drop);
    let mut buf = vec![0; crate::required(encode)?];
    let unsigned = s3::encode_delete(&template, &bytes, &item, &now, &mut buf)?;
    let request = sigv4::sign(&stamp, &unsigned, &mut buf);

    let (parts, mut body) = crate::send(&request, None)?;
    let head = crate::head(&parts);
    match s3::accept_delete_head(template.service(), options.shape(), head)? {
        DeleteHeadOutcome::Accepted => Ok(()),
        DeleteHeadOutcome::NeedErrorBody(failure) => Err(not_removed(
            s3::accept_delete_error_body(options.shape(), failure, &crate::error_body(&mut body)),
        )),
        outcome => Err(not_removed(outcome)),
    }
}

fn not_removed(outcome: DeleteHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 removed no object: {outcome}").into()
}

/// Builds and executes one listing request from `template`, signed with
/// `credentials`, and reads the page it answered.
///
/// Write the template once with `s3::prepare_list` and pass it, with its
/// bytes, for every page. This function reads the page into `body`, and
/// the entries it writes into `into` borrow those bytes. An array of
/// `max_results` entries always holds a whole page, and so does one of
/// 1,000 entries for AWS. A smaller one is refused with the number of
/// entries the page holds.
///
/// # Errors
///
/// Returns an error if the request could not be sent, or if S3 listed
/// nothing.
pub fn list<'b>(
    template: &Template,
    bytes: &[u8],
    credentials: &Credentials<'_>,
    item: &ListItem<'_>,
    body: &'b mut Vec<u8>,
    into: &mut [ListEntry<'b>],
) -> Result<Listing<'b>, Box<dyn std::error::Error>> {
    let now = crate::now()?;
    let stamp = stamp(template, bytes, credentials, &now)?;
    let encode = |buf: &mut [u8]| s3::encode_list(template, bytes, item, &now, buf).map(drop);
    let mut buf = vec![0; crate::required(encode)?];
    let unsigned = s3::encode_list(template, bytes, item, &now, &mut buf)?;
    let request = sigv4::sign(&stamp, &unsigned, &mut buf);

    let (parts, mut incoming) = crate::send(&request, None)?;
    match s3::accept_list_head(crate::head(&parts))? {
        ListHeadOutcome::Page { .. } => {
            // S3 often sends a page without its length, so the read is
            // capped whatever the head says.
            *body = crate::page_body(&mut incoming)?;
            s3::fill_listing(body, into).map_err(Into::into)
        }
        ListHeadOutcome::NeedErrorBody(failure) => Err(not_listed(s3::accept_list_error_body(
            failure,
            &crate::error_body(&mut incoming),
        ))),
        outcome => Err(not_listed(outcome)),
    }
}

fn not_listed(outcome: ListHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 listed no keys: {outcome}").into()
}
