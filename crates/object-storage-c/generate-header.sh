#!/usr/bin/env bash
# Writes include/borink/object_storage.h from src/lib.rs.
#
# cbindgen is a tool rather than a build-dependency of this crate: a build that
# generated the header would write outside OUT_DIR, which the Cargo Book
# forbids, and every consumer would compile cbindgen's 31 crates to reproduce a
# file that is already committed. Run this after changing the ABI, and commit
# what it writes. CI runs it and fails on a diff.
#
#     nix develop --command crates/object-storage-c/generate-header.sh
#
# `flake.nix` pins the cbindgen version. Without nix, install the same one:
# `cargo install cbindgen --version 0.29.4`. Point releases differ in
# whitespace, so a different one will fail CI's diff for no real reason.

set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

cbindgen --quiet \
    --config "$here/cbindgen.toml" \
    --crate borink-object-storage-c \
    --output "$here/include/borink/object_storage.h" \
    "$here"
