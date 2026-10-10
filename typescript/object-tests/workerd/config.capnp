# Runs the object-tests adapter worker in workerd. `grade.sh` bundles the
# worker into dist/worker.js and names the socket's address.
using Workerd = import "/workerd/workerd.capnp";

const config :Workerd.Config = (
  services = [
    (name = "adapter", worker = .adapter),
    # The grader serves its recorded answers on loopback.
    (name = "loopback", network = (allow = ["public", "private", "local"])),
  ],
  sockets = [(name = "http", address = "127.0.0.1:0", http = (), service = "adapter")],
);

const adapter :Workerd.Worker = (
  modules = [(name = "worker.js", esModule = embed "dist/worker.js")],
  compatibilityDate = "2026-09-26",
  globalOutbound = "loopback",
);
