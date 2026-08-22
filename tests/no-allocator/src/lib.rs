#![no_std]

use borink_object_storage::{Blobs, Container, RequestWorkspace, Response};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

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
    let Ok(request) = blobs.get_request(&mut workspace, "object", "Sat, 22 Aug 2026 12:00:00 GMT")
    else {
        return 3;
    };
    let Ok(body) = blobs.interpret_get(Response::new(200, b"body")) else {
        return 4;
    };
    request.url().len() + body.len()
}
