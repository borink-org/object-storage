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
// formats the time it is given. <chrono> is what this costs, and it is paid
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

// Where the entries of a listing go as they are read.
//
// A listing calls this once per arrayful of entries, for the same reason: the
// application decides what a listing costs it. The entries are valid for the
// call alone.
using EntrySink = std::function<void(std::span<const ListEntry>)>;

// How much memory one client may use.
struct Limits {
    // The most that one template may take. A template that needs more is
    // refused rather than served.
    std::size_t template_bytes = 8 * 1024;
    // The most that one request may take: its head, and the body that the
    // library writes for a commit. A request that needs more is refused
    // rather than served.
    std::size_t request_bytes = 64 * 1024;
    // The most of an error body to read. An error body is a diagnostic, and
    // the service decides how long it is: one that does not arrive costs the
    // name of the error, not the outcome.
    std::size_t error_bytes = 8 * 1024;
    // The most that one response head may take. libcurl keeps no header
    // buffer of its own, so this host copies each header into an arena that it
    // reserves once at this size. A head that would outgrow it is refused
    // rather than served, exactly as an oversized request head is.
    std::size_t head_bytes = 8 * 1024;
    // There is no limit on a listing page. libcurl keeps none of the body, so
    // this client holds the whole page while it reads the entries out of it,
    // and how large that is follows from `ListOptions::max_results`: the
    // service sends no more entries than you asked for. Ask for fewer to hold
    // less.
};

// Where this host keeps the response head, and one `HeaderRef` per
// header.
//
// The library takes the head as borrowed bytes and dictates no layout for it.
// A client whose HTTP library retains its own header buffer points straight
// into that. libcurl retains none: it hands over one header at a time and
// keeps nothing, so this host copies each into an arena. That is libcurl's
// fact, and this class is where it stays.
//
// The arena is reserved once and never grows, because every `HeaderRef`
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
        const Bytes stored_name = append(name);
        headers_.push_back(HeaderRef{stored_name, append(value)});
    }

    std::uint16_t status() const { return status_; }

    // Whether a header did not fit, and was therefore not reported. A head
    // read in part would turn a named failure into an unnamed one.
    bool overflowed() const { return overflowed_; }

    // The headers, as bytes that this client owns. They stay valid until the
    // next `restart`.
    const HeaderRef *refs() const { return headers_.data(); }

    // How many of them there are.
    std::size_t count() const { return headers_.size(); }

  private:
    Bytes append(std::string_view value) {
        const std::size_t start = bytes_.size();
        bytes_.insert(bytes_.end(), value.begin(), value.end());
        return Bytes{bytes_.data() + start, value.size()};
    }

    std::uint16_t status_ = 0;
    std::size_t capacity_ = 0;
    bool overflowed_ = false;
    std::vector<std::uint8_t> bytes_;
    std::vector<HeaderRef> headers_;
};

// What one call of a listing read.
//
// The array holds the whole page: a page larger than it is refused.
struct Page {
    // The part of your array that this call filled.
    std::span<const ListEntry> entries;
    // The text that names the next page of the listing, or empty when the
    // listing is complete.
    //
    // It points into the client's page buffer, so copy it before the next
    // request.
    std::string_view next_marker;
};

// One container and its token, and the memory that every request through it
// reuses.
//
// Build one per client and keep it. Its buffers grow to the largest template
// and the largest request that this client has written, up to `limits`, and
// stay that size.
//
// Each request prepares its template again, into the same buffer. That keeps
// this host short, and costs about as much again as the request. A program
// that sends many requests alike prepares once, and keeps the template and
// its bytes for the next.
class Client {
  public:
    Client(std::string endpoint, std::string container, std::string token, Limits limits,
           PlainHttp plain = PlainHttp::Refused)
        : endpoint_(std::move(endpoint)), container_(std::move(container)),
          token_(std::move(token)), limits_(limits), plain_(plain) {
        head_.reserve(limits_.head_bytes);
    }

    // Opens a client against one container. The endpoint is an HTTPS origin,
    // or with `PlainHttp::Allowed` an HTTP one: see `borink::PlainHttp`.
    //
    // Throws std::runtime_error if the endpoint or the container cannot be
    // used. The message is the sentence that the core crate wrote. A token
    // that cannot be used is reported by the first request.
    static Client open(std::string_view endpoint, std::string_view container,
                       std::string_view token, Limits limits = {},
                       PlainHttp plain = PlainHttp::Refused);

    // Reads `item` with a template of `options`, passing the object's stored
    // bytes to `sink` as they arrive.
    //
    // A head read returns no bytes and calls `sink` not at all. Read the
    // properties from `outcome()` and `head()` afterwards.
    //
    // Throws std::runtime_error if Azure returned no object. The message is
    // the sentence that the core crate wrote for the outcome.
    void get(const GetItem &item, const Sink &sink, const GetOptions &options = get_options());

    // Writes `content` as the whole object that `item` names, with a
    // template of `options`. The bytes stay where the caller put them, and
    // the host sends them from there; `item.content_len` is their length.
    void put(PutItem item, std::span<const std::uint8_t> content,
             const PutOptions &options = put_options());

    // Stages `content` as the block that `item` names.
    void stage(StageBlockItem item, std::span<const std::uint8_t> content,
               const StageOptions &options = {});

    // Commits the object that `item` names from its staged blocks.
    void commit(const CommitItem &item, const CommitOptions &options = commit_options());

    // Reads every key under `prefix`, passing the entries to `sink`.
    //
    // This client asks for one page after another, each into `entries`.
    // `entries` is your budget for the entries themselves, however many keys
    // the container holds, and must hold a whole page. The page they are
    // read out of is one buffer of this client, and `options.max_results` is
    // what decides how large it grows.
    //
    // Throws std::runtime_error if Azure listed nothing.
    void list(std::string_view prefix, std::span<ListEntry> entries, const EntrySink &sink,
              const ListOptions &options = list_options());

    // Reads the page of a listing that `item` names into `entries`.
    //
    // Use this instead of `list` to hold a page across other work, or to ask
    // for one page and stop. The entries point into this client's page buffer,
    // and stay valid until the next request through it. Copy what you keep.
    //
    // Throws what `list` throws.
    Page page(const ListItem &item, std::span<ListEntry> entries,
              const ListOptions &options = list_options());

    // The container, built from this client's own strings.
    //
    // Its values point into those strings. This client builds it where it is
    // used and stores none, because moving the client moves the strings.
    Container container() const { return borink::container(endpoint_, container_, plain_); }

    // The token that each template holds, from this client's own string.
    // Each request prepares its template again, so a new token takes effect
    // with the next request.
    Bytes token() const { return as_bytes(token_); }

    // The head of the response to the last request.
    const CollectedHead &head() const { return head_; }

    // What the last response said.
    //
    // Everything it borrows points into `head()`, and stays valid until the
    // next request through this client. Read `bytes_of` for one such value.
    const Outcome &outcome() const { return outcome_; }

    // Reads one range of the request buffer, such as the URL.
    std::string_view part(const Span &range) const { return borink::part(buffer_, range); }

    // The header fields of the last request, for `next_header` to read.
    Bytes fields() const { return fields_of(buffer_, request_); }

    // Returns the body that the library wrote for the last request, such as
    // the block list of a commit. Returns nothing for a request whose content
    // you send.
    std::span<const std::uint8_t> body() const {
        if (!request_.body.present) {
            return {};
        }
        return std::span<const std::uint8_t>(buffer_).subspan(request_.body.span.start,
                                                              request_.body.span.len);
    }

  private:
    // Writes a template into this client's template buffer.
    //
    // `prepare` calls the library, which reports the size that the template
    // needs. The buffer grows to that size once and is reused from then on.
    template <typename Prepare> const Prepared &prepare(Prepare prepare) {
        prepared_ = prepare(into(template_));
        if (prepared_.status.code == ErrorCodeCapacity) {
            if (prepared_.required > limits_.template_bytes) {
                throw std::runtime_error("the template is larger than this client allows");
            }
            template_.resize(prepared_.required);
            prepared_ = prepare(into(template_));
        }
        if (prepared_.status.code != 0) {
            throw std::runtime_error(std::string(describe_whole(message_, prepared_.status)));
        }
        return prepared_;
    }

    // The bytes of the template that `prepare` wrote last.
    Bytes template_bytes() const { return borink::template_bytes(template_, prepared_); }

    // The timestamps of the current second. The library formats them, and
    // this client asks it to once a second rather than for each request.
    const Timestamps &timestamps() {
        const std::uint64_t now = now_unix();
        if (now != stamped_) {
            stamped_ = now;
            timestamps_ = borink_timestamps_from_unix(now);
        }
        return timestamps_;
    }

    // Writes one request into this client's buffer, as `prepare` writes a
    // template into its own.
    template <typename Encode> const WireRequest &encode(Encode encode) {
        head_.restart(0);
        diagnostic_.clear();
        outcome_ = Outcome{};
        request_ = encode(into(buffer_));
        if (request_.status.code == ErrorCodeCapacity) {
            if (request_.required > limits_.request_bytes) {
                throw std::runtime_error("the request is larger than this client allows");
            }
            buffer_.resize(request_.required);
            request_ = encode(into(buffer_));
        }
        if (request_.status.code != 0) {
            throw std::runtime_error(std::string(describe_whole(message_, request_.status)));
        }
        return request_;
    }

    // Sends the last request, a PUT, with `content` after its head, and
    // collects the response head and at most `error_bytes` of its body.
    void upload(std::span<const std::uint8_t> content);

    // The page of the last listing, as the buffer that the entries are read
    // out of. Reading decodes the text where it stands, so it is writable.
    BytesMut page_buffer() { return into(page_); }

    // The diagnostic body that this client kept, capped by its limits.
    Bytes kept_body() const {
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
        const std::string_view said = describe_whole(message_, outcome_);
        throw std::runtime_error(std::string(what) + ": " + std::string(said));
    }

    std::string endpoint_;
    std::string container_;
    std::string token_;
    Limits limits_;
    PlainHttp plain_;
    std::vector<std::uint8_t> template_;
    Prepared prepared_{};
    // The second that `timestamps_` were formatted for, which no clock
    // reads before the first request.
    std::uint64_t stamped_ = ~std::uint64_t{0};
    Timestamps timestamps_{};
    std::vector<std::uint8_t> buffer_;
    WireRequest request_{};
    std::vector<std::uint8_t> message_;
    CollectedHead head_;
    std::vector<std::uint8_t> diagnostic_;
    std::vector<std::uint8_t> page_;
    // The text that names the page after the one in `page_`. A listing keeps
    // it here because the next request overwrites the bytes it came from.
    std::string marker_;
    Outcome outcome_{};

    // Turns what one fill read into the page that the caller reads.
    Page read(const Fill &fill, std::span<ListEntry> entries);
};

} // namespace borink::host
