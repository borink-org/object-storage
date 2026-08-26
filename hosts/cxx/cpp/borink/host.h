// What a host does, and the memory that one client gives it.
//
// A host in this directory sends requests with one HTTP client. A program
// links exactly one of them, so `main.cc` is the same program whichever client
// carries its requests.
//
// The bridge allocates nothing per request and throws nothing. This host
// allocates nothing per request either: every buffer it uses belongs to the
// `Client` that the application built, and is reused by the next request. A
// failure is the one exception, because it builds a message.

#pragma once

#include <algorithm>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <functional>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

#include "borink-azure-cxx/src/lib.rs.h"

namespace borink::host {

// The name of the HTTP client that this host sends with.
extern const std::string_view client;

// Where the bytes of an object go as they arrive.
//
// A read never holds the whole object: the host passes each part along and
// keeps none of it, so the application decides what an object costs it.
using Sink = std::function<void(std::span<const std::uint8_t>)>;

// How much memory one client may use.
struct Limits {
    // The most that one request head may take. A request that needs more is
    // refused rather than served.
    std::size_t request_bytes = 8 * 1024;
    // The most of an error body to read. An error body is a diagnostic, and
    // the service decides how long it is: one that does not arrive costs the
    // name of the error, not the outcome.
    std::size_t error_bytes = 8 * 1024;
};

// Every byte of the object.
inline RangeView whole() { return RangeView{RangeForm::Whole, 0, 0}; }

// The half-open interval `start..end`.
inline RangeView bounded(std::uint64_t start, std::uint64_t end) {
    return RangeView{RangeForm::Bounded, start, end};
}

// Every byte from `start` to the end of the object.
inline RangeView from(std::uint64_t start) { return RangeView{RangeForm::Offset, start, 0}; }

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

// The bytes of one response head, and where each header sits in them.
//
// The HTTP client hands over one header at a time and keeps nothing, so this
// is where the head lives. The bridge reads it here, in place.
class CollectedHead {
  public:
    // Starts a new head. A client that reports more than one response, such as
    // a 100 Continue before the answer, calls this for each.
    void restart(std::uint16_t status) {
        status_ = status;
        bytes_.clear();
        fields_.clear();
    }

    void field(std::string_view name, std::string_view value) {
        fields_.push_back(HeaderField{append(name), append(value)});
    }

    std::uint16_t status() const { return status_; }

    rust::Slice<const std::uint8_t> bytes() const {
        return borrow(std::span<const std::uint8_t>(bytes_));
    }

    rust::Slice<const HeaderField> fields() const {
        return borrow(std::span<const HeaderField>(fields_));
    }

    // Reads one range of the head, such as the entity tag that an outcome
    // names. The range is empty if the head did not carry that value.
    std::span<const std::uint8_t> at(const MaybeSpan &range) const {
        if (!range.present) {
            return {};
        }
        return std::span<const std::uint8_t>(bytes_).subspan(range.span.start, range.span.len);
    }

  private:
    Span append(std::string_view value) {
        const Span range{bytes_.size(), value.size()};
        bytes_.insert(bytes_.end(), value.begin(), value.end());
        return range;
    }

    std::uint16_t status_ = 0;
    std::vector<std::uint8_t> bytes_;
    std::vector<HeaderField> fields_;
};

// One session, and the memory that every request through it reuses.
//
// Build one per client and keep it. Its buffers grow to the largest request
// that this client has made, up to `limits`, and stay that size.
class Client {
  public:
    Client(rust::Box<Session> session, Limits limits)
        : session_(std::move(session)), limits_(limits) {}

    // Opens a client against one container.
    //
    // Throws std::runtime_error if the endpoint, the container or the token
    // cannot be used. The message is the sentence that the core crate wrote.
    static Client open(std::string_view endpoint, std::string_view container,
                       std::string_view token, Limits limits = {});

    // Reads the object that `read` describes, passing its stored bytes to
    // `sink` as they arrive.
    //
    // A metadata read returns no bytes and calls `sink` not at all. Read the
    // metadata from `outcome()` and `head()` afterwards.
    //
    // Throws std::runtime_error if Azure returned no object. The message is
    // the sentence that the core crate wrote for the outcome.
    void get(std::string_view key, const Sink &sink, const Read &read = {});

    // Writes `content` as the whole object. The bytes stay where the caller
    // put them, and the host sends them from there.
    void put(std::string_view key, std::span<const std::uint8_t> content,
             const Write &write = {});

    // Removes what `removal` names.
    //
    // Reports a missing object rather than treating it as success: only the
    // caller knows whether it meant to remove an object that is already gone.
    void remove(std::string_view key, const Removal &removal = {});

    const Session &session() const { return *session_; }

    // The head of the response to the last request, for a caller that wants
    // the metadata that came with it.
    const CollectedHead &head() const { return head_; }

    // What the last response said. Its spans name ranges of `head()`.
    const Outcome &outcome() const { return outcome_; }

    // Reads one range of the request buffer, such as the URL or a header.
    std::string_view part(const Span &range) const {
        return {reinterpret_cast<const char *>(request_.data()) + range.start, range.len};
    }

  private:
    // Writes one request head into this client's buffer.
    //
    // `write` calls the bridge, which reports the size that the head needs.
    // The buffer grows to that size once and is reused from then on.
    template <typename Encode> const RequestHead &encode(Encode encode) {
        head_.restart(0);
        diagnostic_.clear();
        outcome_ = Outcome{};
        request_head_ = encode();
        if (request_head_.status.code == static_cast<std::uint16_t>(ErrorCode::Capacity)) {
            if (request_head_.required > limits_.request_bytes) {
                throw std::runtime_error("the request head is larger than this client allows");
            }
            request_.resize(request_head_.required);
            request_head_ = encode();
        }
        if (request_head_.status.code != 0) {
            throw std::runtime_error(std::string(sentence([&](rust::Slice<std::uint8_t> room) {
                return describe_status(request_head_.status, room);
            })));
        }
        return request_head_;
    }

    rust::Slice<std::uint8_t> request_buffer() { return into(request_); }

    // The diagnostic body that this client kept, capped by its limits.
    rust::Slice<const std::uint8_t> kept_body() const {
        return borrow(std::span<const std::uint8_t>(diagnostic_));
    }

    // Writes what the bridge says into this client's message buffer. This is
    // the one place that a failing request allocates.
    template <typename Describe> std::string_view sentence(Describe describe) {
        std::size_t length = describe(into(message_));
        if (length > message_.size()) {
            message_.resize(length);
            length = describe(into(message_));
        }
        return std::string_view(reinterpret_cast<const char *>(message_.data()),
                                std::min(length, message_.size()));
    }

    // Reports an outcome that carries no object, in the words of the core
    // crate.
    [[noreturn]] void fail(std::string_view what) {
        const std::string_view said = sentence([&](rust::Slice<std::uint8_t> room) {
            return borink::describe(outcome_, head_.bytes(), room);
        });
        throw std::runtime_error(std::string(what) + ": " + std::string(said));
    }

    rust::Box<Session> session_;
    Limits limits_;
    std::vector<std::uint8_t> request_;
    RequestHead request_head_{};
    std::vector<std::uint8_t> message_;
    CollectedHead head_;
    std::vector<std::uint8_t> diagnostic_;
    Outcome outcome_{};
};

} // namespace borink::host
