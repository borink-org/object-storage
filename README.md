# borink-object-storage

Standard implementations of object storage on crates.io (such as `object_store`) have many dependencies, depend on particular runtimes (Tokio), and cannot avoid global allocations. This makes them unsuitable for various environments:

- Embedded in other programming languages (such as C++)
- Embedded systems with no dynamic allocations or very little memory

Furthermore, since they have many dependencies, they cause a lot of bloat and are slow to compile. For many large Rust applications that already have many of those widely used dependencies, this is not a problem (basically, if you're using Tokio and a web framework, just use the `object_store` crate). But if it is, then `borink-object-storage` is for you.

It uses a style of programming inspired by Zig, but still provides Rust's trademark memory safety and thread safety. All memory is managed by the caller, and all I/O is managed by the caller. It takes sans-I/O to its logical conclusion. Other prior art in this style includes rustls's externally buffered APIs. The library is `no_std` and does not depend on `alloc`, although in the future we will also provide a more convenient API that allocates internally and returns owned data. We hope to support basically every modern platform that can make HTTP calls, although it might require some work on your side.

Build Azure Blob GET requests in caller-provided memory and execute them with the HTTP client of your choice. Currently, the caller provides a Microsoft Entra ID OAuth 2.0 bearer access token for Azure Storage, typically with the `https://storage.azure.com/` audience.

```sh
AZURE_STORAGE_ENDPOINT=https://account.blob.core.windows.net \
AZURE_STORAGE_CONTAINER=container \
AZURE_STORAGE_ACCESS_TOKEN=token \
cargo run --locked --manifest-path hosts/ureq/Cargo.toml --example get -- path/to/object
```

Currently we only provide the sans-I/O core as a library; you must provide the host yourself. [`hosts/ureq`](hosts/ureq) contains an example host.

## Supported features

### Azure Blob Storage only

- Get objects

## Limitations

Currently only ASCII endpoints are supported. Object keys may contain Unicode and are percent-encoded for the request. If you have a use case for internationalized endpoints, please let us know and we'll enable them as an optional feature.
