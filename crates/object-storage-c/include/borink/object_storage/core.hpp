// The C++ API with nothing that allocates.
//
// Include this and link `borink::object_storage`. Every declaration here is
// inline. It holds no state, opens no socket, allocates nothing, and needs no
// C++ runtime library. Your program keeps the HTTP client, the buffers and the
// clock. See `hosts/cxx-curl` for one written against libcurl.
//
// `borink/object_storage.hpp` is this file plus the helpers that resize a
// `std::vector`; include this one alone when your program has no allocator.
//
// A C program includes `borink/object_storage.h` alone and needs none of this.

#pragma once

#include <cstddef>
#include <cstdint>
#include <span>
#include <string_view>

#include "borink/object_storage.h"

namespace borink {

// Names every generated type and constant inside `borink`.
//
// `borink::HeaderRef` and `borink_header_ref` are the same type. Write either.
//
// Add a type or a variant here when you add it to `cbindgen.toml`'s
// `[export.rename]` table. Both lists are written by hand.

using Bytes         = borink_bytes;
using BytesMut      = borink_bytes_mut;
using Span          = borink_span;
using MaybeBytes    = borink_maybe_bytes;
using MaybeU64      = borink_maybe_u64;
using MaybeU32      = borink_maybe_u32;
using MaybeSpan     = borink_maybe_span;
using Status        = borink_status;
using Session       = borink_session;
using Range         = borink_range;
using GetShape      = borink_get_shape;
using PutShape      = borink_put_shape;
using DeleteShape   = borink_delete_shape;
using ListShape     = borink_list_shape;
using ListEntry     = borink_list_entry;
using Properties    = borink_properties;
using Property      = borink_property;
using Resume        = borink_resume;
using Fill          = borink_fill;
using RequestHeader = borink_request_header;
using RequestHead   = borink_request_head;
using HeaderRef     = borink_header_ref;
using ObjectMeta    = borink_object_meta;
using BodyWindow    = borink_body_window;
using Failure       = borink_failure;
using Outcome       = borink_outcome;
using Layout        = borink_layout;
using ErrorCode     = borink_error_code;
using Method        = borink_method;
using GetKind       = borink_get_kind;
using RangeForm     = borink_range_form;
using Condition     = borink_condition;
using DeleteKind    = borink_delete_kind;
using FailureClass  = borink_failure_class;
using ServiceError  = borink_service_error;
using OutcomeKind   = borink_outcome_kind;
using EntryKind     = borink_entry_kind;
using FillKind      = borink_fill_kind;

inline constexpr std::size_t MaxHeaders = BORINK_MAX_HEADERS;

inline constexpr ErrorCode ErrorCodeNone             = BORINK_ERROR_CODE_NONE;
inline constexpr ErrorCode ErrorCodeCapacity         = BORINK_ERROR_CODE_CAPACITY;
inline constexpr ErrorCode ErrorCodeInvalidEndpoint  = BORINK_ERROR_CODE_INVALID_ENDPOINT;
inline constexpr ErrorCode ErrorCodeInvalidContainer = BORINK_ERROR_CODE_INVALID_CONTAINER;
inline constexpr ErrorCode ErrorCodeInvalidToken     = BORINK_ERROR_CODE_INVALID_TOKEN;
inline constexpr ErrorCode ErrorCodeInvalidPlan      = BORINK_ERROR_CODE_INVALID_PLAN;
inline constexpr ErrorCode ErrorCodeResponse         = BORINK_ERROR_CODE_RESPONSE;

inline constexpr Method MethodGet    = BORINK_METHOD_GET;
inline constexpr Method MethodHead   = BORINK_METHOD_HEAD;
inline constexpr Method MethodPut    = BORINK_METHOD_PUT;
inline constexpr Method MethodDelete = BORINK_METHOD_DELETE;

inline constexpr GetKind GetKindBytes    = BORINK_GET_KIND_BYTES;
inline constexpr GetKind GetKindMetadata = BORINK_GET_KIND_METADATA;

inline constexpr RangeForm RangeFormWhole   = BORINK_RANGE_FORM_WHOLE;
inline constexpr RangeForm RangeFormBounded = BORINK_RANGE_FORM_BOUNDED;
inline constexpr RangeForm RangeFormOffset  = BORINK_RANGE_FORM_OFFSET;
inline constexpr RangeForm RangeFormSuffix  = BORINK_RANGE_FORM_SUFFIX;

inline constexpr Condition ConditionNone        = BORINK_CONDITION_NONE;
inline constexpr Condition ConditionIfMatch     = BORINK_CONDITION_IF_MATCH;
inline constexpr Condition ConditionIfNoneMatch = BORINK_CONDITION_IF_NONE_MATCH;

inline constexpr DeleteKind DeleteKindObject             = BORINK_DELETE_KIND_OBJECT;
inline constexpr DeleteKind DeleteKindObjectAndSnapshots = BORINK_DELETE_KIND_OBJECT_AND_SNAPSHOTS;
inline constexpr DeleteKind DeleteKindSnapshotsOnly      = BORINK_DELETE_KIND_SNAPSHOTS_ONLY;

inline constexpr FailureClass FailureClassNone      = BORINK_FAILURE_CLASS_NONE;
inline constexpr FailureClass FailureClassAuth      = BORINK_FAILURE_CLASS_AUTH;
inline constexpr FailureClass FailureClassThrottled = BORINK_FAILURE_CLASS_THROTTLED;
inline constexpr FailureClass FailureClassServer    = BORINK_FAILURE_CLASS_SERVER;
inline constexpr FailureClass FailureClassRedirect  = BORINK_FAILURE_CLASS_REDIRECT;
inline constexpr FailureClass FailureClassOther     = BORINK_FAILURE_CLASS_OTHER;

inline constexpr ServiceError ServiceErrorNone                = BORINK_SERVICE_ERROR_NONE;
inline constexpr ServiceError ServiceErrorNotFound            = BORINK_SERVICE_ERROR_NOT_FOUND;
inline constexpr ServiceError ServiceErrorNoSuchContainer     = BORINK_SERVICE_ERROR_NO_SUCH_CONTAINER;
inline constexpr ServiceError ServiceErrorAlreadyExists       = BORINK_SERVICE_ERROR_ALREADY_EXISTS;
inline constexpr ServiceError ServiceErrorPrecondition        = BORINK_SERVICE_ERROR_PRECONDITION;
inline constexpr ServiceError ServiceErrorRangeNotSatisfiable = BORINK_SERVICE_ERROR_RANGE_NOT_SATISFIABLE;
inline constexpr ServiceError ServiceErrorUnauthorized        = BORINK_SERVICE_ERROR_UNAUTHORIZED;
inline constexpr ServiceError ServiceErrorThrottled           = BORINK_SERVICE_ERROR_THROTTLED;
inline constexpr ServiceError ServiceErrorTimeout             = BORINK_SERVICE_ERROR_TIMEOUT;
inline constexpr ServiceError ServiceErrorService             = BORINK_SERVICE_ERROR_SERVICE;

inline constexpr OutcomeKind OutcomeKindDone                = BORINK_OUTCOME_KIND_DONE;
inline constexpr OutcomeKind OutcomeKindBody                = BORINK_OUTCOME_KIND_BODY;
inline constexpr OutcomeKind OutcomeKindComplete            = BORINK_OUTCOME_KIND_COMPLETE;
inline constexpr OutcomeKind OutcomeKindAccepted            = BORINK_OUTCOME_KIND_ACCEPTED;
inline constexpr OutcomeKind OutcomeKindNotFound            = BORINK_OUTCOME_KIND_NOT_FOUND;
inline constexpr OutcomeKind OutcomeKindNotModified         = BORINK_OUTCOME_KIND_NOT_MODIFIED;
inline constexpr OutcomeKind OutcomeKindPreconditionFailed  = BORINK_OUTCOME_KIND_PRECONDITION_FAILED;
inline constexpr OutcomeKind OutcomeKindRangeNotSatisfiable = BORINK_OUTCOME_KIND_RANGE_NOT_SATISFIABLE;
inline constexpr OutcomeKind OutcomeKindNeedErrorBody       = BORINK_OUTCOME_KIND_NEED_ERROR_BODY;
inline constexpr OutcomeKind OutcomeKindServiceFailure      = BORINK_OUTCOME_KIND_SERVICE_FAILURE;
inline constexpr OutcomeKind OutcomeKindUnsupported         = BORINK_OUTCOME_KIND_UNSUPPORTED;
inline constexpr OutcomeKind OutcomeKindInvalid             = BORINK_OUTCOME_KIND_INVALID;
inline constexpr OutcomeKind OutcomeKindPage                = BORINK_OUTCOME_KIND_PAGE;

inline constexpr EntryKind EntryKindObject    = BORINK_ENTRY_KIND_OBJECT;
inline constexpr EntryKind EntryKindPrefix    = BORINK_ENTRY_KIND_PREFIX;
inline constexpr EntryKind EntryKindDirectory = BORINK_ENTRY_KIND_DIRECTORY;

inline constexpr FillKind FillKindPage    = BORINK_FILL_KIND_PAGE;
inline constexpr FillKind FillKindPartial = BORINK_FILL_KIND_PARTIAL;

// Returns a range over every byte of the object.
inline Range whole() { return Range{RangeFormWhole, 0, 0}; }

// Returns the half-open interval `start..end`.
inline Range bounded(std::uint64_t start, std::uint64_t end) {
    return Range{RangeFormBounded, start, end};
}

// Returns every byte from `start` to the end of the object.
inline Range from(std::uint64_t start) {
    return Range{RangeFormOffset, start, 0};
}

// Returns the number of entries that one page of a listing reports.
//
// A default-built `MaybeU32` is absent, and asks for the service's maximum.
// Azure applies that maximum to any larger number too: 6,000 asks for 6,000
// and is answered with 5,000 and a marker.
inline MaybeU32 at_most(std::uint32_t entries) { return MaybeU32{true, entries}; }

// Reads text as the bytes a call takes.
inline Bytes as_bytes(std::string_view value) {
    return Bytes{reinterpret_cast<const std::uint8_t *>(value.data()), value.size()};
}

// Reads a span as the bytes a call takes.
inline Bytes borrow(std::span<const std::uint8_t> items) {
    return Bytes{items.data(), items.size()};
}

// Reads a span as the buffer a call writes into.
//
// An empty span is safe to hand over.
inline BytesMut into(std::span<std::uint8_t> bytes) {
    return BytesMut{bytes.empty() ? nullptr : bytes.data(), bytes.size()};
}

// Returns a session for one container, with the token that opens it.
//
// The three values stay where you put them. Keep them for as long as you make
// requests through this session.
inline Session session(std::string_view endpoint, std::string_view container,
                              std::string_view token) {
    return Session{as_bytes(endpoint), as_bytes(container), as_bytes(token)};
}

// The settings of one read.
//
// The defaults ask for every byte of the object, with no precondition.
struct Read {
    // Whether the read asks for bytes or for metadata.
    GetKind kind = GetKindBytes;
    // The byte range that the read requests.
    Range range = whole();
    // The precondition that the read carries.
    Condition condition = ConditionNone;
    // The entity tag that the precondition compares against.
    std::string_view condition_value;

    GetShape shape() const { return GetShape{kind, range, condition}; }
};

// The settings of one write.
struct Write {
    // The precondition that the write carries.
    Condition condition = ConditionNone;
    // The entity tag that the precondition compares against.
    std::string_view condition_value;

    PutShape shape() const { return PutShape{condition}; }
};

// The settings of one page of a listing.
//
// The defaults ask for every key under the prefix, in pages of the size that
// the service chooses.
struct List {
    // Whether the listing groups the keys at each `/` after the prefix.
    bool delimited = false;
    // The most entries that one page reports.
    MaybeU32 max_results = {};
    // The text that the last page gave for the next one. Empty asks for the
    // first page.
    std::string_view marker;

    ListShape shape() const { return ListShape{delimited, max_results}; }
};

// The settings of one removal.
struct Removal {
    // What the removal takes with it.
    DeleteKind kind = DeleteKindObject;
    // The precondition that the removal carries.
    Condition condition = ConditionNone;
    // The entity tag that the precondition compares against.
    std::string_view condition_value;

    DeleteShape shape() const { return DeleteShape{kind, condition}; }
};

// Returns the bytes of a value, or nothing when the head did not carry it.
//
// The span points into the head that you collected, and is valid for as long
// as that head is. Copy what you keep.
inline std::span<const std::uint8_t> bytes_of(const MaybeBytes &value) {
    if (!value.present) {
        return {};
    }
    return std::span<const std::uint8_t>(value.bytes.ptr, value.bytes.len);
}

// Returns the same bytes as text, for a value that is text.
inline std::string_view text_of(const MaybeBytes &value) {
    const std::span<const std::uint8_t> bytes = bytes_of(value);
    return std::string_view(reinterpret_cast<const char *>(bytes.data()), bytes.size());
}

// Returns the bytes of a value that a call lent back.
//
// The span points into the storage that the call read, and is valid for as
// long as that storage is. Copy what you keep.
inline std::span<const std::uint8_t> bytes_of(const Bytes &value) {
    return std::span<const std::uint8_t>(value.ptr, value.len);
}

// Returns the same bytes as text, such as the key of a listing entry.
inline std::string_view text_of(const Bytes &value) {
    return std::string_view(reinterpret_cast<const char *>(value.ptr), value.len);
}

// Returns an entity tag from a listing in the quoted form that HTTP defines.
//
// A listing writes an entity tag without the quotes that a condition takes.
// This writes the quoted form into `room`, which needs at most two bytes more
// than the tag, and returns it. The text points into `room` until the next
// call that writes there, and is empty when `room` was too small.
inline std::string_view quoted_etag(const MaybeBytes &listed, std::span<std::uint8_t> room) {
    return text_of(borink_quoted_etag(listed.bytes, into(room)));
}

// Returns an HTTP date as milliseconds since the Unix epoch.
//
// Reads the `last_modified` of a listing entry or of object metadata. A value
// that is not an RFC 1123 date is absent.
inline MaybeU64 http_date_ms(const MaybeBytes &value) {
    return borink_http_date_ms(value.bytes);
}

// Returns the part of `entries` that a fill wrote.
inline std::span<const ListEntry> entries_of(std::span<const ListEntry> entries,
                                             const Fill &fill) {
    return entries.subspan(0, fill.filled);
}

// Returns the value that one entry gave for a property.
//
// An entry carries the values every listing reports; this reads one of the
// rest, such as `AccessTier` or `Creation-Time`, out of the entry's own bytes.
// An absent value means the entry wrote no such property.
//
// Reading more than one or two is a walk: see `properties`.
inline MaybeBytes property(const ListEntry &entry, std::string_view name) {
    return borink_entry_property(&entry, as_bytes(name));
}

// Returns a walk over every value that one entry holds.
//
// Step it with `next`, which reports one value per call:
//
//     Properties walk = properties(entry);
//     for (Property found = next(walk); found.present; found = next(walk)) {
//         // text_of(found.name), text_of(found.value)
//     }
inline Properties properties(const ListEntry &entry) { return borink_entry_properties(&entry); }

// Reads the next value of a walk, and steps the walk past it.
inline Property next(Properties &walk) { return borink_next_property(&walk); }

// Returns the text of a listed value with its references resolved.
//
// A value that an entry lends holds what the service wrote, where XML writes
// an `&` as `&amp;`. This writes what those stand for into `room`, which needs
// no more room than the value. The text is empty when `room` is shorter than
// the value, and when the value holds a reference that no listing declares.
inline std::string_view decoded(const Bytes &value, std::span<std::uint8_t> room) {
    return text_of(borink_decode_into(value, into(room)));
}

// What a describe call wrote into a room.
//
// The library writes no message of its own accord. It fills the room it is
// given and reports what a whole sentence takes. Call again with `needed`
// bytes of room to read the rest.
struct Sentence {
    // The part of the sentence that fitted. It points into the room until the
    // next call that writes there.
    std::string_view text;
    // The number of bytes a whole sentence takes.
    std::size_t needed;

    // Returns whether `text` holds the whole sentence.
    bool complete() const { return needed <= text.size(); }
};

// Writes a sentence into `room` and returns what fitted.
template <typename Describe>
Sentence sentence(std::span<std::uint8_t> room, Describe describe) {
    const std::size_t needed = describe(into(room));
    const std::size_t fitted = needed < room.size() ? needed : room.size();
    return Sentence{fitted == 0 ? std::string_view()
                                : std::string_view(reinterpret_cast<const char *>(room.data()),
                                                   fitted),
                    needed};
}

// Writes what an outcome says into `room`.
inline Sentence describe_into(std::span<std::uint8_t> room, const Outcome &outcome) {
    return sentence(room, [&](BytesMut writable) { return borink_describe(&outcome, writable); });
}

// Writes what a status says into `room`.
inline Sentence describe_into(std::span<std::uint8_t> room, Status status) {
    return sentence(room,
                    [&](BytesMut writable) { return borink_describe_status(status, writable); });
}

} // namespace borink
