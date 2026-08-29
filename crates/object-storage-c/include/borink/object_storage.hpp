// The C++ side of the boundary: what a request asks for, and how to read what
// the answer said.
//
// Include this and link `borink::object_storage`. Everything here is an inline
// helper over the declarations in `borink/object_storage.h`: it holds no
// state, opens no socket, allocates only where it is said to, and needs no C++
// runtime library. The HTTP client, the buffers and the clock stay in your
// program — see `hosts/cxx-curl` for one written against libcurl.
//
// A C program includes `borink/object_storage.h` alone and needs none of this.

#pragma once

#include <cstddef>
#include <cstdint>
#include <span>
#include <string_view>
#include <vector>

#include "borink/object_storage.h"

namespace borink {

// Every byte of the object.
inline borink_range whole() { return borink_range{BORINK_RANGE_FORM_WHOLE, 0, 0}; }

// The half-open interval `start..end`.
inline borink_range bounded(std::uint64_t start, std::uint64_t end) {
    return borink_range{BORINK_RANGE_FORM_BOUNDED, start, end};
}

// Every byte from `start` to the end of the object.
inline borink_range from(std::uint64_t start) {
    return borink_range{BORINK_RANGE_FORM_OFFSET, start, 0};
}

// Reads text as the bytes that a call takes.
inline borink_bytes as_bytes(std::string_view value) {
    return borink_bytes{reinterpret_cast<const std::uint8_t *>(value.data()), value.size()};
}

// Reads a span as the bytes that a call takes.
inline borink_bytes borrow(std::span<const std::uint8_t> items) {
    return borink_bytes{items.data(), items.size()};
}

// A buffer that a call writes into, safe to hand over when it is empty.
inline borink_bytes_mut into(std::vector<std::uint8_t> &bytes) {
    return borink_bytes_mut{bytes.empty() ? nullptr : bytes.data(), bytes.size()};
}

// One container, and the token that opens it.
//
// The three values stay where you put them. Keep them for as long as you make
// requests through this session.
inline borink_session session(std::string_view endpoint, std::string_view container,
                              std::string_view token) {
    return borink_session{as_bytes(endpoint), as_bytes(container), as_bytes(token)};
}

// One read, and everything that decides what it asks for.
struct Read {
    // Whether the read asks for bytes or for metadata.
    borink_get_kind kind = BORINK_GET_KIND_BYTES;
    // The byte range that the read requests.
    borink_range range = whole();
    // The precondition that the read carries.
    borink_condition condition = BORINK_CONDITION_NONE;
    // The entity tag that the precondition compares against.
    std::string_view condition_value;

    borink_get_shape shape() const { return borink_get_shape{kind, range, condition}; }
};

// One write, and the precondition that it carries.
struct Write {
    borink_condition condition = BORINK_CONDITION_NONE;
    std::string_view condition_value;

    borink_put_shape shape() const { return borink_put_shape{condition}; }
};

// One removal, what it takes with it, and the precondition that it carries.
struct Removal {
    borink_delete_kind kind = BORINK_DELETE_KIND_OBJECT;
    borink_condition condition = BORINK_CONDITION_NONE;
    std::string_view condition_value;

    borink_delete_shape shape() const { return borink_delete_shape{kind, condition}; }
};

// The bytes of a value that the response head may not have carried.
//
// The span points into the head that you collected, and is valid for as long
// as that head is. Copy what you keep.
inline std::span<const std::uint8_t> bytes_of(const borink_maybe_bytes &value) {
    if (!value.present) {
        return {};
    }
    return std::span<const std::uint8_t>(value.bytes.ptr, value.bytes.len);
}

// The same bytes as text, for a value that is one.
inline std::string_view text_of(const borink_maybe_bytes &value) {
    const std::span<const std::uint8_t> bytes = bytes_of(value);
    return std::string_view(reinterpret_cast<const char *>(bytes.data()), bytes.size());
}

// Writes a sentence into `room`, and returns it as text.
//
// The library writes no message of its own accord and never allocates: it
// fills the room it is given and reports what the whole sentence needed. This
// grows `room` to that size and asks once more, so the text is complete, and
// it points into `room` until the next call that writes there.
template <typename Describe>
std::string_view sentence(std::vector<std::uint8_t> &room, Describe describe) {
    std::size_t length = describe(into(room));
    if (length > room.size()) {
        room.resize(length);
        length = describe(into(room));
    }
    return std::string_view(reinterpret_cast<const char *>(room.data()),
                            length < room.size() ? length : room.size());
}

// What an outcome says, in the words of the core crate.
inline std::string_view describe_into(std::vector<std::uint8_t> &room,
                                      const borink_outcome &outcome) {
    return sentence(room, [&](borink_bytes_mut writable) {
        return borink_describe(&outcome, writable);
    });
}

// What a status says, in the words of the core crate.
inline std::string_view describe_into(std::vector<std::uint8_t> &room, borink_status status) {
    return sentence(room,
                    [&](borink_bytes_mut writable) { return borink_describe_status(status, writable); });
}

} // namespace borink
