// The C header on a board with no C standard library at all.
//
// This program is never run. It is compiled for a Cortex-M7 with only the
// headers the compiler itself provides, and linked with `-nostdlib`, so
// nothing but the compiler's own intrinsics is available: no newlib, no heap.
// A header that named a hosted-only header would fail to compile here, and an
// archive that called an allocator would fail to link. See
// `freestanding.sh`, which is what compiles and links it.
//
// The three memory functions below are the only ones the archive asks for. A
// real board writes them or takes them from its libc; this file writes them so
// the link has nothing else in it.

#include "borink/object_storage.h"

// Where each result goes, so that nothing here is optimized away.
static volatile unsigned sink;

void *memcpy(void *to, const void *from, size_t count) {
    unsigned char *target = to;
    const unsigned char *source = from;
    while (count--) {
        *target++ = *source++;
    }
    return to;
}

void *memset(void *to, int byte, size_t count) {
    unsigned char *target = to;
    while (count--) {
        *target++ = (unsigned char)byte;
    }
    return to;
}

int memcmp(const void *left, const void *right, size_t count) {
    const unsigned char *one = left;
    const unsigned char *other = right;
    for (; count--; one++, other++) {
        if (*one != *other) {
            return *one < *other ? -1 : 1;
        }
    }
    return 0;
}

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
