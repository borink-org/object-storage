// What a C++ program sees of this crate without an allocator.
//
// This test opens no socket and allocates nothing. It includes
// `borink/object_storage.hpp` alone, so the helpers there stay usable by a
// program that has no heap to give them. `borink/object_storage/vector.hpp`
// is the file that may include an owning container, and `hosts/cxx-curl` is
// what compiles it.

#include <array>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <new>
#include <span>
#include <string_view>

#include "borink/object_storage.hpp"

// Every allocation this program makes is one the header made, because it makes
// none of its own. Replacing the operators turns one into a failure.
namespace {

[[noreturn]] void allocated() {
    std::fprintf(stderr, "%s: the header allocated\n", __FILE__);
    std::abort();
}

} // namespace

void *operator new(std::size_t) { allocated(); }
void *operator new[](std::size_t) { allocated(); }
void *operator new(std::size_t, std::align_val_t) { allocated(); }
void *operator new[](std::size_t, std::align_val_t) { allocated(); }
void operator delete(void *) noexcept {}
void operator delete[](void *) noexcept {}
void operator delete(void *, std::size_t) noexcept {}
void operator delete[](void *, std::size_t) noexcept {}
void operator delete(void *, std::align_val_t) noexcept {}
void operator delete[](void *, std::align_val_t) noexcept {}

namespace {

int failures = 0;

void check(bool held, const char *source, int line) {
    if (!held) {
        std::fprintf(stderr, "%s:%d: %s\n", __FILE__, line, source);
        failures += 1;
    }
}

#define CHECK(condition) check((condition), #condition, __LINE__)

// A session names its three values, and an empty endpoint is not one Azure has.
void reports_what_is_wrong_with_a_session() {
    const borink::Session good = borink::session("https://account.blob.core.windows.net",
                                                 "container", "token");
    CHECK(borink_validate(&good).code == borink::ErrorCodeNone);

    const borink::Session bad = borink::session("", "container", "token");
    CHECK(borink_validate(&bad).code == borink::ErrorCodeInvalidEndpoint);
}

// A room the sentence fits in returns the whole sentence.
void writes_a_sentence_into_a_room_that_fits() {
    const borink::Session bad = borink::session("", "container", "token");
    std::array<std::uint8_t, 256> room{};

    const borink::Sentence said = borink::describe_into(room, borink_validate(&bad));

    CHECK(said.complete());
    CHECK(said.needed == said.text.size());
    CHECK(!said.text.empty());
}

// A room too small for it returns what fitted, and what a whole one takes.
void reports_the_room_a_whole_sentence_takes() {
    const borink::Session bad = borink::session("", "container", "token");
    std::array<std::uint8_t, 8> room{};

    const borink::Sentence said = borink::describe_into(room, borink_validate(&bad));

    CHECK(!said.complete());
    CHECK(said.text.size() == room.size());
    CHECK(said.needed > room.size());
}

// A read names its range, its precondition and whether it wants the bytes.
void reads_the_shapes_the_helpers_build() {
    CHECK(borink::whole().form == borink::RangeFormWhole);
    CHECK(borink::bounded(2, 6).start == 2);
    CHECK(borink::bounded(2, 6).end == 6);
    CHECK(borink::from(2).form == borink::RangeFormOffset);

    const borink::Read read;
    CHECK(read.shape().kind == borink::GetKindBytes);
    CHECK(read.shape().condition == borink::ConditionNone);
    CHECK(read.shape().range.form == borink::RangeFormWhole);

    const borink::Write write;
    CHECK(write.shape().condition == borink::ConditionNone);

    const borink::Removal removal;
    CHECK(removal.shape().kind == borink::DeleteKindObject);
}

// Bytes go in as text or as a span, and a buffer goes in as a span.
void borrows_what_the_program_owns() {
    CHECK(borink::as_bytes("key").len == 3);

    const std::array<std::uint8_t, 4> owned{1, 2, 3, 4};
    CHECK(borink::borrow(owned).len == 4);
    CHECK(borink::borrow(owned).ptr == owned.data());

    std::array<std::uint8_t, 4> writable{};
    CHECK(borink::into(writable).len == 4);
    CHECK(borink::into(writable).ptr == writable.data());

    // An empty buffer is safe to hand over.
    CHECK(borink::into(std::span<std::uint8_t>()).ptr == nullptr);
}

} // namespace

int main() {
    reports_what_is_wrong_with_a_session();
    writes_a_sentence_into_a_room_that_fits();
    reports_the_room_a_whole_sentence_takes();
    reads_the_shapes_the_helpers_build();
    borrows_what_the_program_owns();

    if (failures != 0) {
        std::fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    return 0;
}
