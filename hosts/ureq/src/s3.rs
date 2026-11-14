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
    ListHeadOutcome, ListItem, ListOptions, Listing, ObjectItem, Payload, PutHeadOutcome, PutItem,
    PutOptions, Timestamps,
};

use crate::Prepared;

// SigV4 signs with the key of the UTC day, and a Unix day is exactly this
// long.
const SECONDS_PER_DAY: u64 = 86_400;

/// A client of one S3 bucket, which signs its requests with one set of
/// credentials.
///
/// It writes the template of each kind of request once, when it is made,
/// and writes every request into one buffer that it keeps. Once that buffer
/// has grown to fit the longest request, each request is written once. It
/// derives the signing key once a day, and sends with one `ureq` agent,
/// which keeps its connections.
pub struct Objects<'a> {
    credentials: Credentials<'a>,
    // The day's signing key, with the day it signs on, counted in days
    // since the Unix epoch.
    signing_key: Option<(u64, sigv4::SigningKey<Sha256RustCrypto>)>,
    get: Prepared<Template>,
    put: Prepared<Template>,
    delete: Prepared<Template>,
    list: Prepared<Template>,
    buf: Vec<u8>,
    agent: ureq::Agent,
}

impl<'a> Objects<'a> {
    /// A client of `bucket` that signs with `credentials`, and lists as
    /// `list` says. A put signs the SHA-256 of its content.
    ///
    /// # Errors
    ///
    /// Returns the error of a template that the crate refuses to write.
    pub fn new(
        bucket: &Bucket<'_>,
        credentials: Credentials<'a>,
        list: &ListOptions<'_>,
    ) -> Result<Self, Error> {
        let signing = PayloadSigning::Signed;
        Ok(Self {
            get: Prepared::new(|buf| {
                s3::prepare_get(bucket, &credentials, &GetOptions::new(), buf)
            })?,
            put: Prepared::new(|buf| {
                s3::prepare_put(bucket, &credentials, &PutOptions::default(), signing, buf)
            })?,
            delete: Prepared::new(|buf| {
                s3::prepare_delete(bucket, &credentials, &DeleteOptions::new(), buf)
            })?,
            list: Prepared::new(|buf| s3::prepare_list(bucket, &credentials, list, buf))?,
            credentials,
            signing_key: None,
            buf: Vec::new(),
            agent: crate::new_agent(),
        })
    }

    // The stamp for signing at `now`, `unix` seconds after the epoch, from
    // the day's signing key, which this derives on the first request of each
    // day. Every template of the bucket has the same scope, so the get
    // template serves them all.
    fn stamp(
        &mut self,
        unix: u64,
        now: &Timestamps,
    ) -> Result<sigv4::Stamp<Sha256RustCrypto>, Error> {
        let day = unix / SECONDS_PER_DAY;
        let Prepared { template, bytes } = &self.get;
        let key = match self.signing_key {
            Some((signs_on, key)) if signs_on == day => key,
            _ => {
                let key = sigv4::signing_key(template, bytes, &self.credentials, now)?;
                self.signing_key = Some((day, key));
                key
            }
        };
        sigv4::stamp(&key, template, bytes, now)
    }

    /// Gets the object `key`, and returns its bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the request could not be sent, or if S3 returned
    /// no object.
    pub fn get(&mut self, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let unix = crate::unix_now()?;
        let now = Timestamps::from_unix(unix);
        let stamp = self.stamp(unix, &now)?;
        let item = GetItem::new(key);
        let Prepared { template, bytes } = &self.get;
        let placed = crate::grown_to_fit(&mut self.buf, |buf| {
            let unsigned = s3::encode_get(template, bytes, &item, &now, buf)?;
            Ok(sigv4::sign(&stamp, &unsigned, buf).placement())
        })?;

        let (parts, mut body) = crate::send(&self.agent, &placed.request(&self.buf), None)?;
        let shape = item.shape(&GetOptions::new());
        match s3::accept_get_head(template.service(), shape, crate::response_head(&parts))? {
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

    /// Puts `content` as the whole object `key`.
    ///
    /// The request signs the SHA-256 of `content`, which this method
    /// computes.
    ///
    /// # Errors
    ///
    /// Returns an error if the request could not be sent, or if S3 stored no
    /// object.
    pub fn put(&mut self, key: &str, content: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let unix = crate::unix_now()?;
        let now = Timestamps::from_unix(unix);
        let stamp = self.stamp(unix, &now)?;
        let item = PutItem::new(key, Payload::Slice(content));
        let digest = sigv4::sha256::<Sha256RustCrypto>(content);
        let Prepared { template, bytes } = &self.put;
        let placed = crate::grown_to_fit(&mut self.buf, |buf| {
            let unsigned = s3::encode_put(template, bytes, &item, Some(&digest), &now, buf)?;
            Ok(sigv4::sign(&stamp, &unsigned, buf).placement())
        })?;

        let request = placed.request(&self.buf);
        let (parts, mut body) = crate::send(&self.agent, &request, Some(content))?;
        let shape = item.shape(&PutOptions::default());
        match s3::accept_put_head(shape, crate::response_head(&parts))? {
            PutHeadOutcome::Created { .. } => Ok(()),
            PutHeadOutcome::NeedErrorBody(failure) => Err(not_stored(s3::accept_put_error_body(
                shape,
                failure,
                &crate::error_body(&mut body),
            ))),
            outcome => Err(not_stored(outcome)),
        }
    }

    /// Deletes the whole object `key`.
    ///
    /// S3 answers the removal of a missing object as it answers any other
    /// removal, so this method cannot report one.
    ///
    /// # Errors
    ///
    /// Returns an error if the request could not be sent, or if S3 removed
    /// no object.
    pub fn delete(&mut self, key: &str) -> Result<(), Box<dyn std::error::Error>> {
        let unix = crate::unix_now()?;
        let now = Timestamps::from_unix(unix);
        let stamp = self.stamp(unix, &now)?;
        let item = ObjectItem::new(key);
        let Prepared { template, bytes } = &self.delete;
        let placed = crate::grown_to_fit(&mut self.buf, |buf| {
            let unsigned = s3::encode_delete(template, bytes, &item, &now, buf)?;
            Ok(sigv4::sign(&stamp, &unsigned, buf).placement())
        })?;

        let (parts, mut body) = crate::send(&self.agent, &placed.request(&self.buf), None)?;
        let shape = DeleteOptions::new().shape();
        let head = crate::response_head(&parts);
        match s3::accept_delete_head(template.service(), shape, head)? {
            DeleteHeadOutcome::Accepted => Ok(()),
            DeleteHeadOutcome::NeedErrorBody(failure) => Err(not_removed(
                s3::accept_delete_error_body(shape, failure, &crate::error_body(&mut body)),
            )),
            outcome => Err(not_removed(outcome)),
        }
    }

    /// Lists one page, and reads it into `body`.
    ///
    /// The entries this writes into `into` borrow `body`. An array of the
    /// `max_results` of the client's list options always holds a whole page,
    /// and so does one of 1,000 entries for AWS. A smaller one is refused
    /// with the number of entries the page holds.
    ///
    /// # Errors
    ///
    /// Returns an error if the request could not be sent, or if S3 listed
    /// nothing.
    pub fn list<'b>(
        &mut self,
        item: &ListItem<'_>,
        body: &'b mut Vec<u8>,
        into: &mut [ListEntry<'b>],
    ) -> Result<Listing<'b>, Box<dyn std::error::Error>> {
        let unix = crate::unix_now()?;
        let now = Timestamps::from_unix(unix);
        let stamp = self.stamp(unix, &now)?;
        let Prepared { template, bytes } = &self.list;
        let placed = crate::grown_to_fit(&mut self.buf, |buf| {
            let unsigned = s3::encode_list(template, bytes, item, &now, buf)?;
            Ok(sigv4::sign(&stamp, &unsigned, buf).placement())
        })?;

        let (parts, mut incoming) = crate::send(&self.agent, &placed.request(&self.buf), None)?;
        match s3::accept_list_head(crate::response_head(&parts))? {
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
}

fn no_object(outcome: GetHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 returned no object: {outcome}").into()
}

fn not_stored(outcome: PutHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 stored no object: {outcome}").into()
}

fn not_removed(outcome: DeleteHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 removed no object: {outcome}").into()
}

fn not_listed(outcome: ListHeadOutcome<'_>) -> Box<dyn std::error::Error> {
    format!("S3 listed no keys: {outcome}").into()
}

#[cfg(test)]
mod tests {
    use borink_object_storage_crypto::wipe;
    use borink_object_storage_proto::s3::Service;

    use super::*;

    // The signature that a stamp freshly derived at `unix` gives a request.
    fn fresh_signature(objects: &Objects<'_>, unix: u64) -> [u8; 32] {
        let now = Timestamps::from_unix(unix);
        let Prepared { template, bytes } = &objects.get;
        let key = sigv4::signing_key(template, bytes, &objects.credentials, &now).unwrap();
        let stamp: sigv4::Stamp<Sha256RustCrypto> =
            sigv4::stamp(&key, template, bytes, &now).unwrap();
        stamp.sign(&[7; 32])
    }

    #[test]
    fn the_signing_key_is_kept_for_its_day_and_derived_again_on_the_next() {
        let bucket = Bucket::new(
            "https://s3.example.com",
            "bucket",
            "us-east-1",
            Service::Aws,
        )
        .unwrap();
        let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe).unwrap();
        let mut objects = Objects::new(&bucket, credentials, &ListOptions::default()).unwrap();
        let morning = 1_787_400_000 / SECONDS_PER_DAY * SECONDS_PER_DAY + 3_600;
        let day = morning / SECONDS_PER_DAY;
        let stamp = objects
            .stamp(morning, &Timestamps::from_unix(morning))
            .unwrap();
        assert_eq!(stamp.sign(&[7; 32]), fresh_signature(&objects, morning));

        // A key of another secret, kept for today, signs the rest of today.
        let other = Credentials::new("AKIAIOSFODNN7EXAMPLE", "other", wipe).unwrap();
        let Prepared { template, bytes } = &objects.get;
        let now = Timestamps::from_unix(morning);
        let planted = sigv4::signing_key(template, bytes, &other, &now).unwrap();
        objects.signing_key = Some((day, planted));
        let later = morning + 10_000;
        let stamp = objects.stamp(later, &Timestamps::from_unix(later)).unwrap();
        assert_ne!(stamp.sign(&[7; 32]), fresh_signature(&objects, later));

        // Tomorrow's request derives tomorrow's key from the secret again.
        let tomorrow = morning + SECONDS_PER_DAY;
        let stamp = objects
            .stamp(tomorrow, &Timestamps::from_unix(tomorrow))
            .unwrap();
        assert_eq!(
            objects.signing_key.map(|(signs_on, _)| signs_on),
            Some(day + 1)
        );
        assert_eq!(stamp.sign(&[7; 32]), fresh_signature(&objects, tomorrow));
    }
}
