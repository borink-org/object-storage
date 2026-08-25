# borink-object-storage

Standard implementations of object storage on crates.io (such as `object_store`) have many dependencies, depend on particular runtimes (Tokio), and cannot avoid global allocations. This makes them unsuitable for various environments:

- Embedded in other programming languages (such as C++)
- Embedded systems with no dynamic allocations or very little memory

Furthermore, since they have many dependencies, they can quickly bloat your supply chain and are slow to compile. For many large Rust applications that already have many of those widely used dependencies, this is not a problem (basically, if you're using Tokio and a web framework, just use the `object_store` crate). But if it is, then `borink-object-storage` is for you. Additionally, when crates freely allocate a lot of memory internally for things like scratch space and can abort when these allocations fail, it makes it quite hard to manage resources. For example, you might want to limit the memory used by one tenant in a multi-tenant environment.

This library uses a style of programming inspired by Zig, but still provides Rust's trademark memory safety and thread safety (of course, even Rust is not totally safe due to `unsafe` usage, which sometimes cannot be avoided). All memory is managed by the caller, and all I/O is managed by the caller. It takes sans-I/O to its logical conclusion. The library is `no_std` and does not depend on `alloc`, although in the future we will also provide a more convenient API that allocates internally and returns owned data. We hope to support basically every modern platform that can make HTTP calls, although it might require some work on your side.

Currently we only provide the sans-I/O core as a library; you must provide the host yourself. [`hosts/ureq`](hosts/ureq) contains an example host.

## Supported features

### Azure Blob Storage only

- Object get (GET request)
  - Conditional object get (If-Match, If-None-Match)
  - Byte ranges
- Object metadata (HEAD request)
- Response classification: object metadata, byte-range windows, and Azure error
  codes and request IDs, all borrowed from the caller's response headers

## Compressed objects

Azure stores a blob as opaque bytes and never compresses it for you. If you
uploaded compressed bytes and set `Content-Encoding`, then those bytes are what
Azure stores and serves, and every byte range, length and offset counts them.

This crate passes such objects through: it reports the encoding in
`ObjectMeta::content_encoding` and leaves the bytes alone, so you can decompress
them yourself. Your HTTP client must do the same. Turn off its automatic
decompression, such as the `gzip` feature of `reqwest` or of `ureq`: those
decode the body and remove the headers that record it, so the offsets and
lengths this crate reports would no longer describe the bytes you receive.

## Limitations

- Currently only ASCII endpoints are supported. Object keys may contain Unicode and are percent-encoded for the request. If you have a use case for internationalized endpoints, please let us know and we'll enable them as an optional feature.
- The only authorization currently supported is a Microsoft Entra ID OAuth 2.0 bearer token. In the future we will also include code for creating these tokens based on other secrets or even a managed identity.
