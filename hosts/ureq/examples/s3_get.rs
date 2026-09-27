//! Runs one S3 GET and writes the object body to standard output.
//!
//! Set `S3_COMPATIBLE` to any value for a service other than AWS.

use std::env;
use std::io::Write;

use borink_object_storage_proto::s3::{Bucket, Objects, Service};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_ureq::s3;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("AWS_ENDPOINT_URL")?;
    let bucket = env::var("S3_BUCKET")?;
    let region = env::var("AWS_REGION")?;
    let key_id = env::var("AWS_ACCESS_KEY_ID")?;
    let secret = env::var("AWS_SECRET_ACCESS_KEY")?;
    let session_token = env::var("AWS_SESSION_TOKEN").ok();
    let key = env::args().nth(1).ok_or("missing object key")?;

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
    std::io::stdout().write_all(&s3::get(&objects, &key)?)?;
    Ok(())
}
