//! Every byte of a request head is in the caller's buffer, at a known offset.

use borink_object_storage_proto::{
    Blobs, ConditionKind, Container, DeleteKind, GetKind, MAX_HEADERS, Payload, PhysicalDelete,
    PhysicalGet, PhysicalPut, RequestedRange, Span, Timestamps, WireRequest, layered,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "container").unwrap(),
        "access-token",
    )
    .unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

// A request head, kept after the request that named it is dropped. The request
// borrows the buffer, so the parts are read back once it is gone.
struct Head {
    url: (Span, String),
    headers: Vec<((Span, String), (Span, String))>,
}

fn record(request: &WireRequest<'_>) -> Head {
    assert!(request.headers().len() <= MAX_HEADERS);
    assert_eq!(request.header_spans().len(), request.headers().len());
    Head {
        url: (request.url_span(), request.url().to_owned()),
        headers: request
            .headers()
            .zip(request.header_spans())
            .map(|((name, value), (name_span, value_span))| {
                ((name_span, name.to_owned()), (value_span, value.to_owned()))
            })
            .collect(),
    }
}

// The offsets name the same bytes that the slices did, and no part of the head
// falls outside the buffer.
fn check(head: &Head, buf: &[u8]) {
    let at = |span: Span| {
        assert!(span.start + span.len <= buf.len());
        std::str::from_utf8(&buf[span.start..span.start + span.len])
            .unwrap()
            .to_owned()
    };
    assert_eq!(at(head.url.0), head.url.1);
    for ((name_span, name), (value_span, value)) in &head.headers {
        assert_eq!(at(*name_span), *name);
        assert_eq!(at(*value_span), *value);
    }
}

#[test]
fn a_read_names_every_part_of_its_head_by_offset() {
    let blobs = blobs();
    for get in [
        PhysicalGet::new("directory/a key+é"),
        PhysicalGet {
            range: RequestedRange::Bounded { start: 2, end: 6 },
            condition: ConditionKind::IfNoneMatch,
            condition_value: Some(b"\"etag\""),
            ..PhysicalGet::new("object.bin")
        },
        PhysicalGet {
            range: RequestedRange::Offset(4),
            ..PhysicalGet::new("object.bin")
        },
        PhysicalGet {
            kind: GetKind::Metadata,
            ..PhysicalGet::new("object.bin")
        },
    ] {
        let mut buf = vec![0; layered::get_requirements(&blobs, &get, &now()).unwrap()];
        let head = record(&blobs.encode_get(&mut buf, &get, &now()).unwrap());
        check(&head, &buf);
    }
}

#[test]
fn a_write_names_every_part_of_its_head_by_offset() {
    let blobs = blobs();
    let content = Payload::Slice(b"contents");
    for put in [
        PhysicalPut::new("object.bin"),
        PhysicalPut {
            condition: ConditionKind::IfNoneMatch,
            condition_value: Some(b"*"),
            ..PhysicalPut::new("object.bin")
        },
    ] {
        let mut buf = vec![0; layered::put_requirements(&blobs, &put, content, &now()).unwrap()];
        let head = record(&blobs.encode_put(&mut buf, &put, content, &now()).unwrap());
        check(&head, &buf);
    }
}

#[test]
fn a_removal_names_every_part_of_its_head_by_offset() {
    let blobs = blobs();
    for delete in [
        PhysicalDelete::new("object.bin"),
        PhysicalDelete {
            kind: DeleteKind::ObjectAndSnapshots,
            condition: ConditionKind::IfMatch,
            condition_value: Some(b"\"etag\""),
            ..PhysicalDelete::new("object.bin")
        },
    ] {
        let mut buf = vec![0; layered::delete_requirements(&blobs, &delete, &now()).unwrap()];
        let head = record(&blobs.encode_delete(&mut buf, &delete, &now()).unwrap());
        check(&head, &buf);
    }
}

// The requirement covers the whole head, so an exactly sized buffer holds
// every byte that the request names and no byte more.
#[test]
fn the_requirement_is_the_end_of_the_last_part() {
    let blobs = blobs();
    let put = PhysicalPut {
        condition: ConditionKind::IfMatch,
        condition_value: Some(b"\"etag\""),
        ..PhysicalPut::new("object.bin")
    };
    let content = Payload::Streamed { len: 1024 };
    let required = layered::put_requirements(&blobs, &put, content, &now()).unwrap();
    let mut buf = vec![0; required];
    let head = record(&blobs.encode_put(&mut buf, &put, content, &now()).unwrap());

    let end = head
        .headers
        .iter()
        .map(|(_, (span, _))| span.start + span.len)
        .max()
        .unwrap();
    assert_eq!(end, required);
}
