//! Put

use borink_object_storage_proto::{
    Blobs, Container, Payload, PhysicalPut, PutHeadOutcome, PutShape, ResponseHead, Timestamps,
};
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};
use ureq::typestate::WithBody;

fn build_request<'a>(
    blobs: Blobs<'a>,
    key: &str,
    object_length: usize,
) -> (PutShape, ureq::RequestBuilder<WithBody>) {
    let mut buf = Vec::with_capacity(4096);
    let plan = PhysicalPut::new(key);
    let now = Timestamps::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let out = blobs
        .encode_put(
            buf.as_mut_slice(),
            &plan,
            // review: maybe Streamed is not a good name?
            // thinking about it, I think this part of the API is still quite weak
            Payload::Streamed {
                len: object_length as u64,
            },
            &now,
        )
        .unwrap();

    let mut req = ureq::put(out.url());
    for (header, name) in out.headers() {
        req = req.header(header, name);
    }

    (
        plan.shape(),
        req.config().http_status_as_error(false).build(),
    )
}

fn main() -> Result<(), Infallible> {
    // review(deferred): the blobs creation is less ergonomic than maybe you'd want
    let token = "my_token";
    let blobs = Blobs::new(
        Container::new("my_endpoint", "my_container").unwrap(),
        token,
    )
    .unwrap();

    let object = "wheek";
    let (shape, req) = build_request(blobs, "meerkat", object.len());

    let response = req.send(object).unwrap();

    // review(deferred): can we do better with this ResponseHead creation?
    let head = ResponseHead::from_headers(
        response.status().as_u16(),
        response
            .headers()
            .iter()
            // review(deferred): this in particular does not feel great.. this should be stuff Rust is good at right?
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    );
    match blobs.accept_put_head(shape, head).unwrap() {
        PutHeadOutcome::Created { meta } => {
            println!("etag={}", std::str::from_utf8(meta.e_tag.unwrap()).unwrap());
        }
        _ => panic!("Put failed!"),
    }

    Ok(())
}
