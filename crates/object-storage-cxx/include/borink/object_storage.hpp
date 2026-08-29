// The C++ side of the bridge: what a request asks for, and how to read what
// the answer said.
//
// Include this and link `borink::object_storage`. Everything here is a
// helper over the generated declarations in `borink-object-storage-cxx/src/lib.rs.h`:
// it holds no state, opens no socket and allocates only where it is said to.
// The HTTP client, the buffers and the clock stay in your program — see
// `hosts/cxx-curl` for one written against libcurl.

#pragma once

#include <algorithm>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <span>
#include <string_view>
#include <vector>

#include "borink-object-storage-cxx/src/lib.rs.h"

namespace borink {

// Every byte of the object.
inline RangeView whole() { return RangeView{RangeFormView::Whole, 0, 0}; }

// The half-open interval `start..end`.
inline RangeView bounded(std::uint64_t start, std::uint64_t end) {
    return RangeView{RangeFormView::Bounded, start, end};
}

// Every byte from `start` to the end of the object.
inline RangeView from(std::uint64_t start) { return RangeView{RangeFormView::Offset, start, 0}; }

// One read, and everything that decides what it asks for.
struct Read {
    // Whether the read asks for bytes or for metadata.
    GetKindView kind = GetKindView::Bytes;
    // The byte range that the read requests.
    RangeView range = whole();
    // The precondition that the read carries.
    ConditionView condition = ConditionView::None;
    // The entity tag that the precondition compares against.
    std::string_view condition_value;

    GetShapeView shape() const { return GetShapeView{kind, range, condition}; }
};

// One write, and the precondition that it carries.
struct Write {
    ConditionView condition = ConditionView::None;
    std::string_view condition_value;

    PutShapeView shape() const { return PutShapeView{condition}; }
};

// One removal, what it takes with it, and the precondition that it carries.
struct Removal {
    DeleteKindView kind = DeleteKindView::Object;
    ConditionView condition = ConditionView::None;
    std::string_view condition_value;

    DeleteShapeView shape() const { return DeleteShapeView{kind, condition}; }
};

// The application owns the clock, so it reads the current time itself.
inline std::uint64_t now_unix() {
    const auto since_epoch = std::chrono::system_clock::now().time_since_epoch();
    return static_cast<std::uint64_t>(
        std::chrono::duration_cast<std::chrono::seconds>(since_epoch).count());
}

// Reads a key as the bytes that the bridge takes.
inline rust::Slice<const std::uint8_t> as_bytes(std::string_view value) {
    return value.empty() ? rust::Slice<const std::uint8_t>()
                         : rust::Slice<const std::uint8_t>(
                               reinterpret_cast<const std::uint8_t *>(value.data()), value.size());
}

// A buffer that the bridge writes into, safe to hand over when it is empty.
inline rust::Slice<std::uint8_t> into(std::vector<std::uint8_t> &bytes) {
    return bytes.empty() ? rust::Slice<std::uint8_t>()
                         : rust::Slice<std::uint8_t>(bytes.data(), bytes.size());
}

// A slice that is safe to hand to the bridge when the range is empty.
template <typename T> rust::Slice<const T> borrow(std::span<const T> items) {
    return items.empty() ? rust::Slice<const T>() : rust::Slice<const T>(items.data(), items.size());
}

// The bytes of a value that the response head may not have carried.
//
// The span points into the head that the caller collected, and is valid for as
// long as that head is. Copy what you keep.
inline std::span<const std::uint8_t> bytes_of(const MaybeBytes &value) {
    if (!value.present) {
        return {};
    }
    return std::span<const std::uint8_t>(value.bytes.data(), value.bytes.size());
}

// The same bytes as text, for a value that is one.
inline std::string_view text_of(const MaybeBytes &value) {
    const std::span<const std::uint8_t> bytes = bytes_of(value);
    return std::string_view(reinterpret_cast<const char *>(bytes.data()), bytes.size());
}

// Writes a sentence the bridge composes into `room`, and returns it as text.
//
// The bridge writes no message of its own accord and never allocates: it fills
// the room it is given and reports what the whole sentence needed. This grows
// `room` to that size and asks once more, so the text is complete, and it
// points into `room` until the next call that writes there.
template <typename Describe>
std::string_view sentence(std::vector<std::uint8_t> &room, Describe describe) {
    std::size_t length = describe(into(room));
    if (length > room.size()) {
        room.resize(length);
        length = describe(into(room));
    }
    return std::string_view(reinterpret_cast<const char *>(room.data()),
                            std::min(length, room.size()));
}

// What an outcome says, in the words of the core crate.
inline std::string_view describe_into(std::vector<std::uint8_t> &room, const Outcome &outcome) {
    return sentence(room,
                    [&](rust::Slice<std::uint8_t> writable) { return describe(outcome, writable); });
}

// What a status says, in the words of the core crate.
inline std::string_view describe_into(std::vector<std::uint8_t> &room, Status status) {
    return sentence(room, [&](rust::Slice<std::uint8_t> writable) {
        return describe_status(status, writable);
    });
}

} // namespace borink
