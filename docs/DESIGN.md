Note that this document is human-written and therefore incomplete. It records only design decisions and in particular answers some examples of "why does the code look like that".

First, the most important constraints this library is designed around:

- The library has to be sans-I/O in the core crate and its language bindings. This is so it can be used in multiple execution contexts and in various runtimes. The application should decide the I/O. Furthermore, this allows us to be easily `no_std` (so we do not depend on the Rust `std` library).
- The core library performs no dynamic allocation, not even at startup. All allocations and buffers are owned by the host. This takes sans-I/O to its logical conclusion and allows very fine control over memory usage by consumers. This is useful for multiple purposes, but we care about three of these:
  - Large multi-tenant or multi-user applications (like servers) that want to constrain memory (and general resource) usage per tenant or user or operation
  - Embedded applications simply have very few resources and in some cases cannot even do any dynamic allocation at all.
  - For use in zero-copy I/O contexts, like io_uring, where you might have io_uring right directly into the buffer that you also then pass to this library (this usecase has not been explored in detail yet, so it might not work as well as we would want to!)
- The library should be easily embeddable with very low cost in host applications, also in other languages (particular we target also C and C++). This means very few dependencies (also at build time), fast compilation and glue code already provided for you. It should also try not to make any decisions about scheduling/execution, that is all the domain of the host application.
- Broad platform support. Windows (we very explicitly also target Windows C++ desktop apps) and Linux are the primary target, but we also want to support freestanding targets and targets with very tight resource constraints.
