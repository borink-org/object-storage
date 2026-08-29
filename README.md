# borink-object-storage

Object storage — Azure Blob Storage today — with a sans-I/O core that allocates
nothing and leaves every buffer and every HTTP call to the caller. The pitch,
the supported features and the limitations are in the core crate's README:
[`crates/object-storage-proto`](crates/object-storage-proto).

## Layout

The repository has two axes, and a directory for each.

**`crates/` is what a consumer depends on.** Each is a library with a stable
name: `crates/object-storage-proto` is the core (`borink-object-storage-proto`,
`no_std`, no allocator, no I/O). Bindings for other languages join it here, one
directory each.

**`hosts/` is programs that drive one of those crates with a real HTTP client.**
A host owns a transport, its buffers, a clock and a sink, and shows what a
consumer writes; nothing depends on one. [`hosts/ureq`](hosts/ureq) is the Rust
reference host, and [`hosts/cxx`](hosts/cxx) is the C++ bridge with a libcurl
host built on it — the bridge allocates once per client and never per request,
and returns no `Result`, so an application built without exceptions can use it.

`checks/` holds builds that assert a property rather than a behaviour (the
allocator-free link), and `tests/azure-live` is the suite that needs
credentials. `docs/` holds the design documents.
