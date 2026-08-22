# borink-object-storage

Build Azure Blob GET requests in caller-provided memory and execute them with
the HTTP client of your choice. The caller supplies an access token.

```sh
AZURE_STORAGE_ENDPOINT=https://account.blob.core.windows.net \
AZURE_STORAGE_CONTAINER=container \
AZURE_STORAGE_ACCESS_TOKEN=token \
cargo run --example ureq_get -- path/to/object
```

[`examples/ureq_get.rs`](examples/ureq_get.rs) contains the complete host.
