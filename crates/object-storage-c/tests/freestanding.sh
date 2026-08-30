#!/usr/bin/env bash
# Links this crate into a bare-metal image that has no C library at all.
#
# Takes the archive that `cargo build --target thumbv7em-none-eabihf
# --no-default-features` wrote, and checks two things a hosted build cannot:
#
#   1. The header uses only the freestanding headers. `-nostdinc` with the
#      compiler's own include directory is the whole C library a conforming
#      freestanding implementation must provide — stdarg.h, stdbool.h,
#      stddef.h, stdint.h, float.h, limits.h and a few more. Naming <stdlib.h>
#      is then a fatal error rather than a silent dependency on newlib.
#      `-ffreestanding` alone does not do this: newlib's headers stay on the
#      include path, and <stdlib.h> compiles.
#   2. Nothing in the archive calls an allocator. `-nostdlib` leaves out
#      newlib, so no malloc exists to link against; the image is then checked
#      for an undefined symbol and for an allocator that reached it anyway.
#
#     cargo build --locked -p borink-object-storage-c --no-default-features \
#         --target thumbv7em-none-eabihf
#     nix develop --command crates/object-storage-c/tests/freestanding.sh \
#         target/thumbv7em-none-eabihf/debug/libborink_object_storage_c.a

set -euo pipefail

archive=${1:?usage: freestanding.sh <archive>}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The Nucleo-H743ZI2's core.
core=(-mcpu=cortex-m7 -mfpu=fpv5-d16 -mfloat-abi=hard -mthumb)
# The headers the compiler itself provides, and nothing else.
freestanding=$(dirname "$(arm-none-eabi-gcc -print-libgcc-file-name)")/include

arm-none-eabi-gcc -std=c11 -ffreestanding -nostdinc -isystem "$freestanding" \
    -Os -Wall -Wextra -Wpedantic -Werror "${core[@]}" \
    -I "$here/../include" -c "$here/freestanding.c" -o "$work/freestanding.o"

# libgcc is the compiler's own arithmetic, not a library the board must have.
arm-none-eabi-gcc -nostdlib -nostartfiles -Wl,-e,board_main -Wl,-Ttext=0x08000000 \
    -Wl,-z,noexecstack -Wl,--no-warn-execstack "${core[@]}" \
    "$work/freestanding.o" "$archive" -lgcc -o "$work/freestanding.elf"

undefined=$(arm-none-eabi-nm -u "$work/freestanding.elf")
if [ -n "$undefined" ]; then
    echo "the image still needs symbols that no board supplies:" >&2
    echo "$undefined" >&2
    exit 1
fi

allocator=$(arm-none-eabi-nm "$work/freestanding.elf" |
    grep -iE ' (malloc|calloc|realloc|free|_sbrk|_malloc_r|__rust_alloc[a-z_]*)$' || true)
if [ -n "$allocator" ]; then
    echo "an allocator reached the image:" >&2
    echo "$allocator" >&2
    exit 1
fi

echo "linked with no C library and no allocator:"
arm-none-eabi-size "$work/freestanding.elf"
