// The C header on a board with no C standard library at all.
//
// This program is never run. It is compiled for a Cortex-M7 with only the
// headers the compiler itself provides, and linked with `-nostdlib`, so no
// newlib and no heap are available. A header that named a hosted-only header
// would fail to compile here, and an archive that called an allocator would
// fail to link. See `freestanding.sh`, which is what compiles and links it.
//
// This file defines no `memcpy`, `memset` or `memcmp`. The archive carries
// weak definitions of those, of `memmove` and `bcmp`, and of the `__aeabi_mem*`
// wrappers that the ARM C library ABI calls for, so a board supplies none of
// them. Defining them here would override the weak ones and stop this program
// from checking that they are there.

#include "borink/object_storage.h"

// Where each result goes, so that nothing here is optimized away.
static volatile unsigned sink;

static size_t length(const char *text) {
    size_t count = 0;
    while (text[count] != '\0') {
        count += 1;
    }
    return count;
}

static borink_bytes as_bytes(const char *text) {
    return (borink_bytes){(const uint8_t *)text, length(text)};
}

// One of everything the board would call: a session, a request head, a
// response head, and a sentence. The clock is a constant here.
void board_main(void) {
    const borink_session session = {as_bytes("https://account.blob.core.windows.net"),
                                    as_bytes("container"), as_bytes("token")};
    sink = borink_validate(&session).code;

    const borink_get_shape shape = {
        BORINK_GET_KIND_BYTES, {BORINK_RANGE_FORM_BOUNDED, 2, 6}, BORINK_CONDITION_NONE};
    static uint8_t request[1024];
    const borink_bytes nothing = {0, 0};
    const borink_request_head head =
        borink_encode_get(&session, &shape, as_bytes("object.bin"), nothing,
                          (borink_bytes_mut){request, sizeof request}, 1787400000);
    sink = (unsigned)head.required;

    const borink_header_ref headers[] = {
        {as_bytes("ETag"), as_bytes("\"tag\"")},
        {as_bytes("Content-Range"), as_bytes("bytes 2-5/10")},
        {as_bytes("Content-Length"), as_bytes("4")},
    };
    const borink_outcome outcome = borink_accept_get_head(
        &session, &shape, 206, headers, sizeof headers / sizeof headers[0]);
    sink = outcome.disposition;

    static uint8_t sentence[256];
    sink = (unsigned)borink_describe(&outcome, (borink_bytes_mut){sentence, sizeof sentence});

    // A board's entry point never returns.
    for (;;) {
    }
}
