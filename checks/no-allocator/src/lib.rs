//! Link-time proof that the Azure GET path does not require a global allocator.

#![cfg_attr(not(feature = "std"), no_std)]

use borink_object_storage::{Blobs, Container, PhysicalGet, Response, Timestamps};

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
pub extern "C" fn azure_get_without_an_allocator() -> usize {
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
    let headers = [("content-length", "4")];
    let Ok(meta) = blobs.interpret_get(Response::new(200, headers), get.shape) else {
        return 4;
    };
    request.url().len() + meta.size as usize
}
