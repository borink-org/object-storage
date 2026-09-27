//! Link-time proof that the request path does not require a global allocator.

#![cfg_attr(not(feature = "std"), no_std)]

use borink_object_storage_proto::s3::{Bucket, Objects, PayloadHash, Service};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{
    BlobProperty, Blobs, ChecksumKind, Container, GetHeadOutcome, HeaderSpan, ListEntry,
    ListHeadOutcome, MetadataPair, Payload, PhysicalGet, PhysicalList, PhysicalPut, PropertySet,
    ResponseHead, Timestamps, TransactionalChecksum, WriteOptions, layered,
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
    let mut request_headers = [HeaderSpan::default(); 8];
    let Ok(container) = Container::new("https://account", "container") else {
        return 1;
    };
    let Ok(blobs) = Blobs::new(container, "token") else {
        return 2;
    };
    let blobs = blobs
        .with_checksum(borink_object_storage_crypto::CRC64)
        .with_checksum(borink_object_storage_crypto::MD5_RUSTCRYPTO);
    let mut buf = [0; 256];
    let now = Timestamps::from_unix(1_787_400_000);
    let get = PhysicalGet::new("object");
    let Ok(request) = blobs.encode_get(&mut buf, &mut request_headers, &get, &now) else {
        return 3;
    };
    let headers = [("content-length", b"4".as_slice())];
    let Ok(GetHeadOutcome::Body { body, .. }) =
        blobs.accept_get_head(get.shape(), ResponseHead::from_headers(200, headers))
    else {
        return 4;
    };
    request.url().len()
        + body.expected_len.unwrap_or_default() as usize
        + listing(&blobs, &now)
        + computed_checksums(&blobs, &now)
        + signed_requests(&now)
}

// A signed request hashes its canonical form as it is written, and derives
// its key from the secret in a fixed array on the stack. Neither provider
// asks for memory either.
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
    let mut written = 0;
    for sha256 in [
        borink_object_storage_crypto::SHA256_RUSTCRYPTO,
        borink_object_storage_crypto::SHA256_MINIMAL,
    ] {
        let objects = Objects::new(bucket, credentials, sha256)
            .with_signing_key(now)
            .with_checksum(borink_object_storage_crypto::MD5_RUSTCRYPTO);
        let mut request_headers = [HeaderSpan::default(); 12];
        let mut buf = [0; 768];
        let get = PhysicalGet::new("object");
        let Ok(request) = objects.encode_get(&mut buf, &mut request_headers, &get, now) else {
            return 12;
        };
        written += request.url().len();
        let headers = [("content-length", b"4".as_slice())];
        let Ok(GetHeadOutcome::Body { .. }) =
            objects.accept_get_head(get.shape(), ResponseHead::from_headers(200, headers))
        else {
            return 13;
        };
        let metadata = [MetadataPair {
            name: "Colour",
            value: "dark  red",
        }];
        let put = PhysicalPut {
            metadata: &metadata,
            options: WriteOptions {
                checksum: Some(TransactionalChecksum::Compute(ChecksumKind::Md5)),
                ..WriteOptions::default()
            },
            ..PhysicalPut::new("object")
        };
        let mut request_headers = [HeaderSpan::default(); 12];
        let mut buf = [0; 768];
        let Ok(request) = objects.encode_put(
            &mut buf,
            &mut request_headers,
            &put,
            Payload::Slice(b"0123456789"),
            PayloadHash::Compute,
            now,
        ) else {
            return 14;
        };
        written += request.url().len();
    }
    written
}

// A computed checksum runs a provider over the content while the head is
// written. The state is a slot on this stack frame, so neither the encoder
// nor either implementation asks for memory.
fn computed_checksums(blobs: &Blobs<'_>, now: &Timestamps) -> usize {
    let mut written = 0;
    for kind in [ChecksumKind::Crc64, ChecksumKind::Md5] {
        let mut request_headers = [HeaderSpan::default(); 8];
        let mut buf = [0; 256];
        let options = WriteOptions {
            checksum: Some(TransactionalChecksum::Compute(kind)),
            ..WriteOptions::default()
        };
        let put = PhysicalPut {
            options,
            ..PhysicalPut::new("object")
        };
        let Ok(request) = blobs.encode_put(
            &mut buf,
            &mut request_headers,
            &put,
            Payload::Slice(b"0123456789"),
            now,
        ) else {
            return 9;
        };
        written += request.url().len();
    }
    written
}

// A listing reads a document out of a buffer and decodes the text in it where
// it stands, so it is the one operation that could want scratch. It does not.
fn listing(blobs: &Blobs<'_>, now: &Timestamps) -> usize {
    let mut request_headers = [HeaderSpan::default(); 8];
    let mut buf = [0; 256];
    let list = PhysicalList {
        delimited: true,
        max_results: Some(2),
        ..PhysicalList::new("directory/")
    };
    let Ok(request) = blobs.encode_list(&mut buf, &mut request_headers, &list, now) else {
        return 5;
    };
    let url = request.url().len();
    let Ok(ListHeadOutcome::Page { .. }) = blobs.accept_list_head(ResponseHead::new(200)) else {
        return 6;
    };
    let mut body = *b"<EnumerationResults><Blobs><Blob><Name>a&amp;b</Name><Properties>\
<Content-Length>8</Content-Length></Properties></Blob><Blob><Name>c</Name><Properties>\
<Content-Length>9</Content-Length></Properties></Blob></Blobs>\
<NextMarker>next</NextMarker></EnumerationResults>";
    // The entries borrow the body, so the array that holds them belongs to
    // the read.
    let mut first = [ListEntry::default(); 2];
    let Ok(page) = blobs.fill_listing(&mut body, &mut first) else {
        return 7;
    };
    // A property that the entry does not carry is read out of its own bytes,
    // and decoding one is a copy into the caller's buffer. Neither allocates.
    let mut into = [0; 16];
    let length = first[0]
        .property("Content-Length")
        .and_then(|value| layered::decode_into(value, &mut into))
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
    let Ok(again) = blobs.fill_listing_with(&mut body, &mut picked, wanted, |entry, values| {
        (entry.key.len(), values.get(BlobProperty::AccessTier))
    }) else {
        return 8;
    };
    let tier = picked[0].1.map_or(0, <[u8]>::len);

    url + key + page.filled + page.next_marker.unwrap_or_default().len() + again.filled + tier
}
