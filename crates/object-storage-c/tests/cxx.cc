// What a C++ program sees of this crate, checked against what it should say.
//
// This test opens no socket. It checks the helpers in
// `borink/object_storage.hpp`, which is the whole C++ API: the shapes a
// request carries, the values a response head lends back, and both ways of
// reading a sentence the library wrote.
//
// That the helpers in `borink/object_storage/core.hpp` reach no allocator is
// not checked here. `checks/freestanding` links them into a board image and
// reads the symbols, which holds for code this test never runs.

#include <array>
#include <cstdint>
#include <string>
#include <cstdio>
#include <span>
#include <string_view>
#include <vector>

#include "borink/object_storage.hpp"

namespace {

int failures = 0;

void check(bool held, const char *source, int line) {
    if (!held) {
        std::fprintf(stderr, "%s:%d: %s\n", __FILE__, line, source);
        failures += 1;
    }
}

#define CHECK(condition) check((condition), #condition, __LINE__)

borink::Session a_session() {
    return borink::session("https://account.blob.core.windows.net", "container", "token");
}

// A session names its three values, and an empty endpoint is not one Azure has.
void reports_what_is_wrong_with_a_session() {
    const borink::Session good = a_session();
    CHECK(borink_validate(&good).code == borink::ErrorCodeNone);

    const borink::Session bad = borink::session("", "container", "token");
    CHECK(borink_validate(&bad).code == borink::ErrorCodeInvalidEndpoint);
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

// A read names its range, its precondition and whether it wants the bytes.
void reads_the_shapes_the_helpers_build() {
    CHECK(borink::whole().form == borink::RangeFormWhole);
    CHECK(borink::bounded(2, 6).form == borink::RangeFormBounded);
    CHECK(borink::bounded(2, 6).start == 2);
    CHECK(borink::bounded(2, 6).end == 6);
    CHECK(borink::from(2).form == borink::RangeFormOffset);
    CHECK(borink::from(2).start == 2);

    const borink::Read read;
    CHECK(read.shape().kind == borink::GetKindBytes);
    CHECK(read.shape().condition == borink::ConditionNone);
    CHECK(read.shape().range.form == borink::RangeFormWhole);

    const borink::Write write{borink::ConditionIfMatch, "\"tag\""};
    CHECK(write.shape().condition == borink::ConditionIfMatch);

    const borink::Removal removal;
    CHECK(removal.shape().kind == borink::DeleteKindObject);
    CHECK(removal.shape().condition == borink::ConditionNone);
}

// A listing names how it groups the keys and how many it asks for.
void reads_the_shape_a_listing_carries() {
    const borink::List all;
    CHECK(all.shape().delimited == false);
    CHECK(all.shape().max_results.present == false);

    const borink::List page{true, borink::at_most(1000), "marker-1"};
    CHECK(page.shape().delimited == true);
    CHECK(page.shape().max_results.value == 1000);
}

// A page is read out of the caller's own buffer, into the caller's own array.
// The entries point back into that buffer.
void reads_a_page_out_of_a_body() {
    const borink::Session session = a_session();
    const borink::HeaderRef headers[] = {
        {borink::as_bytes("Content-Length"), borink::as_bytes("196")},
    };
    const borink::Outcome outcome =
        borink_accept_list_head(&session, 200, headers, sizeof headers / sizeof headers[0]);
    CHECK(outcome.kind == borink::OutcomeKindPage);
    CHECK(outcome.body.expected_len.value == 196);

    // Reading decodes the text of the body where it stands, so the page is a
    // buffer of this program rather than a literal.
    std::string body =
        "<EnumerationResults><Blobs>"
        "<Blob><Name>a&amp;b.txt</Name><Properties><Etag>0x1</Etag>"
        "<Content-Length>4</Content-Length></Properties></Blob>"
        "<BlobPrefix><Name>nested/</Name></BlobPrefix>"
        "</Blobs><NextMarker>next</NextMarker></EnumerationResults>";
    const borink::BytesMut page =
        borink::into(std::span<std::uint8_t>(reinterpret_cast<std::uint8_t *>(body.data()),
                                             body.size()));

    std::array<borink::ListEntry, 4> entries{};
    const borink::Fill fill =
        borink_fill_listing(&session, page, entries.data(), entries.size());

    CHECK(fill.status.code == borink::ErrorCodeNone);
    const std::span<const borink::ListEntry> read = borink::entries_of(entries, fill);
    CHECK(read.size() == 2);
    // The escape in the name was decoded inside the body.
    CHECK(borink::text_of(read[0].key) == "a&b.txt");
    CHECK(read[0].kind == borink::EntryKindObject);
    CHECK(read[0].size.value == 4);
    CHECK(borink::text_of(read[0].e_tag) == "0x1");
    // A delimited listing reports the level below as a group, which has no
    // size of its own.
    CHECK(read[1].kind == borink::EntryKindPrefix);
    CHECK(read[1].size.present == false);
    CHECK(borink::text_of(read[1].key) == "nested/");
    CHECK(borink::text_of(fill.next_marker) == "next");
}

// An array smaller than the page is refused, and the fill says how many
// entries the page holds.
void an_array_smaller_than_the_page_is_refused() {
    const borink::Session session = a_session();
    std::string body = "<EnumerationResults><Blobs>"
                       "<Blob><Name>a.txt</Name><Properties>"
                       "<Content-Length>4</Content-Length></Properties></Blob>"
                       "<Blob><Name>b.txt</Name><Properties>"
                       "<Content-Length>8</Content-Length></Properties></Blob>"
                       "</Blobs><NextMarker /></EnumerationResults>";
    const borink::BytesMut page =
        borink::into(std::span<std::uint8_t>(reinterpret_cast<std::uint8_t *>(body.data()),
                                             body.size()));

    std::array<borink::ListEntry, 1> entries{};
    const borink::Fill fill =
        borink_fill_listing(&session, page, entries.data(), entries.size());
    CHECK(fill.status.code == borink::ErrorCodeCapacity);
    CHECK(fill.required == 2);
    CHECK(fill.filled == 0);
}

// An entry's entity tag and date are read by the helpers beside them.
void reads_the_values_an_entry_lends() {
    std::string body = "<EnumerationResults><Blobs>"
                       "<Blob><Name>a.txt</Name><Properties>"
                       "<Last-Modified>Wed, 26 Aug 2026 12:00:00 GMT</Last-Modified>"
                       "<Etag>0x8DF0</Etag>"
                       "<Content-Length>4</Content-Length></Properties></Blob>"
                       "</Blobs><NextMarker /></EnumerationResults>";
    const borink::Session session = a_session();
    const borink::BytesMut page =
        borink::into(std::span<std::uint8_t>(reinterpret_cast<std::uint8_t *>(body.data()),
                                             body.size()));
    std::array<borink::ListEntry, 1> entries{};
    const borink::Fill fill =
        borink_fill_listing(&session, page, entries.data(), entries.size());
    CHECK(fill.filled == 1);

    // The listed tag carries no quotes; a condition takes the quoted form.
    CHECK(borink::text_of(entries[0].e_tag) == "0x8DF0");
    std::array<std::uint8_t, 32> room{};
    CHECK(borink::quoted_etag(entries[0].e_tag, room) == "\"0x8DF0\"");
    // A room too small for the quotes writes nothing.
    CHECK(borink::quoted_etag(entries[0].e_tag, std::span(room).first(6)).empty());

    const borink::MaybeU64 modified = borink::http_date_ms(entries[0].last_modified);
    CHECK(modified.present);
    CHECK(modified.value == 1787745600000ULL);
    // A value the entry did not carry is not a date.
    CHECK(!borink::http_date_ms(borink::MaybeBytes{}).present);
}

// A value that no field of an entry carries is read out of the entry itself.
void reads_a_property_out_of_an_entry() {
    const borink::Session session = a_session();
    std::string body = "<EnumerationResults><Blobs><Blob><Name>a.txt</Name>"
                       "<VersionId>2026-09-01T19:08:11Z</VersionId><Properties>"
                       "<Content-Length>4</Content-Length><AccessTier>Hot</AccessTier>"
                       "</Properties><Metadata><colour>a&amp;b</colour></Metadata>"
                       "</Blob></Blobs><NextMarker /></EnumerationResults>";
    std::array<borink::ListEntry, 1> entries{};
    const borink::Fill fill = borink_fill_listing(
        &session,
        borink::into(std::span<std::uint8_t>(reinterpret_cast<std::uint8_t *>(body.data()),
                                             body.size())),
        entries.data(), entries.size());
    CHECK(fill.filled == 1);

    // Inside the properties element and beside it, by one call.
    CHECK(borink::text_of(borink::property(entries[0], "AccessTier")) == "Hot");
    CHECK(borink::text_of(borink::property(entries[0], "VersionId")) == "2026-09-01T19:08:11Z");
    CHECK(!borink::property(entries[0], "Snapshot").present);

    // The walk reports every value once, in the order the service wrote them.
    std::vector<std::string> names;
    borink::Properties walk = borink::properties(entries[0]);
    for (borink::Property found = borink::next(walk); found.present;
         found = borink::next(walk)) {
        names.push_back(std::string(borink::text_of(found.name)));
    }
    CHECK(names == std::vector<std::string>({"Name", "VersionId", "Content-Length",
                                             "AccessTier", "Metadata"}));
    CHECK(!borink::next(walk).present);

    // An element that holds others reports those bytes, and a reference in
    // them is resolved into a room of the caller's.
    const borink::MaybeBytes metadata = borink::property(entries[0], "Metadata");
    CHECK(borink::text_of(metadata) == "<colour>a&amp;b</colour>");
    std::array<std::uint8_t, 32> room{};
    CHECK(borink::decoded(metadata.bytes, room) == "<colour>a&b</colour>");
}

// A range read reports where the bytes sit, and lends back what the head said.
void reads_the_values_a_head_lent_back() {
    const borink::Session session = a_session();
    const borink::Read read{borink::GetKindBytes, borink::bounded(2, 6), borink::ConditionNone,
                            {}};
    const borink::GetShape shape = read.shape();
    const borink::HeaderRef headers[] = {
        {borink::as_bytes("ETag"), borink::as_bytes("\"tag\"")},
        {borink::as_bytes("Content-Range"), borink::as_bytes("bytes 2-5/10")},
        {borink::as_bytes("Content-Length"), borink::as_bytes("4")},
    };

    const borink::Outcome outcome =
        borink_accept_get_head(&session, &shape, 206, headers, sizeof headers / sizeof headers[0]);

    CHECK(outcome.kind == borink::OutcomeKindBody);
    CHECK(outcome.body.object_offset == 2);
    CHECK(outcome.body.object_size.value == 10);
    CHECK(borink::text_of(outcome.meta.e_tag) == "\"tag\"");
    CHECK(borink::bytes_of(outcome.meta.e_tag).size() == 5);

    // A value the head did not carry reads as empty.
    CHECK(borink::bytes_of(outcome.meta.content_encoding).empty());
    CHECK(borink::text_of(outcome.meta.content_encoding).empty());
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

// The growing helper asks again with the room the first answer wanted.
void grows_a_room_until_the_sentence_is_whole() {
    const borink::Session bad = borink::session("", "container", "token");
    std::array<std::uint8_t, 256> reference{};
    const borink::Sentence whole = borink::describe_into(reference, borink_validate(&bad));

    // Empty, so the first call cannot fit and the second one has to.
    std::vector<std::uint8_t> room;
    const std::string_view said = borink::describe_whole(room, borink_validate(&bad));

    CHECK(said == whole.text);
    CHECK(room.size() == whole.needed);
}

// An outcome says the same thing however the room was found.
void describes_an_outcome_either_way() {
    const borink::Session session = a_session();
    const borink::GetShape shape = borink::Read().shape();
    const borink::HeaderRef headers[] = {
        {borink::as_bytes("Content-Length"), borink::as_bytes("4")},
    };
    const borink::Outcome outcome =
        borink_accept_get_head(&session, &shape, 200, headers, sizeof headers / sizeof headers[0]);

    std::array<std::uint8_t, 256> room{};
    const borink::Sentence said = borink::describe_into(room, outcome);
    CHECK(said.complete());

    std::vector<std::uint8_t> grown;
    CHECK(borink::describe_whole(grown, outcome) == said.text);
}

} // namespace

int main() {
    reports_what_is_wrong_with_a_session();
    borrows_what_the_program_owns();
    reads_the_shapes_the_helpers_build();
    reads_the_shape_a_listing_carries();
    reads_a_page_out_of_a_body();
    an_array_smaller_than_the_page_is_refused();
    reads_the_values_an_entry_lends();
    reads_a_property_out_of_an_entry();
    reads_the_values_a_head_lent_back();
    writes_a_sentence_into_a_room_that_fits();
    reports_the_room_a_whole_sentence_takes();
    grows_a_room_until_the_sentence_is_whole();
    describes_an_outcome_either_way();

    if (failures != 0) {
        std::fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    return 0;
}
