// C++ helpers over the declarations in `borink/object_storage.h`.
//
// Include this and link `borink::object_storage`. It pulls in
// `borink/object_storage/core.hpp` — the whole API, which allocates nothing —
// and adds the helpers that resize a `std::vector`: each one calls again with
// the room the first answer asked for, so the text it returns is whole.
//
// A program with no allocator includes `borink/object_storage/core.hpp`
// alone. A C program includes `borink/object_storage.h` and needs neither.

#pragma once

#include <cstdint>
#include <string_view>
#include <vector>

#include "borink/object_storage/core.hpp"

namespace borink {

// Writes a whole sentence into `room`, growing it to fit.
//
// Calls `describe` once. Resizes `room` and calls `describe` again when the
// sentence did not fit. The text points into `room` until the next call that
// writes there.
template <typename Describe>
std::string_view whole_sentence(std::vector<std::uint8_t> &room, Describe describe) {
    Sentence said = sentence(room, describe);
    if (!said.complete()) {
        room.resize(said.needed);
        said = sentence(room, describe);
    }
    return said.text;
}

// Writes what an outcome says into `room`, growing it to fit.
inline std::string_view describe_whole(std::vector<std::uint8_t> &room,
                                       const Outcome &outcome) {
    return whole_sentence(room,
                          [&](BytesMut writable) { return borink_describe(&outcome, writable); });
}

// Writes what a status says into `room`, growing it to fit.
inline std::string_view describe_whole(std::vector<std::uint8_t> &room, Status status) {
    return whole_sentence(
        room, [&](BytesMut writable) { return borink_describe_status(status, writable); });
}

} // namespace borink
