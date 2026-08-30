//! Link-time proof that the request path does not require a global allocator.

#![cfg_attr(not(feature = "std"), no_std)]

use borink_object_storage_proto::{
    Blobs, Container, GetHeadOutcome, PhysicalGet, ResponseHead, Timestamps,
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
    request.url().len() + body.expected_len.unwrap_or_default() as usize
}
