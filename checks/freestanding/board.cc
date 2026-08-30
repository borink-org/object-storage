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
    const borink::Session session =
        borink::session("https://account.blob.core.windows.net", "container", "token");
    sink = borink_validate(&session).code;

    // The three ranges, and the three shapes that carry them.
    sink = borink::whole().form + borink::bounded(2, 6).form + borink::from(2).form;

    const borink::Read read{borink::GetKindBytes, borink::bounded(2, 6), borink::ConditionNone,
                            {}};
    const borink::GetShape shape = read.shape();
    const borink::Write write{borink::ConditionIfMatch, "\"tag\""};
    const borink::Removal removal{borink::DeleteKindObject, borink::ConditionNone, {}};
    sink = shape.kind + write.shape().condition + removal.shape().kind;

    // Bytes in, as text and as a span, and a buffer out.
    static std::uint8_t request[1024];
    const std::span<std::uint8_t> writable(request, sizeof request);
    sink = static_cast<unsigned>(borink::as_bytes("object.bin").len +
                                 borink::borrow(std::span<const std::uint8_t>(request, 4)).len +
                                 borink::into(writable).len);

    const borink::RequestHead head =
        borink_encode_get(&session, &shape, borink::as_bytes("object.bin"),
                          borink::Bytes{nullptr, 0}, borink::into(writable), 1787400000);
    sink = static_cast<unsigned>(head.required);

    const borink::HeaderRef headers[] = {
        {borink::as_bytes("ETag"), borink::as_bytes("\"tag\"")},
        {borink::as_bytes("Content-Range"), borink::as_bytes("bytes 2-5/10")},
        {borink::as_bytes("Content-Length"), borink::as_bytes("4")},
    };
    const borink::Outcome outcome =
        borink_accept_get_head(&session, &shape, 206, headers, sizeof headers / sizeof headers[0]);
    sink = outcome.kind;

    // What the head may not have carried, as bytes and as text.
    sink = static_cast<unsigned>(borink::bytes_of(outcome.meta.e_tag).size() +
                                 borink::text_of(outcome.meta.e_tag).size());

    // Both sentences, and the room a whole one takes.
    static std::uint8_t room[256];
    const std::span<std::uint8_t> paper(room, sizeof room);
    const borink::Sentence said = borink::describe_into(paper, outcome);
    const borink::Sentence status = borink::describe_into(paper, borink_validate(&session));
    sink = static_cast<unsigned>(said.needed + said.text.size() + said.complete() +
                                 status.needed + status.complete());
}
