//! Link-time proof that the request path does not require a global allocator.

#![cfg_attr(not(feature = "std"), no_std)]

use borink_object_storage_crypto::sigv4;
use borink_object_storage_proto::azure;
use borink_object_storage_proto::azure::{AzureNamespace, BlobProperty, Container, PropertySet};
use borink_object_storage_proto::s3::{self, Bucket, PayloadSigning, Service};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{
    ChecksumKind, ChecksumValue, GetHeadOutcome, GetItem, GetOptions, ListEntry, ListHeadOutcome,
    ListItem, ListMarker, ListOptions, MetadataPair, Payload, PutItem, PutOptions, ResponseHead,
    Timestamps, decode_property,
};

// Required to link this no_std artifact; the exported check does not panic.
#[cfg(all(feature = "link-check", not(feature = "std")))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Exercises request construction and response interpretation in a reachable symbol.
#[unsafe(no_mangle)]
pub extern "C" fn object_storage_without_an_allocator() -> usize {
    let Ok(container) = Container::new("https://account", "container") else {
        return 1;
    };
    let now = Timestamps::from_unix(1_787_400_000);
    let options = GetOptions::new();
    let mut bytes = [0; 256];
    let Ok(template) = azure::prepare_get(
        &container,
        "token",
        AzureNamespace::Unknown,
        &options,
        &mut bytes,
    ) else {
        return 3;
    };
    let get = GetItem::new("object");
    let mut buf = [0; 512];
    let Ok(request) = azure::encode_get(&template, &bytes, &get, &now, &mut buf) else {
        return 3;
    };
    let headers = [("content-length", b"4".as_slice())];
    let Ok(GetHeadOutcome::Body { body, .. }) = azure::accept_get_head(
        get.shape(&options),
        ResponseHead::from_headers(200, headers),
    ) else {
        return 4;
    };
    request.url(&buf).len()
        + body.expected_len.unwrap_or_default() as usize
        + listing(&container, &now)
        + computed_checksums(&container, &now)
        + signed_requests(&now)
}

// A signed request derives its key from the secret in a fixed array on the
// stack, and hashes its canonical request where it was written. Neither
// SHA-256 implementation asks for memory either.
fn signed_requests(now: &Timestamps) -> usize {
    let Ok(bucket) = Bucket::new("https://s3.example.com", "bucket", "auto", Service::Aws) else {
        return 10;
    };
    let Ok(credentials) = Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "secret",
        borink_object_storage_crypto::wipe,
    ) else {
        return 11;
    };
    let headers = [("content-length", b"4".as_slice())];
    let options = GetOptions::new();
    let Ok(GetHeadOutcome::Body { .. }) = s3::accept_get_head(
        Service::Aws,
        GetItem::new("object").shape(&options),
        ResponseHead::from_headers(200, headers),
    ) else {
        return 13;
    };
    signed::<borink_object_storage_crypto::Sha256RustCrypto>(&bucket, &credentials, now)
        + signed::<borink_object_storage_crypto::Sha256Minimal>(&bucket, &credentials, now)
}

// A read and a write from templates, signed. The templates, the requests and
// every hash state are on this stack frame.
fn signed<S: borink_object_storage_crypto::Sha256>(
    bucket: &Bucket<'_>,
    credentials: &Credentials<'_>,
    now: &Timestamps,
) -> usize {
    let options = GetOptions::new();
    let mut bytes = [0; 768];
    let Ok(template) = s3::prepare_get(bucket, credentials, &options, &mut bytes) else {
        return 15;
    };
    let Ok(key) = sigv4::signing_key::<S>(&template, &bytes, credentials, now) else {
        return 16;
    };
    let Ok(stamp) = sigv4::stamp(&key, &template, &bytes, now) else {
        return 17;
    };
    let mut buf = [0; 1024];
    let Ok(unsigned) = s3::encode_get(&template, &bytes, &GetItem::new("object"), now, &mut buf)
    else {
        return 12;
    };
    let mut written = sigv4::sign(&stamp, &unsigned, &mut buf).url(&buf).len();

    let metadata = [MetadataPair {
        name: "Colour",
        value: "dark  red",
    }];
    let options = PutOptions {
        metadata: &metadata,
        checksum: Some(ChecksumKind::Md5),
        ..PutOptions::default()
    };
    let mut bytes = [0; 1024];
    let Ok(template) = s3::prepare_put(
        bucket,
        credentials,
        &options,
        PayloadSigning::Signed,
        &mut bytes,
    ) else {
        return 18;
    };
    let content = b"0123456789";
    let item = PutItem {
        checksum: Some(ChecksumValue::Digest(digest::<
            borink_object_storage_crypto::Md5RustCrypto,
        >(content))),
        ..PutItem::new("object", Payload::Slice(content))
    };
    let sha256 = sigv4::sha256::<S>(content);
    let mut buf = [0; 1024];
    let Ok(unsigned) = s3::encode_put(&template, &bytes, &item, Some(&sha256), now, &mut buf)
    else {
        return 14;
    };
    written += sigv4::sign(&stamp, &unsigned, &mut buf).url(&buf).len();
    written
}

// A checksum of content, computed into a value on this stack frame.
fn digest<C: borink_object_storage_crypto::Checksum>(
    content: &[u8],
) -> borink_object_storage_proto::Digest {
    let mut checksum = C::default();
    checksum.update(content);
    checksum.finish()
}

// A write sends the checksum that a `Checksum` computed over its content.
// Neither the encoder nor either implementation asks for memory.
fn computed_checksums(container: &Container<'_>, now: &Timestamps) -> usize {
    let mut written = 0;
    let content = b"0123456789";
    for (kind, value) in [
        (
            ChecksumKind::Crc64,
            digest::<borink_object_storage_crypto::Crc64>(content),
        ),
        (
            ChecksumKind::Md5,
            digest::<borink_object_storage_crypto::Md5RustCrypto>(content),
        ),
    ] {
        let options = PutOptions {
            checksum: Some(kind),
            ..PutOptions::default()
        };
        let mut bytes = [0; 256];
        let Ok(template) = azure::prepare_put(
            container,
            "token",
            AzureNamespace::Unknown,
            &options,
            &mut bytes,
        ) else {
            return 9;
        };
        let item = PutItem {
            checksum: Some(ChecksumValue::Digest(value)),
            ..PutItem::new("object", Payload::Slice(content))
        };
        let mut buf = [0; 512];
        let Ok(request) = azure::encode_put(&template, &bytes, &item, now, &mut buf) else {
            return 9;
        };
        written += request.url(&buf).len();
    }
    written
}

// A listing reads a document out of a buffer and decodes the text in it where
// it stands, so it is the one operation that could want scratch. It does not.
fn listing(container: &Container<'_>, now: &Timestamps) -> usize {
    let options = ListOptions {
        delimiter: Some("/"),
        max_results: Some(2),
        ..ListOptions::default()
    };
    let mut bytes = [0; 256];
    let Ok(template) = azure::prepare_list(
        container,
        "token",
        AzureNamespace::Unknown,
        &options,
        &mut bytes,
    ) else {
        return 5;
    };
    let mut buf = [0; 512];
    let item = ListItem::new("directory/").after(ListMarker::Text("marker"));
    let Ok(request) = azure::encode_list(&template, &bytes, &item, now, &mut buf) else {
        return 5;
    };
    let url = request.url(&buf).len();
    let Ok(ListHeadOutcome::Page { .. }) = azure::accept_list_head(ResponseHead::new(200)) else {
        return 6;
    };
    let mut body = *b"<EnumerationResults><Blobs><Blob><Name>a&amp;b</Name><Properties>\
<Content-Length>8</Content-Length></Properties></Blob><Blob><Name>c</Name><Properties>\
<Content-Length>9</Content-Length></Properties></Blob></Blobs>\
<NextMarker>next</NextMarker></EnumerationResults>";
    // The entries borrow the body, so the array that holds them belongs to
    // the read.
    let mut first = [ListEntry::default(); 2];
    let Ok(page) = azure::fill_listing(&mut body, &mut first) else {
        return 7;
    };
    // A property that the entry does not carry is read out of its own bytes,
    // and decoding one is a copy into the caller's buffer. Neither allocates.
    let mut into = [0; 16];
    let length = first[0]
        .property("Content-Length")
        .and_then(|value| decode_property(value, &mut into))
        .map_or(0, <[u8]>::len)
        + first[0].properties().count();
    let key = first[0].key.len() + first[1].key.len() + length;

    // The same page read into the caller's own entry type, keeping one
    // property of each entry as the page is read.
    let mut body = *b"<EnumerationResults><Blobs><Blob><Name>a</Name><Properties>\
<Content-Length>8</Content-Length><AccessTier>Hot</AccessTier></Properties></Blob></Blobs>\
<NextMarker /></EnumerationResults>";
    let wanted = PropertySet::of(&[BlobProperty::AccessTier]);
    let mut picked = [(0usize, None); 1];
    let Ok(again) = azure::fill_listing_with(&mut body, &mut picked, wanted, |entry, values| {
        (entry.key.len(), values.get(BlobProperty::AccessTier))
    }) else {
        return 8;
    };
    let tier = picked[0].1.map_or(0, <[u8]>::len);

    url + key
        + page.filled
        + page
            .next_marker
            .and_then(ListMarker::text)
            .unwrap_or_default()
            .len()
        + again.filled
        + tier
}
