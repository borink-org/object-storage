// One object, one request, from the command line.
//
// This program is the same for every host in this directory. Which HTTP client
// sends the request is decided by which host it is linked with.
//
//     borink-azure-curl get    <key>      writes the object to standard output
//     borink-azure-curl put    <key>      stores standard input as the object
//     borink-azure-curl delete <key>      removes the object
//     borink-azure-curl list   <prefix>   writes every key under the prefix

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <exception>
#include <iostream>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "host.h"

namespace {

std::string from_environment(const char *name) {
    const char *value = std::getenv(name);
    if (value == nullptr) {
        throw std::runtime_error(std::string(name) + " is not set");
    }
    return value;
}

std::vector<std::uint8_t> read_all(std::FILE *stream) {
    std::vector<std::uint8_t> content;
    std::uint8_t chunk[16 * 1024];
    while (const std::size_t read = std::fread(chunk, 1, sizeof chunk, stream)) {
        content.insert(content.end(), chunk, chunk + read);
    }
    if (std::ferror(stream) != 0) {
        throw std::runtime_error("standard input could not be read");
    }
    return content;
}

void write_all(std::span<const std::uint8_t> part) {
    if (std::fwrite(part.data(), 1, part.size(), stdout) != part.size()) {
        throw std::runtime_error("standard output could not be written");
    }
}

void write_keys(std::span<const borink::ListEntry> entries) {
    for (const borink::ListEntry &entry : entries) {
        std::cout << borink::text_of(entry.key) << "\n";
    }
}

} // namespace

int main(int argc, char **argv) {
    try {
        if (argc != 3) {
            std::cerr << "usage: " << argv[0] << " get|put|delete <key> | list <prefix>\n";
            return 2;
        }
        const std::string_view command = argv[1];
        const std::string_view key = argv[2];

        // One client, holding the buffers that every request through it
        // reuses. An application keeps one of these per container.
        borink::host::Client client =
            borink::host::Client::open(from_environment("AZURE_STORAGE_ENDPOINT"),
                                       from_environment("AZURE_STORAGE_CONTAINER"),
                                       from_environment("AZURE_STORAGE_ACCESS_TOKEN"));

        if (command == "get") {
            client.get(key, write_all);
        } else if (command == "put") {
            const std::vector<std::uint8_t> content = read_all(stdin);
            client.put(key, content);
        } else if (command == "delete") {
            client.remove(key);
        } else if (command == "list") {
            // The array is this program's budget: the client reads every page
            // into it, an arrayful at a time, and holds no more than that.
            std::vector<borink::ListEntry> entries(1000);
            client.list(key, entries, write_keys, borink::List{false, borink::at_most(1000), {}});
        } else {
            std::cerr << "unknown command " << command << "\n";
            return 2;
        }
        return 0;
    } catch (const std::exception &failure) {
        std::cerr << borink::host::client << ": " << failure.what() << "\n";
        return 1;
    }
}
