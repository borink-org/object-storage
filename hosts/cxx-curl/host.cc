// The parts of a client that do not depend on the HTTP client it sends with.

#include "host.h"

#include <stdexcept>
#include <string>
#include <vector>

namespace borink::host {

Client Client::open(std::string_view endpoint, std::string_view container, std::string_view token,
                    Limits limits) {
    Client client{std::string(endpoint), std::string(container), std::string(token), limits};
    const Session session = client.session();
    const Status status = borink_validate(&session);
    if (status.code != 0) {
        // The core crate names the value that cannot be used. This host writes
        // no second table of its own.
        std::vector<std::uint8_t> message(128);
        throw std::runtime_error(std::string(describe_into(message, status)));
    }
    return client;
}

} // namespace borink::host
