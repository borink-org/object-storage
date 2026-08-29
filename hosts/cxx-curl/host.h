// What this host does, and the memory that one client gives it.
//
// This is a host: it sends the requests that `borink::object_storage` builds,
// with one HTTP client, and it is a consumer's code rather than the library's.
// Everything here is shaped by libcurl. A program written against another HTTP
// library writes its own `Client` over the same library, and includes
// `borink/object_storage.hpp` alone.
//
// The library allocates nothing at all and throws nothing. This host keeps
// every buffer on the `Client` that the application built, and reuses it for
// the next request. It still allocates twice per request, and both are
// libcurl's terms rather than the library's: `curl_slist_append` copies each
// header line, and a failure builds a message. A host that will not pay for
// the first chooses a different HTTP library, not a different binding.

#pragma once

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <functional>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

#include "borink/object_storage.hpp"

namespace borink::host {

// The name of the HTTP client that this host sends with.
extern const std::string_view client;

// The clock is the host's, not the library's: the library reads no clock and
// takes the time as a number. <chrono> is what this costs, and it is paid
// here rather than by everyone who includes `borink/object_storage.hpp`.
inline std::uint64_t now_unix() {
    const auto since_epoch = std::chrono::system_clock::now().time_since_epoch();
    return static_cast<std::uint64_t>(
        std::chrono::duration_cast<std::chrono::seconds>(since_epoch).count());
}

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
    // The most that one response head may take. libcurl keeps no header
    // buffer of its own, so this host copies each header into an arena that it
    // reserves once at this size. A head that would outgrow it is refused
    // rather than served, exactly as an oversized request head is.
    std::size_t head_bytes = 8 * 1024;
};

// Where this host keeps the response head, and one `borink_header_ref` per
// header.
//
// The library takes the head as borrowed bytes and dictates no layout for it.
// A client whose HTTP library retains its own header buffer points straight
// into that. libcurl retains none: it hands over one header at a time and
// keeps nothing, so this host copies each into an arena. That is libcurl's
// fact, and this class is where it stays.
//
// The arena is reserved once and never grows, because every `borink_header_ref`
// points into it and a reallocation would leave them all dangling. A head that
// would outgrow the reserve is refused: see `overflowed`.
class CollectedHead {
  public:
    // Reserves the arena. Call once, before the first head.
    void reserve(std::size_t capacity) {
        capacity_ = capacity;
        bytes_.reserve(capacity);
    }

    // Starts a new head. A client that reports more than one response, such as
    // a 100 Continue before the answer, calls this for each.
    void restart(std::uint16_t status) {
        status_ = status;
        bytes_.clear();
        headers_.clear();
        overflowed_ = false;
    }

    void field(std::string_view name, std::string_view value) {
        if (bytes_.size() + name.size() + value.size() > capacity_) {
            overflowed_ = true;
            return;
        }
        const borink_bytes stored_name = append(name);
        headers_.push_back(borink_header_ref{stored_name, append(value)});
    }

    std::uint16_t status() const { return status_; }

    // Whether a header did not fit, and was therefore not reported. A head
    // read in part would turn a named failure into an unnamed one.
    bool overflowed() const { return overflowed_; }

    // The headers, as bytes that this client owns. They stay valid until the
    // next `restart`.
    const borink_header_ref *refs() const { return headers_.data(); }

    // How many of them there are.
    std::size_t count() const { return headers_.size(); }

  private:
    borink_bytes append(std::string_view value) {
        const std::size_t start = bytes_.size();
        bytes_.insert(bytes_.end(), value.begin(), value.end());
        return borink_bytes{bytes_.data() + start, value.size()};
    }

    std::uint16_t status_ = 0;
    std::size_t capacity_ = 0;
    bool overflowed_ = false;
    std::vector<std::uint8_t> bytes_;
    std::vector<borink_header_ref> headers_;
};

// One session, and the memory that every request through it reuses.
//
// Build one per client and keep it. Its buffers grow to the largest request
// that this client has made, up to `limits`, and stay that size.
class Client {
  public:
    Client(std::string endpoint, std::string container, std::string token, Limits limits)
        : endpoint_(std::move(endpoint)), container_(std::move(container)),
          token_(std::move(token)), limits_(limits) {
        head_.reserve(limits_.head_bytes);
    }

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

    // The session, built from this client's own strings.
    //
    // The three values point into them, so it is built where it is used rather
    // than stored: moving this client would move the strings out from under a
    // stored one. Refreshing the token is `token()` returning the new one.
    borink_session session() const {
        return borink::session(endpoint_, container_, token_);
    }

    // The head of the response to the last request.
    const CollectedHead &head() const { return head_; }

    // What the last response said.
    //
    // Everything it borrows points into `head()`, and stays valid until the
    // next request through this client. Read `bytes_of` for one such value.
    const borink_outcome &outcome() const { return outcome_; }

    // Reads one range of the request buffer, such as the URL or a header.
    std::string_view part(const borink_span &range) const {
        return {reinterpret_cast<const char *>(request_.data()) + range.start, range.len};
    }

  private:
    // Writes one request head into this client's buffer.
    //
    // `encode` calls the library, which reports the size that the head needs.
    // The buffer grows to that size once and is reused from then on.
    template <typename Encode> const borink_request_head &encode(Encode encode) {
        head_.restart(0);
        diagnostic_.clear();
        outcome_ = borink_outcome{};
        request_head_ = encode();
        if (request_head_.status.code == BORINK_ERROR_CODE_CAPACITY) {
            if (request_head_.required > limits_.request_bytes) {
                throw std::runtime_error("the request head is larger than this client allows");
            }
            request_.resize(request_head_.required);
            request_head_ = encode();
        }
        if (request_head_.status.code != 0) {
            throw std::runtime_error(std::string(describe_into(message_, request_head_.status)));
        }
        return request_head_;
    }

    borink_bytes_mut request_buffer() { return into(request_); }

    // The diagnostic body that this client kept, capped by its limits.
    borink_bytes kept_body() const {
        return borrow(std::span<const std::uint8_t>(diagnostic_));
    }

    // Refuses a head that did not fit the arena, before it is read in part.
    void checked_head() const {
        if (head_.overflowed()) {
            throw std::runtime_error("the response head is larger than this client allows");
        }
    }

    // Reports an outcome that carries no object, in the words of the core
    // crate.
    // `message_` is this client's room for such a sentence, and writing it is
    // the one place that a failing request allocates.
    [[noreturn]] void fail(std::string_view what) {
        const std::string_view said = describe_into(message_, outcome_);
        throw std::runtime_error(std::string(what) + ": " + std::string(said));
    }

    std::string endpoint_;
    std::string container_;
    std::string token_;
    Limits limits_;
    std::vector<std::uint8_t> request_;
    borink_request_head request_head_{};
    std::vector<std::uint8_t> message_;
    CollectedHead head_;
    std::vector<std::uint8_t> diagnostic_;
    borink_outcome outcome_{};
};

} // namespace borink::host
