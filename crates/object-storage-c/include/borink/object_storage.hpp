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

// The C names, spelled the way C++ spells them.
//
// C has no namespaces, so every generated name carries a `borink_` prefix.
// These aliases put the same types and constants in `borink`, so a C++ caller
// writes `borink::HeaderRef` rather than `borink_header_ref`. They rename
// nothing: each one is the declaration in `borink/object_storage.h` under a
// second name, and the two are the same type to the compiler.
//
// A type or a variant added to the ABI belongs here as well as in
// `cbindgen.toml`'s `[export.rename]` table. Both lists are written by hand.

using Bytes         = borink_bytes;
using BytesMut      = borink_bytes_mut;
using Span          = borink_span;
using MaybeBytes    = borink_maybe_bytes;
using MaybeU64      = borink_maybe_u64;
using Status        = borink_status;
using Session       = borink_session;
using Range         = borink_range;
using GetShape      = borink_get_shape;
using PutShape      = borink_put_shape;
using DeleteShape   = borink_delete_shape;
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

// Every byte of the object.
inline Range whole() { return Range{RangeFormWhole, 0, 0}; }

// The half-open interval `start..end`.
inline Range bounded(std::uint64_t start, std::uint64_t end) {
    return Range{RangeFormBounded, start, end};
}

// Every byte from `start` to the end of the object.
inline Range from(std::uint64_t start) {
    return Range{RangeFormOffset, start, 0};
}

// Reads text as the bytes that a call takes.
inline Bytes as_bytes(std::string_view value) {
    return Bytes{reinterpret_cast<const std::uint8_t *>(value.data()), value.size()};
}

// Reads a span as the bytes that a call takes.
inline Bytes borrow(std::span<const std::uint8_t> items) {
    return Bytes{items.data(), items.size()};
}

// A buffer that a call writes into, safe to hand over when it is empty.
inline BytesMut into(std::vector<std::uint8_t> &bytes) {
    return BytesMut{bytes.empty() ? nullptr : bytes.data(), bytes.size()};
}

// One container, and the token that opens it.
//
// The three values stay where you put them. Keep them for as long as you make
// requests through this session.
inline Session session(std::string_view endpoint, std::string_view container,
                              std::string_view token) {
    return Session{as_bytes(endpoint), as_bytes(container), as_bytes(token)};
}

// One read, and everything that decides what it asks for.
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

// One write, and the precondition that it carries.
struct Write {
    Condition condition = ConditionNone;
    std::string_view condition_value;

    PutShape shape() const { return PutShape{condition}; }
};

// One removal, what it takes with it, and the precondition that it carries.
struct Removal {
    DeleteKind kind = DeleteKindObject;
    Condition condition = ConditionNone;
    std::string_view condition_value;

    DeleteShape shape() const { return DeleteShape{kind, condition}; }
};

// The bytes of a value that the response head may not have carried.
//
// The span points into the head that you collected, and is valid for as long
// as that head is. Copy what you keep.
inline std::span<const std::uint8_t> bytes_of(const MaybeBytes &value) {
    if (!value.present) {
        return {};
    }
    return std::span<const std::uint8_t>(value.bytes.ptr, value.bytes.len);
}

// The same bytes as text, for a value that is one.
inline std::string_view text_of(const MaybeBytes &value) {
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
                                      const Outcome &outcome) {
    return sentence(room, [&](BytesMut writable) {
        return borink_describe(&outcome, writable);
    });
}

// What a status says, in the words of the core crate.
inline std::string_view describe_into(std::vector<std::uint8_t> &room, Status status) {
    return sentence(room,
                    [&](BytesMut writable) { return borink_describe_status(status, writable); });
}

} // namespace borink
