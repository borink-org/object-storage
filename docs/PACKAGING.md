# Packaging the C library

Why `cargo cbuild` and `cargo cinstall` produce every C artifact, and what a
consumer can rely on. The mechanics are in `crates/object-storage-c/Cargo.toml`
and `flake.nix`; this file holds the reasons, so those files do not have to.

## Why cargo-c

Cargo builds a static archive and installs nothing. A C consumer needs three
things: the archive, the header, and something that says where both are. Only
the first comes out of `cargo build`.

We had written the other two by hand. `CMakeLists.txt` declared the link line
as `Threads::Threads ${CMAKE_DL_LIBS} m`, which is a guess. It holds on glibc
Linux and would need maintaining per platform. cargo-c writes rustc's
`--print native-static-libs` output instead, so the link line comes from the
compiler that produced the archive.

The alternative was a second, hand-written path used only by packagers. That
path would rot, because nothing in CI would build through it.

The cost: a consumer needs cargo-c on `PATH`, not just cargo. Under nix that is
one line, since cargo-c is in the nixpkgs revision `flake.nix` already pins.
Without nix it is `cargo install cargo-c`, which compiles cargo as a library
and is slow.

## What the pkg-config file promises

`pkg-config` reads a small text database. Ours declares four things a consumer
depends on.

**Nothing to propagate.** There is no `Requires` and no `Requires.private`. The
Rust dependencies are linked into the archive, and `cbindgen.toml` holds the
public header to `<stdbool.h>`, `<stddef.h>` and `<stdint.h>`. No type from
another package crosses the API. A nix derivation therefore lists this library
in `buildInputs` and needs no `propagatedBuildInputs` — the recurring source of
packaging bugs that pkg-config's model otherwise invites.

**Every path derives from `${prefix}`.** Rewriting that one line moves the
whole install. This is what makes a nix store path work, where there is no
`/usr/lib` to fall back on.

**`Libs` and `Libs.private` carry the same native libraries.** For a
static-only package this is right, not a duplication to work around. There is
no shared object recording those dependencies, so a plain `--libs` that omitted
them would not link. cargo-c does this on purpose; see `only_staticlib` in its
`src/build.rs`.

**`Cflags` names the directory above `borink/`.** The header is included as
`borink/object_storage.h`, so `-I${includedir}` is the correct answer.
cargo-c's default points into the subdirectory, which would only answer
`#include <object_storage.h>`; `strip_include_path_components = 1` corrects it.

## Why the header is committed

`include/borink/object_storage.h` is generated from `src/lib.rs`, and checked
in. `generate-header.sh` rewrites it and CI fails on a diff.

It is not written by a build script. A build script that generated it would
write outside `OUT_DIR`, which the Cargo Book forbids. Every consumer would
also vendor and compile cbindgen's 31 crates to reproduce a file they already
have. cbindgen is therefore a tool the development shell pins, not a
dependency of the crate.

cargo-c can generate a header too, and `generation = false` stops it. Its
cbindgen run adds `*_MAJOR/_MINOR/_PATCH` macros and drops the C23 enum
typedefs ours carries. CI compares the installed header against the committed
one byte for byte.

`cbindgen.toml` names three system headers — `<stdbool.h>`, `<stddef.h>`,
`<stdint.h>` — rather than taking cbindgen's default list. All three are
freestanding, so a board supplies them without a libc. The default list also
names `<stdlib.h>`, which is hosted and declares an allocator this crate never
calls.

The library is a static archive only, so `versioning = false`. There is no
soname, and nothing to promise about ABI stability yet.

## What the bare-metal check proves

`tests/freestanding.sh` compiles `tests/freestanding.c` for a Cortex-M7 and
links it against the archive. The program is never run. Three things only a
bare-metal link can establish:

**The header names no hosted header.** `-nostdinc` with the compiler's own
include directory is the whole C library a conforming freestanding
implementation must provide. Naming `<stdlib.h>` is then a compile error.
`-ffreestanding` alone is not enough: newlib's headers stay on the include path.

**Nothing calls an allocator.** `-nostdlib` leaves out newlib, so no `malloc`
exists to link against. The image is then checked for undefined symbols and for
an allocator that reached it anyway.

**The image is the size a board would flash.** `-ffunction-sections`,
`-fdata-sections` and `--gc-sections` are what an embedded build uses. Without
them the linker keeps every object in the archive: 256 KB rather than 51 KB.

The archive carries weak definitions of `memcpy`, `memset`, `memcmp`,
`memmove`, `bcmp` and the `__aeabi_mem*` wrappers. A board supplies none of
them. `freestanding.c` must not define them either, or it would override the
weak ones and stop testing that they are there.

`-lgcc` stays on the link line although this archive turns out not to need it.
It is the compiler's own arithmetic, not a library a board must have, and a
board's own build passes it.

## Where the toolchains split

The bare-metal build needs a rustc carrying `thumbv7em-none-eabihf`. The
pinned nixpkgs' rustc carries `x86_64-unknown-linux-gnu`, `wasm32-unknown-unknown`
and `wasm32v1-none` and nothing else. So CI keeps `dtolnay/rust-toolchain` for
that target and puts the pinned cargo-c on `PATH` beside it, through the
`packages.cargo-c` flake output. Everything else runs inside `nix develop`.

## The nix derivation

`packages.default` replaces `buildPhase` and `installPhase` rather than
appending to them. Appending — the shape `rav1e` uses in nixpkgs — leaves
`buildRustPackage`'s own `cargo build` in place, and the crate compiles twice.

Two details that are easy to get wrong:

- `--libdir=$out/lib`. cargo-c defaults to a multiarch subdirectory, and
  nixpkgs' setup hook only puts `lib/pkgconfig` on `PKG_CONFIG_PATH`.
- `cargoLock.lockFile` rather than `cargoHash`. Adding a dependency then does
  not also mean chasing a vendor hash.

`checks.pkg-config-consumer` compiles `tests/abi.c` against the built package
using only what pkg-config reported. It is how the claims above are checked
rather than asserted.
