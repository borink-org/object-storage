//! Link-time proof that the request path does not require a global allocator.

#![cfg_attr(not(feature = "std"), no_std)]

use borink_object_storage_proto::{
    Blobs, Container, Fill, GetHeadOutcome, ListEntry, ListHeadOutcome, PhysicalGet, PhysicalList,
    ResponseHead, Timestamps, layered,
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
    let Ok(blobs) = Blobs::new(container, "token") else {
        return 2;
    };
    let mut buf = [0; 256];
    let now = Timestamps::from_unix(1_787_400_000);
    let get = PhysicalGet::new("object");
    let Ok(request) = blobs.encode_get(&mut buf, &get, &now) else {
        return 3;
    };
    let headers = [("content-length", b"4".as_slice())];
    let Ok(GetHeadOutcome::Body { body, .. }) =
        blobs.accept_get_head(get.shape(), ResponseHead::from_headers(200, headers))
    else {
        return 4;
    };
    request.url().len() + body.expected_len.unwrap_or_default() as usize + listing(&blobs, &now)
}

// A listing reads a document out of a buffer and decodes the text in it where
// it stands, so it is the one operation that could want scratch. It does not.
fn listing(blobs: &Blobs<'_>, now: &Timestamps) -> usize {
    let mut buf = [0; 256];
    let list = PhysicalList {
        delimited: true,
        max_results: Some(2),
        ..PhysicalList::new("directory/")
    };
    let Ok(request) = blobs.encode_list(&mut buf, &list, now) else {
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
    // One entry at a time, so the walk that stops short and the one that
    // resumes are both linked. The entries borrow the body, so the array that
    // holds them belongs to the round that reads them.
    let mut first = [ListEntry::default(); 1];
    let Ok(Fill::Partial { filled, resume }) = blobs.fill_listing(&mut body, &mut first) else {
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
    let key = filled + first[0].key.len() + length;
    let mut rest = [ListEntry::default(); 1];
    let Ok(Fill::Page(page)) = blobs.resume_listing(&mut body, resume, &mut rest) else {
        return 8;
    };
    url + key + page.filled + page.next_marker.unwrap_or_default().len()
}
