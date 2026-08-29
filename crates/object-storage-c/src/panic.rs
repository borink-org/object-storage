//! What the archive does when Rust code panics.
//!
//! A static archive carries its own panic handler. On a hosted target the
//! `std` feature links the standard library, which brings one. A freestanding
//! target turns that feature off and gets the handler below.
//!
//! No call in this crate panics: each is total over its inputs.

/// Stops the processor where the panic happened.
///
/// Build with `--no-default-features` and wrap this crate to reset the board
/// instead.
#[cfg(not(feature = "std"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
