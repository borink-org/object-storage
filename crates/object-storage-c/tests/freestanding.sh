#!/usr/bin/env bash
# Links this crate into a bare-metal image that has no C library at all.
#
# Takes an archive built for thumbv7em-none-eabihf. Compiles the header
# against the compiler's own headers alone, links with no newlib and no heap,
# and reports the flash and RAM the image takes.
#
#     cargo cbuild --locked -p borink-object-storage-c --no-default-features \
#         --release --target thumbv7em-none-eabihf --library-type staticlib
#     nix develop --command crates/object-storage-c/tests/freestanding.sh \
#         target/thumbv7em-none-eabihf/release/libborink_object_storage_c.a

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

# libgcc is the compiler's own arithmetic. This archive does not need it; a
# board's own build passes it, so this one does too.
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
# .bss here is this program's two static buffers; the library keeps no state
# between calls. `size -A` names each section, which the Berkeley columns do
# not.
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
