// What a host does against a server on the loopback address.
//
// One server answers one request with one canned response, and the test reads
// back what the host sent. This is the same program for every host: it is
// linked with one of them, and checks the request that host puts on the wire.

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

#include "borink/host.h"

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

// One HTTP server, for one request.
class Server {
  public:
    explicit Server(std::string response) : response_(std::move(response)) {
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

    // The request that the host sent, once the server has answered it.
    const std::string &received() {
        if (serving_.joinable()) {
            serving_.join();
        }
        return received_;
    }

    std::string_view head() { return std::string_view(received()).substr(0, head_end(received())); }

    std::string_view body() { return std::string_view(received()).substr(head_end(received())); }

  private:
    // Reads the received bytes directly, because the thread that fills them
    // calls this too and may not wait for itself.
    static std::size_t head_end(const std::string &received) {
        const std::size_t end = received.find("\r\n\r\n");
        return end == std::string::npos ? received.size() : end + 4;
    }

    void serve() {
        const int client = ::accept(listener_, nullptr, nullptr);
        if (client < 0) {
            return;
        }
        char chunk[4096];
        while (received_.find("\r\n\r\n") == std::string::npos) {
            const ssize_t read = ::recv(client, chunk, sizeof chunk, 0);
            if (read <= 0) {
                break;
            }
            received_.append(chunk, static_cast<std::size_t>(read));
        }
        const std::size_t end = head_end(received_);
        const std::size_t content = content_length(std::string_view(received_).substr(0, end));
        while (received_.size() - end < content) {
            const ssize_t read = ::recv(client, chunk, sizeof chunk, 0);
            if (read <= 0) {
                break;
            }
            received_.append(chunk, static_cast<std::size_t>(read));
        }
        ::send(client, response_.data(), response_.size(), 0);
        ::close(client);
    }

    static std::size_t content_length(std::string_view head) {
        const std::string lowered = lowercase(std::string(head));
        const std::size_t at = lowered.find("content-length:");
        if (at == std::string::npos) {
            return 0;
        }
        return std::strtoul(lowered.c_str() + at + sizeof("content-length:") - 1, nullptr, 10);
    }

    std::string response_;
    int listener_ = -1;
    std::uint16_t port_ = 0;
    std::thread serving_;
    std::string received_;
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

} // namespace

int main() {
    try {
        reads_an_object();
        writes_an_object();
        removes_an_object();
        names_an_error_that_only_the_body_carries();
        reports_a_missing_object();
        refuses_a_request_over_the_limit();
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
