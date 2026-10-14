//! Writes three small blobs from one template, with `ureq` called directly.
//!
//! The template holds the endpoint, the token and every field the writes
//! share. Each write then fills in only its key, its length and the time.

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use borink_object_storage_proto::azure;
use borink_object_storage_proto::azure::{AzureNamespace, Container};
use borink_object_storage_proto::{
    Payload, PutHeadOutcome, PutItem, PutOptions, ResponseHead, Timestamps,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("AZURE_STORAGE_ENDPOINT")?;
    let container = env::var("AZURE_STORAGE_CONTAINER")?;
    let token = env::var("AZURE_STORAGE_ACCESS_TOKEN")?;
    let container = Container::new(&endpoint, &container)?;

    // An empty buffer is refused with the size the template needs.
    let options = PutOptions::default();
    let prepare = |buf: &mut [u8]| {
        azure::prepare_put(&container, &token, AzureNamespace::Unknown, &options, buf)
    };
    let size = match prepare(&mut []) {
        Err(borink_object_storage_proto::Error::Capacity(capacity)) => capacity.required,
        other => return Err(format!("{other:?}").into()),
    };
    let mut bytes = vec![0; size];
    let template = prepare(&mut bytes)?;

    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let now = Timestamps::from_unix(unix);
    let mut buf = vec![0; 4096];
    for (key, content) in [("a.txt", "one"), ("b.txt", "two"), ("c.txt", "three")] {
        let item = PutItem::new(key, Payload::Slice(content.as_bytes()));
        let placed = azure::encode_put(&template, &bytes, &item, &now, &mut buf)?;
        let request = placed.request(&buf);

        let mut outgoing = ureq::http::Request::builder()
            .method(request.method().as_str())
            .uri(request.url());
        for (name, value) in request.headers() {
            outgoing = outgoing.header(name, value);
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let response = agent.run(outgoing.body(content.as_bytes())?)?;

        // The head and the body are two values, so the head can be read while
        // the body stays readable.
        let (parts, _body) = response.into_parts();
        let head = ResponseHead::from_headers(
            parts.status.as_u16(),
            parts
                .headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        match azure::accept_put_head(item.shape(&options), head)? {
            PutHeadOutcome::Created { meta } => {
                println!(
                    "{key}: etag={}",
                    String::from_utf8_lossy(meta.e_tag.unwrap_or_default())
                );
            }
            outcome => return Err(format!("{key}: {outcome}").into()),
        }
    }
    Ok(())
}
