// The C++ header on a board, where nothing can allocate.
//
// This function is never run. It is compiled for a Cortex-M7 and linked with
// `-nostdlib`, so a helper in `borink/object_storage/core.hpp` that reached an
// allocator would leave `operator new` in the image. `freestanding.sh` fails
// on that symbol.
//
// Unlike `board.c` this compiles against the toolchain's C library headers.
// libstdc++ needs them: `<string_view>` reaches `<cwchar>`, which includes
// `<wchar.h>`. So the C header keeps the stronger promise of the two, and this
// one promises what `core.hpp` says it does — no allocator, and no C++ runtime.
//
// Name every helper in that header here. One this file leaves out is one the
// image never carries, and the check would pass without ever seeing it.

#include "borink/object_storage/core.hpp"

// Where each result goes, so that nothing here is optimized away.
static volatile unsigned sink;

extern "C" void board_cxx(void) {
    const borink::Container container =
        borink::container("https://account.blob.core.windows.net", "container");
    sink = borink_container_check(&container).code;

    // The three ranges.
    sink = borink::whole().form + borink::bounded(2, 6).form + borink::from(2).form;

    // Bytes in, as text and as a span, and a buffer out.
    static std::uint8_t request[1024];
    const std::span<std::uint8_t> writable(request, sizeof request);
    sink = static_cast<unsigned>(borink::as_bytes("object.bin").len +
                                 borink::borrow(std::span<const std::uint8_t>(request, 4)).len +
                                 borink::into(writable).len);
    const std::uint64_t seconds = 1787400000;
    const borink::Timestamps now = borink_timestamps_from_unix(seconds);

    // A read of one object, whose template holds its key, and its head.
    static std::uint8_t room[256];
    const std::span<std::uint8_t> template_room(room, sizeof room);
    const borink::Container one = borink::object(container, "object.bin");
    borink::GetOptions options = borink::get_options();
    options.ranging = borink::RangingRanged;
    sink = borink::head_options().kind;
    const borink::Prepared reads = borink_prepare_get(&one, borink::as_bytes("token"),
                                                      borink::AzureNamespaceUnknown,
                                                      &options, borink::into(template_room));
    borink::GetItem item = borink::get_item("");
    item.range = borink::bounded(2, 6);
    item.condition_value = borink::condition_tag("\"tag\"");
    item.condition_value = borink::condition_time(seconds);
    const borink::WireRequest read = borink_encode_get(
        &reads.template_, borink::template_bytes(template_room, reads), &item, &now,
        borink::into(writable));
    sink = static_cast<unsigned>(borink::part(writable, read.url).size());
    borink::Bytes fields = borink::fields_of(writable, read);
    for (borink::Header field = borink::next_header(fields); field.present;
         field = borink::next_header(fields)) {
        sink = static_cast<unsigned>(borink::text_of(field.name).size() + field.value.len);
    }
    const borink::GetShape shape = borink::shape(item, options);

    const borink::HeaderRef headers[] = {
        {borink::as_bytes("ETag"), borink::as_bytes("\"tag\"")},
        {borink::as_bytes("Content-Range"), borink::as_bytes("bytes 2-5/10")},
        {borink::as_bytes("Content-Length"), borink::as_bytes("4")},
    };
    const borink::Outcome outcome =
        borink_accept_get_head(shape, 206, headers, sizeof headers / sizeof headers[0]);
    sink = outcome.kind;

    // What the head may not have carried, as bytes and as text.
    sink = static_cast<unsigned>(borink::bytes_of(outcome.meta.e_tag).size() +
                                 borink::text_of(outcome.meta.e_tag).size());

    // The items and the shapes of a write and of a commit, and a listing's
    // options and item.
    const borink::PutOptions writes = borink::put_options();
    const borink::PutItem write = borink::put_item("object.bin", 4);
    const borink::StageBlockItem stage = borink::stage_block_item("object.bin", "YmxvY2s=", 4);
    const borink::BlockRef blocks[] = {borink::block("YmxvY2s=")};
    const borink::CommitItem commit = borink::commit_item("object.bin", blocks);
    sink = borink::shape(write, writes).condition +
           borink::shape(commit, borink::commit_options()).condition +
           static_cast<unsigned>(stage.content_len);
    const borink::ListOptions listing = borink::list_options();
    const borink::ListItem page_item = borink::list_item("directory/", "marker-1");
    sink = static_cast<unsigned>(listing.include + borink::at_most(2).value +
                                 page_item.marker.len);

    static char page[] = "<EnumerationResults><Blobs><Blob><Name>a.txt</Name><Properties>"
                         "<Content-Length>4</Content-Length></Properties></Blob></Blobs>"
                         "<NextMarker>next</NextMarker></EnumerationResults>";
    static borink::ListEntry entries[1];
    const borink::BytesMut body =
        borink::into(std::span<std::uint8_t>(reinterpret_cast<std::uint8_t *>(page),
                                             sizeof page - 1));
    const borink::Fill fill = borink_fill_listing(body, entries, 1);
    const std::span<const borink::ListEntry> listed = borink::entries_of(entries, fill);
    sink = static_cast<unsigned>(listed.size() + borink::text_of(entries[0].key).size() +
                                 borink::bytes_of(entries[0].key).size() +
                                 borink::text_of(fill.next_marker).size());

    // What a listing lends back, read by the two helpers that read it.
    static std::uint8_t quoted[64];
    sink = static_cast<unsigned>(
        borink::quoted_etag(entries[0].e_tag, std::span<std::uint8_t>(quoted, sizeof quoted))
            .size());
    sink = static_cast<unsigned>(borink::http_date_ms(entries[0].last_modified).value);

    // A value that no field of the entry carries, by name and by walk, and one
    // decoded into the board's own memory.
    sink = static_cast<unsigned>(borink::property(entries[0], "Content-Length").bytes.len);
    borink::Properties walk = borink::properties(entries[0]);
    for (borink::Property found = borink::next(walk); found.present;
         found = borink::next(walk)) {
        sink = static_cast<unsigned>(borink::text_of(found.name).size() + found.value.len);
    }
    sink = static_cast<unsigned>(
        borink::decoded(borink::as_bytes("a&amp;b"),
                        std::span<std::uint8_t>(quoted, sizeof quoted))
            .size());

    // The same page read again, keeping two properties of the entry in the
    // board's own row of values.
    static char page_again[] = "<EnumerationResults><Blobs><Blob><Name>a.txt</Name><Properties>"
                               "<Content-Length>4</Content-Length><AccessTier>Hot</AccessTier>"
                               "</Properties></Blob></Blobs><NextMarker /></EnumerationResults>";
    const borink::PropertySet wanted =
        borink::property_set({borink::BlobPropertyAccessTier, borink::BlobPropertyCreationTime});
    static borink::MaybeBytes values[2];
    const borink::Fill again = borink_fill_listing_with(
        borink::into(std::span<std::uint8_t>(reinterpret_cast<std::uint8_t *>(page_again),
                                             sizeof page_again - 1)),
        entries, 1, wanted, values, sizeof values / sizeof values[0]);
    const std::span<const borink::MaybeBytes> row = borink::values_of(values, wanted, 0);
    sink = static_cast<unsigned>(
        again.filled +
        borink::text_of(borink::value(row, wanted, borink::BlobPropertyAccessTier)).size());

    // Both sentences, and the room a whole one takes.
    static std::uint8_t page_of_text[256];
    const std::span<std::uint8_t> paper(page_of_text, sizeof page_of_text);
    const borink::Sentence said = borink::describe_into(paper, outcome);
    const borink::Sentence status = borink::describe_into(paper, borink_container_check(&container));
    sink = static_cast<unsigned>(said.needed + said.text.size() + said.complete() +
                                 status.needed + status.complete());
}
