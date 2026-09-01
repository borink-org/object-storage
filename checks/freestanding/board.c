// The C header on a board with no C standard library at all.
//
// This program is never run. It is compiled for a Cortex-M7 against the
// compiler's own headers, and linked with `-nostdlib`. A hosted-only include
// then fails to compile, and an archive that called an allocator fails to
// link. `freestanding.sh` compiles and links it.
//
// `board_main` calls `board_cxx` in `board.cc`, which does the same for the
// C++ header. The linker starts at `board_main` and discards what nothing
// reaches, so a call here is what puts that code in the image.
//
// Define no `memcpy`, `memset` or `memcmp` here. The archive carries weak
// definitions of those and of the `__aeabi_mem*` wrappers. A definition here
// would override them and stop this program from checking that they are
// there.

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

// What the board would call from C, and then from C++.
void board_cxx(void);

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
    sink = outcome.kind;

    static uint8_t sentence[256];
    sink = (unsigned)borink_describe(&outcome, (borink_bytes_mut){sentence, sizeof sentence});

    // A listing: the request, the head, and the page read out of a body.
    const borink_list_shape listing = {true, {true, 2}};
    const borink_request_head page_head =
        borink_encode_list(&session, &listing, as_bytes("directory/"), nothing,
                           (borink_bytes_mut){request, sizeof request}, 1787400000);
    sink = (unsigned)page_head.required;
    sink = borink_accept_list_head(&session, 200, headers,
                                   sizeof headers / sizeof headers[0]).kind;

    static char page[] = "<EnumerationResults><Blobs><Blob><Name>a.txt</Name><Properties>"
                         "<Content-Length>4</Content-Length></Properties></Blob></Blobs>"
                         "<NextMarker>next</NextMarker></EnumerationResults>";
    static borink_list_entry entries[1];
    const borink_bytes_mut body = {(uint8_t *)page, sizeof page - 1};
    borink_fill fill = borink_fill_listing(&session, body, entries, 1);
    fill = borink_resume_listing(&session, body, &fill.resume, entries, 1);
    sink = (unsigned)(fill.filled + entries[0].key.len + fill.next_marker.bytes.len);

    board_cxx();

    // A board's entry point never returns.
    for (;;) {
    }
}
