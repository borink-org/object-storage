//! Lists every key under one prefix, one page at a time.
//!
//! Set `S3_COMPATIBLE` to any value for a service other than AWS.

use std::env;

use borink_object_storage_proto::s3::{Bucket, Objects, Service};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{ListEntry, PhysicalList};
use borink_object_storage_ureq::s3;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("AWS_ENDPOINT_URL")?;
    let bucket = env::var("S3_BUCKET")?;
    let region = env::var("AWS_REGION")?;
    let key_id = env::var("AWS_ACCESS_KEY_ID")?;
    let secret = env::var("AWS_SECRET_ACCESS_KEY")?;
    let session_token = env::var("AWS_SESSION_TOKEN").ok();
    let prefix = env::args().nth(1).unwrap_or_default();

    let service = match env::var_os("S3_COMPATIBLE") {
        Some(_) => Service::Compatible,
        None => Service::Aws,
    };
    let bucket = Bucket::new(&endpoint, &bucket, &region, service)?;
    let mut credentials = Credentials::new(&key_id, &secret, borink_object_storage_crypto::wipe)?;
    if let Some(token) = &session_token {
        credentials = credentials.with_session_token(token)?;
    }
    let objects = Objects::new(
        bucket,
        credentials,
        borink_object_storage_crypto::SHA256_RUSTCRYPTO,
    );

    let mut marker: Option<String> = None;
    let mut body = Vec::new();
    loop {
        // The array holds a whole page, because it is as long as the page the
        // plan asks for. It borrows the body, so it belongs to the round that
        // reads it.
        let mut entries = vec![ListEntry::default(); 1000];
        let plan = PhysicalList {
            marker: marker.as_deref(),
            max_results: Some(1000),
            ..PhysicalList::new(&prefix)
        };
        let page = s3::list(&objects, &plan, &mut body, &mut entries)?;
        for entry in &entries[..page.filled] {
            println!("{}", entry.key);
        }
        // The next request reads into another body, so the token is copied
        // out of this one.
        match page.next_marker {
            Some(next) => marker = Some(next.to_owned()),
            None => return Ok(()),
        }
    }
}
