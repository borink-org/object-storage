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
#   2. The image is the size a board would flash. `-ffunction-sections`,
#      `-fdata-sections` and `--gc-sections` are what every embedded build
#      uses, and they matter enormously here: without them the linker keeps
#      every object of the archive, which is 256 KB rather than 51 KB.
#   3. Nothing in the archive calls an allocator. `-nostdlib` leaves out
#      newlib, so no malloc exists to link against; the image is then checked
#      for an undefined symbol and for an allocator that reached it anyway.
#      The archive answers for its own `memcpy`, `memset`, `memcmp`, `memmove`
#      and `bcmp` and for the `__aeabi_mem*` wrappers, all weakly, so the
#      program supplies none of them and a board would not have to either.
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
    -Os -ffunction-sections -fdata-sections -Wall -Wextra -Wpedantic -Werror \
    "${core[@]}" -I "$here/../include" -c "$here/freestanding.c" -o "$work/freestanding.o"

# libgcc is the compiler's own arithmetic, not a library the board must have.
# This archive turns out to carry its own, so the link succeeds without it too;
# it stays on the line because that is what a board's own build does.
arm-none-eabi-gcc -nostdlib -nostartfiles -Wl,-e,board_main -Wl,-Ttext=0x08000000 \
    -Wl,-z,noexecstack -Wl,--no-warn-execstack -Wl,--gc-sections "${core[@]}" \
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

# What a board would flash: code and constants go to flash, .bss to RAM. The
# 1284 bytes of .bss here are this program's two static buffers, not the
# library's — it keeps no state of its own between calls.
# `size -A` names each section. The Berkeley columns would not: their "text"
# already folds .rodata in, and their "data" is .data alone.
section() {
    arm-none-eabi-size -A "$work/freestanding.elf" | awk -v want="$1" '$1 == want { print $2 + 0; found = 1 }
        END { if (!found) print 0 }'
}
text=$(section .text)
rodata=$(section .rodata)
data=$(section .data)
bss=$(section .bss)

echo "linked with no C library and no allocator"
printf 'flash %d bytes (.text %d, .rodata %d, .data %d), RAM %d bytes (.data %d, .bss %d)\n' \
    "$((text + rodata + data))" "$text" "$rodata" "$data" "$((data + bss))" "$data" "$bss"

# A number for a human to read, when there is a job summary to write it to.
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "### Board image"
        echo
        echo "| section | bytes |"
        echo "|---|---|"
        echo "| flash (.text + .rodata + .data) | $((text + rodata + data)) |"
        echo "| .text | $text |"
        echo "| .rodata | $rodata |"
        echo "| RAM (.data + .bss) | $((data + bss)) |"
    } >> "$GITHUB_STEP_SUMMARY"
fi
