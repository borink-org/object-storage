Warning: This document is AI-generated and not thoroughly reviewed.

# Packaging the C library

`cargo cinstall --prefix=<dir> -p borink-object-storage-c --library-type staticlib` (cargo-c) installs the static archive, `borink/object_storage.h`, `borink/object_storage.hpp`, `borink/object_storage/core.hpp` and `borink-object-storage-c.pc`. Link with what that pkg-config file reports: it carries the native libraries rustc named, every path in it derives from `${prefix}`, and it has no `Requires`, so a nix derivation lists this package in `buildInputs` alone. `packages.default` in `flake.nix` builds it that way, and `checks.pkg-config-consumer` links a C program against it using only what pkg-config reports.

A CMake consumer can instead add `crates/object-storage-c` with `add_subdirectory` or `FetchContent` and link `borink::object_storage`. That runs `cargo cbuild`, so cargo-c must be on `PATH`; the pinned nixpkgs carries it.

All three headers are installed and a C consumer includes only the first. The C++ ones are inline helpers over the C declarations, and they are how a C++ consumer of the packaged library uses it; there is no second package to put them in, and unincluded they cost two files. `object_storage.hpp` is what a C++ consumer includes: the whole API plus the convenience that resizes a `std::vector` to what a call asked for. `object_storage/core.hpp` is that API alone — it allocates nothing, so a program with no allocator includes it instead. `hosts/cxx-curl` includes the first.

The header is committed. `generate-header.sh` rewrites it with the cbindgen that `flake.nix` pins, and CI fails on a diff. It includes only `<stdbool.h>`, `<stddef.h>` and `<stdint.h>`, and the archive calls no allocator; `tests/freestanding.sh` links it into a Cortex-M7 image with no C library to check both.
