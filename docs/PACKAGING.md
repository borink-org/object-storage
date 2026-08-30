Warning: This document is AI-generated and not thoroughly reviewed.

# Packaging the C library

`cargo cinstall --prefix=<dir> -p borink-object-storage-c --library-type staticlib` (cargo-c) installs the static archive, `borink/object_storage.h` and `borink-object-storage-c.pc`. Link with what that pkg-config file reports: it carries the native libraries rustc named, every path in it derives from `${prefix}`, and it has no `Requires`, so a nix derivation lists this package in `buildInputs` alone. `packages.default` in `flake.nix` builds it that way, and `checks.pkg-config-consumer` links a C program against it using only what pkg-config reports.

A CMake consumer can instead add `crates/object-storage-c` with `add_subdirectory` or `FetchContent` and link `borink::object_storage`. That runs `cargo cbuild`, so cargo-c must be on `PATH`; the pinned nixpkgs carries it.

The header is committed. `generate-header.sh` rewrites it with the cbindgen that `flake.nix` pins, and CI fails on a diff. It includes only `<stdbool.h>`, `<stddef.h>` and `<stdint.h>`, and the archive calls no allocator; `tests/freestanding.sh` links it into a Cortex-M7 image with no C library to check both.
