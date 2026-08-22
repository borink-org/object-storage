#![no_std]

use borink_object_storage::{Blobs, Container, RequestWorkspace, Response, Timestamps};

// Required to link this no_std artifact; the exported check does not panic.
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

// Keeping the complete GET path reachable makes any reachable allocation fail to link.
#[unsafe(no_mangle)]
pub extern "C" fn azure_get_without_an_allocator() -> usize {
    let Ok(container) = Container::new("https://account", "container") else {
        return 1;
    };
    let Ok(blobs) = Blobs::new(container, "token") else {
        return 2;
    };
    let mut storage = [0; 256];
    let mut workspace = RequestWorkspace::new(&mut storage);
    let now = Timestamps::from_unix(1_787_400_000);
    let Ok(request) = blobs.get_request(&mut workspace, "object", &now) else {
        return 3;
    };
    let Ok(body) = blobs.interpret_get(Response::new(200, b"body")) else {
        return 4;
    };
    request.url().len() + body.len()
}
