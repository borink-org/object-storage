// The parts of a client that do not depend on the HTTP client it sends with.

#include "borink/host.h"

#include <stdexcept>
#include <string>

namespace borink::host {

Client Client::open(std::string_view endpoint, std::string_view container, std::string_view token,
                    Limits limits) {
    rust::Box<Session> session =
        open_session(as_bytes(endpoint), as_bytes(container), as_bytes(token));
    switch (session->fault()) {
    case SessionFault::None:
        break;
    case SessionFault::Endpoint:
        throw std::runtime_error("the endpoint is not an ASCII HTTP or HTTPS origin");
    case SessionFault::Container:
        throw std::runtime_error("the container name is not usable in a request");
    case SessionFault::Token:
        throw std::runtime_error("the token is not usable as one header value");
    default:
        throw std::runtime_error("the endpoint, container or token is not text");
    }
    return Client(std::move(session), limits);
}

const char *Client::refusal(PlanOutcome outcome) {
    switch (outcome) {
    case PlanOutcome::InvalidSession:
        return "this client cannot build requests";
    case PlanOutcome::InvalidKey:
        return "the object key is empty, too long, or not text";
    case PlanOutcome::ContentTooLarge:
        return "the content is longer than one request can carry";
    case PlanOutcome::TooManyHeaders:
        return "the request has more headers than the bridge carries";
    default:
        return "the request could not be built";
    }
}

} // namespace borink::host
