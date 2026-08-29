// The libcurl host.
//
// libcurl reports the response head through one callback and the body through
// another, and it calls them in that order. A read therefore asks the bridge
// what the head means when the first body byte arrives, and passes on, caps or
// drops what follows from the answer.

#include "host.h"

#include <algorithm>
#include <cstring>
#include <exception>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>

#include <curl/curl.h>

namespace borink::host {
namespace {

// curl_easy_init starts libcurl on its own, but not in a way that two threads
// may reach at once. Starting it here keeps a shared client safe.
void start_curl() {
    static std::once_flag once;
    std::call_once(once, [] {
        if (curl_global_init(CURL_GLOBAL_DEFAULT) != CURLE_OK) {
            throw std::runtime_error("libcurl failed to start");
        }
    });
}

// One easy handle, and the header list that belongs to it.
//
// `curl_slist_append` copies the line it is given, so this is where a request
// allocates: once per header, on libcurl's terms. The bridge allocates none of
// it, and neither does the arena that holds the response head.
class Handle {
  public:
    Handle() {
        start_curl();
        handle_ = curl_easy_init();
        if (handle_ == nullptr) {
            throw std::runtime_error("libcurl gave no handle");
        }
    }

    ~Handle() {
        curl_slist_free_all(headers_);
        curl_easy_cleanup(handle_);
    }

    Handle(const Handle &) = delete;
    Handle &operator=(const Handle &) = delete;

    template <typename Value> void set(CURLoption option, Value value) {
        const CURLcode code = curl_easy_setopt(handle_, option, value);
        if (code != CURLE_OK) {
            throw std::runtime_error(std::string("libcurl refused an option: ") +
                                     curl_easy_strerror(code));
        }
    }

    // libcurl takes the URL and each header as a C string and copies both, so
    // this is where a range of the request buffer becomes one.
    void url(std::string_view value) {
        line_.assign(value);
        set(CURLOPT_URL, line_.c_str());
    }

    void header(std::string_view name, std::string_view value) {
        line_.assign(name);
        line_.append(": ").append(value);
        header(line_.c_str());
    }

    void header(const char *line) {
        curl_slist *grown = curl_slist_append(headers_, line);
        if (grown == nullptr) {
            throw std::runtime_error("libcurl took no header");
        }
        headers_ = grown;
    }

    void send() {
        set(CURLOPT_HTTPHEADER, headers_);
        const CURLcode code = curl_easy_perform(handle_);
        if (code != CURLE_OK) {
            throw std::runtime_error(std::string("libcurl sent no request: ") +
                                     curl_easy_strerror(code));
        }
    }

  private:
    CURL *handle_ = nullptr;
    curl_slist *headers_ = nullptr;
    std::string line_;
};

// Gives libcurl the URL and the headers that the bridge named.
void apply(Handle &handle, const Client &client, const RequestHead &request) {
    handle.url(client.part(request.url));
    for (std::size_t index = 0; index < request.header_count; index += 1) {
        handle.header(client.part(request.headers[index].name),
                      client.part(request.headers[index].value));
    }
    // Azure needs no expectation, and a 100 Continue only costs a round trip.
    handle.header("Expect:");
    // CURLOPT_ACCEPT_ENCODING stays unset, so libcurl neither asks for a
    // compressed body nor decodes one. A decoded body would hold other bytes
    // than the lengths and offsets in the response head describe.
}

std::size_t collect_head(char *data, std::size_t size, std::size_t count, void *user) {
    CollectedHead &head = *static_cast<CollectedHead *>(user);
    const std::size_t length = size * count;
    std::string_view line(data, length);
    while (!line.empty() && (line.back() == '\r' || line.back() == '\n')) {
        line.remove_suffix(1);
    }
    // A status line starts a head. libcurl reports every head it receives, and
    // only the last one answers the request.
    if (line.starts_with("HTTP/")) {
        const std::size_t space = line.find(' ');
        std::uint16_t status = 0;
        if (space != std::string_view::npos) {
            for (const char digit : line.substr(space + 1)) {
                if (digit < '0' || digit > '9') {
                    break;
                }
                status = static_cast<std::uint16_t>(status * 10 + (digit - '0'));
            }
        }
        head.restart(status);
        return length;
    }
    const std::size_t colon = line.find(':');
    if (colon == std::string_view::npos) {
        return length;
    }
    std::string_view value = line.substr(colon + 1);
    while (!value.empty() && (value.front() == ' ' || value.front() == '\t')) {
        value.remove_prefix(1);
    }
    head.field(line.substr(0, colon), value);
    return length;
}

// Keeps at most `cap` bytes of a body, and reports every byte as taken. A body
// that runs past the cap is dropped rather than stopping the transfer.
std::size_t keep(std::vector<std::uint8_t> &bytes, std::size_t cap, const char *data,
                 std::size_t length) {
    const std::size_t room = cap - std::min(cap, bytes.size());
    bytes.insert(bytes.end(), data, data + std::min(room, length));
    return length;
}

// A read, which has to know what the head means before the body arrives.
struct Reading {
    const Session &session;
    GetShapeView shape;
    const Sink &sink;
    const CollectedHead &head;
    std::vector<std::uint8_t> &diagnostic;
    std::size_t error_bytes;
    std::optional<Outcome> outcome;
    // A callback may not let an exception reach libcurl, so it stops the
    // transfer and keeps the exception for the caller to rethrow.
    std::exception_ptr failure;

    void decide() { outcome = session.accept_get_head(shape, head.status(), head.refs()); }

    std::size_t take(const char *data, std::size_t length) {
        if (!outcome.has_value()) {
            // A head read in part would turn a named failure into an unnamed
            // one, so it stops the transfer instead of being interpreted.
            if (head.overflowed()) {
                throw std::runtime_error("the response head is larger than this client allows");
            }
            decide();
        }
        switch (outcome->disposition) {
        case Disposition::Body:
            // The object never lands in a buffer of this host. It goes
            // straight from libcurl to whoever asked for it.
            sink(std::span<const std::uint8_t>(reinterpret_cast<const std::uint8_t *>(data),
                                               length));
            return length;
        case Disposition::NeedErrorBody:
            return keep(diagnostic, error_bytes, data, length);
        default:
            return length;
        }
    }
};

std::size_t read_body(char *data, std::size_t size, std::size_t count, void *user) {
    Reading &reading = *static_cast<Reading *>(user);
    try {
        return reading.take(data, size * count);
    } catch (...) {
        // The bridge throws nothing, so this catches what the sink raised.
        reading.failure = std::current_exception();
        return 0;
    }
}

// A body that only ever carries a diagnostic, which a write and a removal
// answer with.
struct Diagnostic {
    std::vector<std::uint8_t> &bytes;
    std::size_t cap;
};

std::size_t collect_diagnostic(char *data, std::size_t size, std::size_t count, void *user) {
    Diagnostic &diagnostic = *static_cast<Diagnostic *>(user);
    return keep(diagnostic.bytes, diagnostic.cap, data, size * count);
}

// The content of a write, which stays where the caller put it.
struct Content {
    std::span<const std::uint8_t> bytes;
    std::size_t sent = 0;
};

std::size_t send_content(char *data, std::size_t size, std::size_t count, void *user) {
    Content &content = *static_cast<Content *>(user);
    const std::size_t taking = std::min(content.bytes.size() - content.sent, size * count);
    std::memcpy(data, content.bytes.data() + content.sent, taking);
    content.sent += taking;
    return taking;
}

} // namespace

const std::string_view client = "libcurl";

void Client::get(std::string_view key, const Sink &sink, const Read &read) {
    const std::uint64_t now = now_unix();
    const GetShapeView shape = read.shape();
    const RequestHead &request = encode([&] {
        return session_->encode_get(shape, as_bytes(key), as_bytes(read.condition_value),
                                    request_buffer(), now);
    });

    Reading reading{*session_,   shape,               sink,         head_,
                    diagnostic_, limits_.error_bytes, std::nullopt, nullptr};
    Handle handle;
    apply(handle, *this, request);
    // A metadata read sends HEAD, which carries no body at all.
    if (request.method == Method::Head) {
        handle.set(CURLOPT_NOBODY, 1L);
    }
    handle.set(CURLOPT_HEADERFUNCTION, collect_head);
    handle.set(CURLOPT_HEADERDATA, &head_);
    handle.set(CURLOPT_WRITEFUNCTION, read_body);
    handle.set(CURLOPT_WRITEDATA, &reading);
    try {
        handle.send();
    } catch (...) {
        // A callback that stopped the transfer kept the reason. libcurl
        // reports only that it was stopped, so the reason wins.
        if (reading.failure) {
            std::rethrow_exception(reading.failure);
        }
        throw;
    }
    if (reading.failure) {
        std::rethrow_exception(reading.failure);
    }
    // A response without a body never reaches the body callback, so the head
    // still has to be read here. It is checked first, for the reason that
    // `Reading::take` checks: a head read in part is never interpreted.
    checked_head();
    if (!reading.outcome.has_value()) {
        reading.decide();
    }
    outcome_ = *reading.outcome;
    // Azure names the error in the head when it can. When it did not, the
    // diagnostic body names it, and the bridge finishes the outcome from it.
    if (outcome_.disposition == Disposition::NeedErrorBody) {
        outcome_ = session_->finish_get_error_body(outcome_.failure, kept_body());
    }
    switch (outcome_.disposition) {
    case Disposition::Body:
    case Disposition::Complete:
        return;
    default:
        fail("Azure returned no object");
    }
}

void Client::put(std::string_view key, std::span<const std::uint8_t> content,
                 const Write &write) {
    const std::uint64_t now = now_unix();
    const std::uint64_t length = content.size();
    const PutShapeView shape = write.shape();
    const RequestHead &request = encode([&] {
        return session_->encode_put(shape, as_bytes(key), as_bytes(write.condition_value),
                                    request_buffer(), length, now);
    });

    Content sending{content, 0};
    Diagnostic diagnostic{diagnostic_, limits_.error_bytes};
    Handle handle;
    apply(handle, *this, request);
    handle.set(CURLOPT_UPLOAD, 1L);
    handle.set(CURLOPT_INFILESIZE_LARGE, static_cast<curl_off_t>(content.size()));
    handle.set(CURLOPT_READFUNCTION, send_content);
    handle.set(CURLOPT_READDATA, &sending);
    handle.set(CURLOPT_HEADERFUNCTION, collect_head);
    handle.set(CURLOPT_HEADERDATA, &head_);
    handle.set(CURLOPT_WRITEFUNCTION, collect_diagnostic);
    handle.set(CURLOPT_WRITEDATA, &diagnostic);
    handle.send();

    checked_head();
    outcome_ = session_->accept_put_head(shape, head_.status(), head_.refs());
    if (outcome_.disposition == Disposition::NeedErrorBody) {
        outcome_ = session_->finish_put_error_body(outcome_.failure, kept_body());
    }
    if (outcome_.disposition != Disposition::Done) {
        fail("Azure stored no object");
    }
}

void Client::remove(std::string_view key, const Removal &removal) {
    const std::uint64_t now = now_unix();
    const DeleteShapeView shape = removal.shape();
    const RequestHead &request = encode([&] {
        return session_->encode_delete(shape, as_bytes(key), as_bytes(removal.condition_value),
                                       request_buffer(), now);
    });

    Diagnostic diagnostic{diagnostic_, limits_.error_bytes};
    Handle handle;
    apply(handle, *this, request);
    handle.set(CURLOPT_CUSTOMREQUEST, "DELETE");
    handle.set(CURLOPT_HEADERFUNCTION, collect_head);
    handle.set(CURLOPT_HEADERDATA, &head_);
    handle.set(CURLOPT_WRITEFUNCTION, collect_diagnostic);
    handle.set(CURLOPT_WRITEDATA, &diagnostic);
    handle.send();

    checked_head();
    outcome_ = session_->accept_delete_head(shape, head_.status(), head_.refs());
    if (outcome_.disposition == Disposition::NeedErrorBody) {
        outcome_ = session_->finish_delete_error_body(outcome_.failure, kept_body());
    }
    if (outcome_.disposition != Disposition::Accepted) {
        fail("Azure removed no object");
    }
}

} // namespace borink::host
