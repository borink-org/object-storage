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

// The templates live in RAM here. A board may keep them, and their bytes, in
// flash instead: neither holds a pointer.
static uint8_t read_template[256];
static uint8_t write_template[256];
static uint8_t stage_template[256];
static uint8_t commit_template[256];
static uint8_t list_template[256];

// One of everything the board would call: a container, each template, a
// request of each, the response heads, and a sentence. The clock is a
// constant here, and its dates are formatted once, as a board formats them
// once a second.
void board_main(void) {
    const borink_container container = {as_bytes("https://account.blob.core.windows.net"),
                                        as_bytes("container"), false, {0, 0}};
    sink = borink_container_check(&container).code;
    const borink_bytes token = as_bytes("token");
    static uint8_t request[1024];
    const borink_bytes_mut buffer = {request, sizeof request};
    const borink_timestamps now = borink_timestamps_from_unix(1787400000);

    // A ranged read.
    const borink_get_options get = {BORINK_GET_KIND_BYTES, BORINK_RANGING_RANGED,
                                    BORINK_CONDITION_NONE, BORINK_REVISION_KIND_CURRENT};
    const borink_prepared reads =
        borink_prepare_get(&container, token, BORINK_AZURE_NAMESPACE_UNKNOWN, &get,
                           (borink_bytes_mut){read_template, sizeof read_template});
    const borink_bytes read_bytes = {read_template, reads.required};
    sink = borink_template_check(&reads.template_, read_bytes).code;
    const borink_get_item item = {as_bytes("object.bin"),
                                  {BORINK_RANGE_FORM_BOUNDED, 2, 6},
                                  {BORINK_CONDITION_FORM_NONE, {0, 0}, 0},
                                  {0, 0}};
    const borink_wire_request read =
        borink_encode_get(&reads.template_, read_bytes, &item, &now, buffer);
    sink = (unsigned)read.required;
    borink_bytes fields = {request + read.fields.start, read.fields.len};
    for (borink_header field = borink_next_header(&fields); field.present;
         field = borink_next_header(&fields)) {
        sink = (unsigned)(field.name.len + field.value.len);
    }

    const borink_header_ref headers[] = {
        {as_bytes("ETag"), as_bytes("\"tag\"")},
        {as_bytes("Content-Range"), as_bytes("bytes 2-5/10")},
        {as_bytes("Content-Length"), as_bytes("4")},
    };
    const size_t count = sizeof headers / sizeof headers[0];
    const borink_get_shape shape = borink_get_item_shape(&item, &get);
    const borink_outcome outcome = borink_accept_get_head(shape, 206, headers, count);
    sink = outcome.kind;
    sink = borink_accept_get_error_body(shape, outcome.failure, as_bytes("")).kind;

    static uint8_t sentence[256];
    sink = (unsigned)borink_describe(&outcome, (borink_bytes_mut){sentence, sizeof sentence});
    sink = (unsigned)borink_describe_status(reads.status,
                                            (borink_bytes_mut){sentence, sizeof sentence});
    sink = borink_azure_rejection_for(reads.status, BORINK_AZURE_NAMESPACE_FLAT).status;

    // A whole-object write.
    const borink_put_options put = {BORINK_CONDITION_IF_NONE_MATCH,
                                    {{{0, 0}, {0, 0}, {0, 0}, {0, 0}, {0, 0}}, {0, 0}},
                                    BORINK_CHECKSUM_KIND_NONE};
    const borink_prepared writes =
        borink_prepare_put(&container, token, BORINK_AZURE_NAMESPACE_UNKNOWN, &put,
                           (borink_bytes_mut){write_template, sizeof write_template});
    const borink_put_item content = {
        as_bytes("object.bin"), 4, {BORINK_CONDITION_FORM_E_TAG, as_bytes("*"), 0}, {0, 0}};
    sink = (unsigned)borink_encode_put(&writes.template_,
                                       (borink_bytes){write_template, writes.required}, &content,
                                       &now, buffer)
               .required;
    const borink_put_shape put_shape = borink_put_item_shape(&content, &put);
    const borink_outcome stored = borink_accept_put_head(put_shape, 201, headers, count);
    sink = stored.kind + borink_accept_put_error_body(put_shape, stored.failure, as_bytes("")).kind;

    // A write in blocks.
    const borink_stage_options stage = {BORINK_CHECKSUM_KIND_NONE};
    const borink_prepared stages =
        borink_prepare_stage_block(&container, token, BORINK_AZURE_NAMESPACE_UNKNOWN, &stage,
                                   (borink_bytes_mut){stage_template, sizeof stage_template});
    const borink_stage_block_item block = {as_bytes("object.bin"), as_bytes("YmxvY2s="), 4,
                                           {0, 0}};
    sink = (unsigned)borink_encode_stage_block(&stages.template_,
                                               (borink_bytes){stage_template, stages.required},
                                               &block, &now, buffer)
               .required;
    const borink_outcome staged = borink_accept_stage_block_head(201, headers, count);
    sink = staged.kind + borink_accept_stage_block_error_body(staged.failure, as_bytes("")).kind;

    const borink_commit_options commit = {
        BORINK_CONDITION_NONE, {{{0, 0}, {0, 0}, {0, 0}, {0, 0}, {0, 0}}, {0, 0}}, false};
    const borink_prepared commits =
        borink_prepare_commit_blocks(&container, token, BORINK_AZURE_NAMESPACE_UNKNOWN, &commit,
                                     (borink_bytes_mut){commit_template, sizeof commit_template});
    const borink_block_ref blocks[] = {{as_bytes("YmxvY2s="), BORINK_BLOCK_SOURCE_LATEST}};
    const borink_commit_item committing = {
        as_bytes("object.bin"), {BORINK_CONDITION_FORM_NONE, {0, 0}, 0}, blocks, 1, {0, 0}};
    sink = (unsigned)borink_encode_commit_blocks(&commits.template_,
                                                 (borink_bytes){commit_template, commits.required},
                                                 &committing, &now, buffer)
               .body.span.len;
    const borink_commit_shape commit_shape = borink_commit_item_shape(&committing, &commit);
    const borink_outcome committed =
        borink_accept_commit_blocks_head(commit_shape, 201, headers, count);
    sink = committed.kind +
           borink_accept_commit_blocks_error_body(commit_shape, committed.failure, as_bytes(""))
               .kind;

    // A listing: the request, the head, and the page read out of a body.
    const borink_list_options listing = {as_bytes("/"), {true, 2}, BORINK_LIST_INCLUDE_METADATA};
    const borink_prepared pages =
        borink_prepare_list(&container, token, BORINK_AZURE_NAMESPACE_UNKNOWN, &listing,
                            (borink_bytes_mut){list_template, sizeof list_template});
    const borink_list_item page_item = {as_bytes("directory/"), {0, 0}};
    sink = (unsigned)borink_encode_list(&pages.template_,
                                        (borink_bytes){list_template, pages.required}, &page_item,
                                        &now, buffer)
               .required;
    const borink_outcome listed = borink_accept_list_head(200, headers, count);
    sink = listed.kind + borink_accept_list_error_body(listed.failure, as_bytes("")).kind;

    static char page[] = "<EnumerationResults><Blobs><Blob><Name>a.txt</Name><Properties>"
                         "<Content-Length>4</Content-Length></Properties></Blob></Blobs>"
                         "<NextMarker>next</NextMarker></EnumerationResults>";
    static borink_list_entry entries[1];
    const borink_bytes_mut body = {(uint8_t *)page, sizeof page - 1};
    const borink_fill fill = borink_fill_listing(body, entries, 1);
    sink = (unsigned)(fill.filled + entries[0].key.len + fill.next_marker.bytes.len);

    // What a listing lends back, read by the two calls that read it.
    static uint8_t quoted[64];
    sink = (unsigned)borink_quoted_etag(entries[0].e_tag.bytes,
                                        (borink_bytes_mut){quoted, sizeof quoted})
               .bytes.len;
    sink = (unsigned)borink_http_date_ms(entries[0].last_modified.bytes).value;

    // A value that no field of the entry carries, by name and by walk, and one
    // decoded into the board's own memory.
    sink = (unsigned)borink_entry_property(&entries[0], as_bytes("Content-Length")).bytes.len;
    borink_properties walk = borink_entry_properties(&entries[0]);
    for (borink_property found = borink_next_property(&walk); found.present;
         found = borink_next_property(&walk)) {
        sink = (unsigned)(found.name.len + found.value.len);
    }
    sink = (unsigned)borink_decode_into(as_bytes("a&amp;b"),
                                        (borink_bytes_mut){quoted, sizeof quoted})
               .bytes.len;

    // The same page read again, keeping two properties of the entry in the
    // board's own row of values.
    static char page_again[] = "<EnumerationResults><Blobs><Blob><Name>a.txt</Name><Properties>"
                               "<Content-Length>4</Content-Length><AccessTier>Hot</AccessTier>"
                               "</Properties></Blob></Blobs><NextMarker /></EnumerationResults>";
    borink_property_set wanted = {0};
    wanted = borink_property_set_with(wanted, BORINK_BLOB_PROPERTY_ACCESS_TIER);
    wanted = borink_property_set_with(wanted, BORINK_BLOB_PROPERTY_CREATION_TIME);
    static borink_maybe_bytes values[2];
    const borink_fill again = borink_fill_listing_with(
        (borink_bytes_mut){(uint8_t *)page_again, sizeof page_again - 1}, entries, 1,
        wanted, values, borink_property_set_len(wanted));
    sink = (unsigned)(again.filled +
                      values[borink_property_slot(wanted, BORINK_BLOB_PROPERTY_ACCESS_TIER)]
                          .bytes.len +
                      borink_property_name(BORINK_BLOB_PROPERTY_ACCESS_TIER).len);

    board_cxx();

    // A board's entry point never returns.
    for (;;) {
    }
}
