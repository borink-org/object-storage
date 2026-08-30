# Checks

A **behaviour test** asks whether the code computes the right answer. It lives in the `tests/` directory of the crate it tests and runs under that crate's test runner: `cargo test`, or `ctest` for `crates/object-storage-c`.

A **property proof** asks whether the built artifact keeps a promise. It lives here, and it asserts on the artifact — the build fails, or the symbols are read. It never asserts on execution, because running only covers the lines that ran, and a promise has to hold for the code a test never reaches.

Two of them:

- `freestanding/` links the C ABI archive into a Cortex-M7 image with no C library and no heap, then reads the symbols. `board.c` names what a C program calls and `board.cc` what a C++ one calls, and `board_main` calls into both, because the linker discards what nothing reaches. A helper left out of those files is one the image never carries, so the check would pass without ever seeing it. It also reports the flash and RAM the image takes.
- `no-allocator/` builds the Rust API a Rust consumer calls directly, `no_std` and with no global allocator, so a path that allocated would not compile. The image above covers the same code through the C ABI; this covers it through the Rust one.

`board.c` compiles with `-nostdinc` against the compiler's own headers, and `board.cc` cannot: libstdc++'s `<string_view>` reaches `<cwchar>`, which includes `<wchar.h>`. So the C header keeps the stronger promise. Both are linked with `-nostdlib`, which is where the allocator check happens.
