// What a host does against a server on the loopback address.
//
// One server answers each request with the next canned response, and the test
// reads back what the host sent. This is the same program for every host: it is
// linked with one of them, and checks the requests that host puts on the wire.

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdint>
#include <cctype>
#include <cstdlib>
#include <iostream>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

#include "host.h"

namespace {

int failures = 0;

void check(bool held, std::string_view what, int line) {
    if (!held) {
        std::cerr << "line " << line << ": " << what << "\n";
        failures += 1;
    }
}

#define CHECK(condition) check((condition), #condition, __LINE__)

std::string lowercase(std::string value) {
    for (char &byte : value) {
        byte = static_cast<char>(std::tolower(static_cast<unsigned char>(byte)));
    }
    return value;
}

// One HTTP server, answering one canned response per request.
class Server {
  public:
    explicit Server(std::string response) : Server(std::vector<std::string>{std::move(response)}) {}

    explicit Server(std::vector<std::string> responses) : responses_(std::move(responses)) {
        listener_ = ::socket(AF_INET, SOCK_STREAM, 0);
        if (listener_ < 0) {
            throw std::runtime_error("no socket");
        }
        const int reuse = 1;
        ::setsockopt(listener_, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof reuse);

        sockaddr_in address{};
        address.sin_family = AF_INET;
        address.sin_addr.s_addr = ::htonl(INADDR_LOOPBACK);
        address.sin_port = 0;
        if (::bind(listener_, reinterpret_cast<sockaddr *>(&address), sizeof address) != 0 ||
            ::listen(listener_, 1) != 0) {
            throw std::runtime_error("no listening socket");
        }
        socklen_t length = sizeof address;
        ::getsockname(listener_, reinterpret_cast<sockaddr *>(&address), &length);
        port_ = ::ntohs(address.sin_port);

        serving_ = std::thread([this] { serve(); });
    }

    ~Server() {
        if (serving_.joinable()) {
            serving_.join();
        }
        ::close(listener_);
    }

    std::string endpoint() const { return "http://127.0.0.1:" + std::to_string(port_); }

    // Wakes the waiting thread for a test whose client never sends anything.
    void stop() {
        ::shutdown(listener_, SHUT_RDWR);
        if (serving_.joinable()) {
            serving_.join();
        }
    }

    // One request that the host sent, once the server has answered them all.
    const std::string &received(std::size_t index = 0) {
        if (serving_.joinable()) {
            serving_.join();
        }
        return requests_.at(index);
    }

    std::string_view head(std::size_t index = 0) {
        const std::string &request = received(index);
        return std::string_view(request).substr(0, head_end(request));
    }

    std::string_view body(std::size_t index = 0) {
        const std::string &request = received(index);
        return std::string_view(request).substr(head_end(request));
    }

  private:
    // Reads the received bytes directly, because the thread that fills them
    // calls this too and may not wait for itself.
    static std::size_t head_end(const std::string &received) {
        const std::size_t end = received.find("\r\n\r\n");
        return end == std::string::npos ? received.size() : end + 4;
    }

    void serve() {
        for (const std::string &response : responses_) {
            const int client = ::accept(listener_, nullptr, nullptr);
            if (client < 0) {
                return;
            }
            std::string request;
            char chunk[4096];
            while (request.find("\r\n\r\n") == std::string::npos) {
                const ssize_t read = ::recv(client, chunk, sizeof chunk, 0);
                if (read <= 0) {
                    break;
                }
                request.append(chunk, static_cast<std::size_t>(read));
            }
            const std::size_t end = head_end(request);
            const std::size_t content = content_length(std::string_view(request).substr(0, end));
            while (request.size() - end < content) {
                const ssize_t read = ::recv(client, chunk, sizeof chunk, 0);
                if (read <= 0) {
                    break;
                }
                request.append(chunk, static_cast<std::size_t>(read));
            }
            requests_.push_back(std::move(request));
            ::send(client, response.data(), response.size(), 0);
            ::close(client);
        }
    }

    static std::size_t content_length(std::string_view head) {
        const std::string lowered = lowercase(std::string(head));
        const std::size_t at = lowered.find("content-length:");
        if (at == std::string::npos) {
            return 0;
        }
        return std::strtoul(lowered.c_str() + at + sizeof("content-length:") - 1, nullptr, 10);
    }

    std::vector<std::string> responses_;
    int listener_ = -1;
    std::uint16_t port_ = 0;
    std::thread serving_;
    std::vector<std::string> requests_;
};

borink::host::Client open(const Server &server) {
    return borink::host::Client::open(server.endpoint(), "container", "token");
}

void reads_an_object() {
    Server server("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: \"tag\"\r\n"
                  "Connection: close\r\n\r\nbody");
    std::vector<std::uint8_t> object;
    borink::host::Client client = open(server);
    client.get("a key", [&](std::span<const std::uint8_t> part) {
        object.insert(object.end(), part.begin(), part.end());
    });

    CHECK(std::string(object.begin(), object.end()) == "body");
    CHECK(server.head().starts_with("GET /container/a%20key HTTP/1.1\r\n"));
    CHECK(lowercase(std::string(server.head())).find("authorization: bearer token\r\n") !=
          std::string::npos);
}

// A read names its range, its precondition and whether it wants the bytes at
// all. Each of those reaches the wire through the shape that C++ stores.
void reads_a_range_of_an_object() {
    Server server("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-5/10\r\n"
                  "Content-Length: 4\r\nConnection: close\r\n\r\npart");
    std::vector<std::uint8_t> object;
    borink::host::Client client = open(server);
    client.get(
        "a key",
        [&](std::span<const std::uint8_t> part) {
            object.insert(object.end(), part.begin(), part.end());
        },
        borink::Read{borink::GetKindBytes, borink::bounded(2, 6), borink::ConditionNone, {}});

    CHECK(std::string(object.begin(), object.end()) == "part");
    CHECK(lowercase(std::string(server.head())).find("range: bytes=2-5\r\n") !=
          std::string::npos);
    CHECK(client.outcome().body.object_offset == 2);
    CHECK(client.outcome().body.object_size.value == 10);
}

// A condition that held returns no object, which this host reports rather than
// deciding for its caller what an unchanged object means.
void reads_an_object_only_if_it_changed() {
    Server server("HTTP/1.1 304 Not Modified\r\nETag: \"tag\"\r\nContent-Length: 0\r\n"
                  "Connection: close\r\n\r\n");
    borink::host::Client client = open(server);
    std::string reported;
    try {
        client.get(
            "a key", [](std::span<const std::uint8_t>) { CHECK(false); },
            borink::Read{borink::GetKindBytes, borink::whole(), borink::ConditionIfNoneMatch,
                         "\"tag\""});
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("not modified") != std::string::npos);
    CHECK(client.outcome().kind == borink::OutcomeKindNotModified);
    CHECK(lowercase(std::string(server.head())).find("if-none-match: \"tag\"\r\n") !=
          std::string::npos);
}

// A metadata read asks for the head alone, which Azure answers with HEAD.
void reads_the_metadata_of_an_object() {
    Server server("HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"tag\"\r\n"
                  "Connection: close\r\n\r\n");
    borink::host::Client client = open(server);
    client.get(
        "a key", [](std::span<const std::uint8_t>) { CHECK(false); },
        borink::Read{borink::GetKindMetadata, borink::whole(), borink::ConditionNone, {}});

    CHECK(server.head().starts_with("HEAD /container/a%20key HTTP/1.1\r\n"));
    CHECK(client.outcome().kind == borink::OutcomeKindComplete);
    CHECK(client.outcome().meta.size.value == 10);
    CHECK(borink::text_of(client.outcome().meta.e_tag) == "\"tag\"");
}

void writes_an_object() {
    Server server("HTTP/1.1 201 Created\r\nETag: \"tag\"\r\nContent-Length: 0\r\n"
                  "Connection: close\r\n\r\n");
    const std::string content = "contents";
    const std::span<const std::uint8_t> bytes(
        reinterpret_cast<const std::uint8_t *>(content.data()), content.size());
    open(server).put("a key", bytes);

    CHECK(server.head().starts_with("PUT /container/a%20key HTTP/1.1\r\n"));
    CHECK(lowercase(std::string(server.head())).find("content-length: 8\r\n") != std::string::npos);
    CHECK(lowercase(std::string(server.head())).find("x-ms-blob-type: blockblob\r\n") !=
          std::string::npos);
    CHECK(server.body() == content);
}

void removes_an_object() {
    Server server("HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    open(server).remove("a key");

    CHECK(server.head().starts_with("DELETE /container/a%20key HTTP/1.1\r\n"));
}

// A removal says what it takes with it, and Azure refuses to guess.
void removes_an_object_and_its_snapshots() {
    Server server("HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    open(server).remove("a key", borink::Removal{borink::DeleteKindObjectAndSnapshots,
                                                 borink::ConditionNone, {}});

    CHECK(lowercase(std::string(server.head())).find("x-ms-delete-snapshots: include\r\n") !=
          std::string::npos);
}

// One page of a listing: the request names the container rather than a key,
// and the entries come out of the body that the host held.
void lists_one_page_of_keys() {
    const std::string body =
        "<EnumerationResults><Blobs>"
        "<Blob><Name>directory/a.txt</Name><Properties><Etag>0x1</Etag>"
        "<Content-Length>4</Content-Length></Properties></Blob>"
        "<BlobPrefix><Name>directory/nested/</Name></BlobPrefix>"
        "</Blobs><NextMarker>next</NextMarker></EnumerationResults>";
    Server server("HTTP/1.1 200 OK\r\nContent-Length: " + std::to_string(body.size()) +
                  "\r\nConnection: close\r\n\r\n" + body);

    borink::host::Client client = open(server);
    std::vector<borink::ListEntry> entries(4);
    const borink::host::Page page =
        client.page("directory/", entries, borink::List{true, borink::at_most(1000), {}});

    CHECK(server.head().starts_with(
        "GET /container?restype=container&comp=list&prefix=directory%2F"
        "&delimiter=%2F&maxresults=1000 HTTP/1.1\r\n"));
    CHECK(page.entries.size() == 2);
    CHECK(borink::text_of(page.entries[0].key) == "directory/a.txt");
    CHECK(page.entries[0].kind == borink::EntryKindObject);
    CHECK(page.entries[0].size.value == 4);
    // A delimited listing reports the level below as one group.
    CHECK(page.entries[1].kind == borink::EntryKindPrefix);
    CHECK(borink::text_of(page.entries[1].key) == "directory/nested/");
    CHECK(page.next_marker == "next");
}

// The array must hold the whole page. One that does not is refused, and the
// sentence says how many entries the page holds.
void an_array_smaller_than_the_page_is_refused() {
    const std::string body = "<EnumerationResults><Blobs>"
                             "<Blob><Name>a.txt</Name><Properties>"
                             "<Content-Length>4</Content-Length></Properties></Blob>"
                             "<Blob><Name>b.txt</Name><Properties>"
                             "<Content-Length>8</Content-Length></Properties></Blob>"
                             "</Blobs><NextMarker /></EnumerationResults>";
    Server server("HTTP/1.1 200 OK\r\nContent-Length: " + std::to_string(body.size()) +
                  "\r\nConnection: close\r\n\r\n" + body);

    borink::host::Client client = open(server);
    std::vector<borink::ListEntry> entries(1);
    std::string reported;
    try {
        client.page("", entries);
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("too small") != std::string::npos);
}

// A whole listing is one call: the client asks for the next page on the marker
// the last one gave, and reads each page in as many rounds as the array takes.
void lists_every_key_over_two_pages() {
    const std::string first = "<EnumerationResults><Blobs>"
                              "<Blob><Name>a.txt</Name><Properties>"
                              "<Content-Length>1</Content-Length></Properties></Blob>"
                              "<Blob><Name>b.txt</Name><Properties>"
                              "<Content-Length>2</Content-Length></Properties></Blob>"
                              "</Blobs><NextMarker>page-2</NextMarker></EnumerationResults>";
    const std::string second = "<EnumerationResults><Blobs>"
                               "<Blob><Name>c.txt</Name><Properties>"
                               "<Content-Length>3</Content-Length></Properties></Blob>"
                               "</Blobs><NextMarker /></EnumerationResults>";
    const auto answer = [](const std::string &body) {
        return "HTTP/1.1 200 OK\r\nContent-Length: " + std::to_string(body.size()) +
               "\r\nConnection: close\r\n\r\n" + body;
    };
    Server server(std::vector<std::string>{answer(first), answer(second)});

    borink::host::Client client = open(server);
    // Room for the larger page, and the sink is called once per page.
    std::vector<borink::ListEntry> entries(2);
    std::vector<std::string> keys;
    std::size_t rounds = 0;
    client.list("", entries, [&](std::span<const borink::ListEntry> read) {
        rounds += 1;
        for (const borink::ListEntry &entry : read) {
            keys.push_back(std::string(borink::text_of(entry.key)));
        }
    });

    CHECK(keys == std::vector<std::string>({"a.txt", "b.txt", "c.txt"}));
    CHECK(rounds == 2);
    // The second request asks for the page that the first one named.
    CHECK(server.head(0).find("&marker=") == std::string_view::npos);
    CHECK(server.head(1).find("&marker=page-2") != std::string_view::npos);
}

// A listing lists a container, and a container that is not there is the one
// thing a listing reports as missing.
void reports_a_container_that_is_not_there() {
    Server server("HTTP/1.1 404 Not Found\r\nx-ms-error-code: ContainerNotFound\r\n"
                  "Content-Length: 0\r\nConnection: close\r\n\r\n");

    borink::host::Client client = open(server);
    std::vector<borink::ListEntry> entries(4);
    std::string reported;
    try {
        client.page("", entries);
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("container does not exist") != std::string::npos);
    CHECK(client.outcome().failure.kind == borink::ServiceErrorNoSuchContainer);
}

// Azure names an error in the head when it can, and in the body when it
// cannot. A host that stops at the head would report neither.
void names_an_error_that_only_the_body_carries() {
    const std::string error =
        "<?xml version=\"1.0\"?><Error><Code>BlobAlreadyExists</Code></Error>";
    Server server("HTTP/1.1 409 Conflict\r\nContent-Length: " + std::to_string(error.size()) +
                  "\r\nx-ms-request-id: request-123\r\nConnection: close\r\n\r\n" + error);

    std::string reported;
    try {
        open(server).put("a key", std::span<const std::uint8_t>());
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("already exists") != std::string::npos);
    CHECK(reported.find("request-123") != std::string::npos);
}

// A read whose head names no error reads a bounded body, and the outcome that
// the library finishes from it names the error and keeps the request id.
void names_an_error_that_only_a_read_body_carries() {
    const std::string error = "<?xml version=\"1.0\"?><Error><Code>ServerBusy</Code></Error>";
    Server server("HTTP/1.1 503 Service Unavailable\r\nContent-Length: " +
                  std::to_string(error.size()) +
                  "\r\nx-ms-request-id: request-123\r\nConnection: close\r\n\r\n" + error);

    borink::host::Client client = open(server);
    std::string reported;
    try {
        client.get("a key", [](std::span<const std::uint8_t>) { CHECK(false); });
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("throttled") != std::string::npos);
    CHECK(reported.find("request-123") != std::string::npos);
    CHECK(client.outcome().kind == borink::OutcomeKindServiceFailure);
    CHECK(client.outcome().failure.kind == borink::ServiceErrorThrottled);
    CHECK(client.outcome().failure.class_ == borink::FailureClassThrottled);
}

void reports_a_missing_object() {
    Server server("HTTP/1.1 404 Not Found\r\nx-ms-error-code: BlobNotFound\r\n"
                  "Content-Length: 0\r\nConnection: close\r\n\r\n");

    std::string reported;
    try {
        open(server).get("a key", [](std::span<const std::uint8_t>) { CHECK(false); });
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("does not exist") != std::string::npos);
}

// A client says how much memory it will lend a request, and a request that
// needs more is refused rather than served.
void refuses_a_request_over_the_limit() {
    Server server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    borink::host::Client client = borink::host::Client::open(
        server.endpoint(), "container", "token", borink::host::Limits{64, 64});

    std::string reported;
    try {
        client.get("a key", [](std::span<const std::uint8_t>) { CHECK(false); });
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("larger than this client allows") != std::string::npos);
    // The server is still waiting for a request that this client never sent.
    server.stop();
}

// The library takes the head as borrowed bytes and dictates no layout for it.
// Two separate buffers are one head, and the outcome points back into both.
void reads_a_head_that_lives_in_two_buffers() {
    const std::string first = "\"tag\"";
    const std::string second = "request-123";
    const borink::Session session =
        borink::session("https://account.example", "container", "token");
    CHECK(borink_validate(&session).code == 0);

    const borink::HeaderRef headers[] = {
        borink::HeaderRef{borink::as_bytes("ETag"), borink::as_bytes(first)},
        borink::HeaderRef{borink::as_bytes("x-ms-request-id"), borink::as_bytes(second)},
        borink::HeaderRef{borink::as_bytes("Content-Length"), borink::as_bytes("10")},
    };
    const borink::GetShape shape = borink::Read{}.shape();
    const borink::Outcome outcome = borink_accept_get_head(
        &session, &shape, 200, headers, sizeof headers / sizeof headers[0]);

    CHECK(outcome.kind == borink::OutcomeKindBody);
    CHECK(borink::text_of(outcome.meta.e_tag) == first);
    CHECK(borink::bytes_of(outcome.meta.e_tag).data() ==
          reinterpret_cast<const std::uint8_t *>(first.data()));
    CHECK(outcome.body.expected_len.value == 10);
}

// A client says how much of a response head it will hold, and a head that
// would outgrow it is refused rather than read in part.
void refuses_a_head_over_the_limit() {
    Server server("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: \"tag\"\r\n"
                  "Connection: close\r\n\r\nbody");
    borink::host::Client client = borink::host::Client::open(
        server.endpoint(), "container", "token", borink::host::Limits{8192, 8192, 8});

    std::string reported;
    try {
        client.get("a key", [](std::span<const std::uint8_t>) { CHECK(false); });
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("response head is larger") != std::string::npos);
}

// A metadata read never reaches the body callback, so it is the path that
// checks the head for itself. It must refuse before it interprets, not after.
void refuses_an_overflowed_head_on_a_read_with_no_body() {
    Server server("HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"tag\"\r\n"
                  "Connection: close\r\n\r\n");
    borink::host::Client client = borink::host::Client::open(
        server.endpoint(), "container", "token", borink::host::Limits{8192, 8192, 8});

    std::string reported;
    try {
        client.get(
            "a key", [](std::span<const std::uint8_t>) { CHECK(false); },
            borink::Read{borink::GetKindMetadata, borink::whole(), borink::ConditionNone, {}});
        CHECK(false);
    } catch (const std::exception &failure) {
        reported = failure.what();
    }
    CHECK(reported.find("response head is larger") != std::string::npos);
}

} // namespace

int main() {
    try {
        reads_an_object();
        reads_a_range_of_an_object();
        reads_an_object_only_if_it_changed();
        reads_the_metadata_of_an_object();
        writes_an_object();
        removes_an_object();
        removes_an_object_and_its_snapshots();
        lists_one_page_of_keys();
        an_array_smaller_than_the_page_is_refused();
        lists_every_key_over_two_pages();
        reports_a_container_that_is_not_there();
        names_an_error_that_only_the_body_carries();
        names_an_error_that_only_a_read_body_carries();
        reports_a_missing_object();
        reads_a_head_that_lives_in_two_buffers();
        refuses_a_request_over_the_limit();
        refuses_a_head_over_the_limit();
        refuses_an_overflowed_head_on_a_read_with_no_body();
    } catch (const std::exception &failure) {
        std::cerr << "unexpected failure: " << failure.what() << "\n";
        return 1;
    }
    if (failures != 0) {
        std::cerr << failures << " check(s) failed with " << borink::host::client << "\n";
        return 1;
    }
    return 0;
}
