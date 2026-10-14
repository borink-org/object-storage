//! Every byte of a request head is in the caller's buffer, at a known offset.

use borink_object_storage_proto::azure::{self, Template};
use borink_object_storage_proto::azure::{AzureNamespace, Container};
use borink_object_storage_proto::{
    ConditionKind, ConditionValue, DeleteKind, DeleteOptions, GetItem, GetOptions, ObjectItem,
    Payload, PutItem, PutOptions, RequestedRange, Result, Span, Timestamps, WireRequest,
};

fn container() -> Container<'static> {
    Container::new("https://account.blob.core.windows.net", "container").unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

fn template(prepare: impl Fn(&mut [u8]) -> Result<Template>) -> (Template, Vec<u8>) {
    let mut bytes = vec![0; 512];
    (prepare(&mut bytes).unwrap(), bytes)
}

// The parts of a request, kept after the request read from the buffer is
// dropped, so the parts are checked against the buffer itself.
struct Parts {
    url: (Span, String),
    head: (Span, Vec<u8>),
    fields: (Span, usize),
}

fn parts(request: &WireRequest, buf: &[u8]) -> Parts {
    Parts {
        url: (request.url_span(), request.url(buf).to_owned()),
        head: (request.head_span(), request.head(buf).to_vec()),
        fields: (request.fields_span(), request.headers(buf).count()),
    }
}

// Each offset names the bytes that the slice did.
fn check_offsets(parts: &Parts, buf: &[u8]) {
    let at = |span: Span| &buf[span.start..span.start + span.len];
    let (head, fields) = (parts.head.0, parts.fields.0);
    assert_eq!(at(parts.url.0), parts.url.1.as_bytes());
    assert_eq!(at(head), parts.head.1.as_slice());
    assert!(head.start <= fields.start && fields.start + fields.len + 2 == head.start + head.len);
    let lines = std::str::from_utf8(at(fields)).unwrap();
    assert_eq!(lines.lines().count(), parts.fields.1);
}

// A request from a template names its URL, its head and its fields by
// offset, and each offset names the bytes that the slice does.
#[test]
fn a_request_names_every_part_of_it_by_offset() {
    let ranged = GetOptions {
        condition: ConditionKind::IfNoneMatch,
        ..GetOptions::new().ranged()
    };
    for (options, get) in [
        (GetOptions::new(), GetItem::new("directory/a key+é")),
        (
            ranged,
            GetItem {
                condition_value: Some(ConditionValue::ETag(b"\"etag\"")),
                ..GetItem::new("object.bin")
                    .with_range(RequestedRange::Bounded { start: 2, end: 6 })
            },
        ),
        (GetOptions::head(), GetItem::new("object.bin")),
    ] {
        let (template, bytes) = template(|buf| {
            azure::prepare_get(
                &container(),
                "access-token",
                AzureNamespace::Unknown,
                &options,
                buf,
            )
        });
        let mut buf = vec![0; 512];
        let request = azure::encode_get(&template, &bytes, &get, &now(), &mut buf).unwrap();
        let parts = parts(&request, &buf);
        check_offsets(&parts, &buf);
    }

    let create = PutOptions {
        condition: ConditionKind::IfNoneMatch,
        ..PutOptions::default()
    };
    for (options, put) in [
        (
            PutOptions::default(),
            PutItem::new("object.bin", Payload::Slice(b"contents")),
        ),
        (
            create,
            PutItem {
                condition_value: Some(ConditionValue::ETag(b"*")),
                ..PutItem::new("object.bin", Payload::Streamed { len: 1024 })
            },
        ),
    ] {
        let (template, bytes) = template(|buf| {
            azure::prepare_put(
                &container(),
                "access-token",
                AzureNamespace::Unknown,
                &options,
                buf,
            )
        });
        let mut buf = vec![0; 512];
        let request = azure::encode_put(&template, &bytes, &put, &now(), &mut buf).unwrap();
        let parts = parts(&request, &buf);
        check_offsets(&parts, &buf);
    }

    let with_snapshots = DeleteOptions {
        kind: DeleteKind::ObjectAndSnapshots,
        condition: ConditionKind::IfMatch,
        ..DeleteOptions::new()
    };
    for (options, delete) in [
        (DeleteOptions::new(), ObjectItem::new("object.bin")),
        (
            with_snapshots,
            ObjectItem {
                condition_value: Some(ConditionValue::ETag(b"\"etag\"")),
                ..ObjectItem::new("object.bin")
            },
        ),
    ] {
        let (template, bytes) = template(|buf| {
            azure::prepare_delete(
                &container(),
                "access-token",
                AzureNamespace::Unknown,
                &options,
                buf,
            )
        });
        let mut buf = vec![0; 512];
        let request = azure::encode_delete(&template, &bytes, &delete, &now(), &mut buf).unwrap();
        let parts = parts(&request, &buf);
        check_offsets(&parts, &buf);
    }
}
