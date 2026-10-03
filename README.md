# borink-object-storage

Standard implementations of object storage on crates.io (such as `object_store`) have many dependencies, depend on particular runtimes (Tokio), and cannot avoid global allocations. This makes them unsuitable for various environments:

- Embedded in other programming languages (such as C and C++)
- Embedded systems with no dynamic allocations or very little memory

Furthermore, since they have many dependencies, they can quickly bloat your supply chain and are slow to compile. For many large Rust applications that already have many of those widely used dependencies, this is not a problem (basically, if you're using Tokio and a web framework, just use the `object_store` crate). But if it is a problem, then `borink-object-storage` is for you. Additionally, when crates freely allocate a lot of memory internally for things like scratch space and can abort when these allocations fail, it makes it quite hard to manage resources. For example, you might want to limit the memory used by one tenant in a multi-tenant environment.

This library uses a style of programming inspired by Zig, but still provides Rust's trademark memory safety and thread safety (of course, even Rust is not totally safe due to `unsafe` usage, which sometimes cannot be avoided). All memory is managed by the caller, and all I/O is managed by the caller. It takes sans-I/O to its logical conclusion. The library is `no_std` and does not depend on `alloc`, although in the future we will also provide a more convenient API that allocates internally and returns owned data. We hope to support basically every modern platform that can make HTTP calls, although it might require some work on your side.

Currently we only provide the sans-I/O core as a library; you must provide the HTTP client yourself. [`hosts/ureq`](https://github.com/borink-org/object-storage/tree/master/hosts/ureq) contains an example host.

We also provide C/C++ bindings, but these only implement a subset of the features the Rust crate do, and only Azure for now. They will be brought up to par before the 1.0 release.

## Supported features

### Azure Blob Storage, S3 and S3-compatible services

- Object get (GET request)
  - Conditional (If-Match, If-None-Match, If-Modified-Since, If-Unmodified-Since)
  - Byte ranges: offset, bounded, suffix (S3 only, Azure refuses suffix ranges)
- Object metadata and information (HEAD request), including the content properties, the stored MD5 and the storage class or access tier
- Object put (PUT request, whole object)
  - Conditional (If-Match, If-None-Match; If-Modified-Since and If-Unmodified-Since on Azure only)
  - Content properties (Content-Type, Content-Encoding, Content-Language, Content-Disposition, Cache-Control), tags, and the storage class or access tier
  - Content is borrowed or streamed: the head states its length, so a write can come from a file or a socket without holding the object in memory
  - S3: signs the SHA-256 of the content, or leave it unsigned or provide the hash yourself
- Object delete (DELETE request)
  - Conditional (If-Match; If-None-Match and the date conditions on Azure only)
  - Takes the object alone, the object and its snapshots, or the snapshots alone (Azure only)
- Object listing (GET request on the container or bucket, one page at a time)
  - Custom delimiters, prefixes
  - Some object properties are always returned, others can be registered to return in a custom entry return type
  - S3: start after a key (not possible on Azure)
- Deleting several objects in one request (Azure Blob Batch, S3 DeleteObjects)
- Object tags: reading and replacing them
- Metadata (arbitrary key-value pairs attached to objects) reading and writing
  - Correctly encodes (even where e.g. the AWS C++ SDK doesn't) and rejects values that don't roundtrip, or (correctly) rejects non-ASCII in the case of Azure
- Checksums (crypto implementations through [`crates/object-storage-crypto`](https://github.com/borink-org/object-storage/tree/master/crates/object-storage-crypto))
  - MD5 and CRC64 on both; CRC32, CRC32C, SHA-1 and SHA-256 on S3
- Object multipart upload
- Response classification: object metadata, byte-range windows, request IDs, and complete error handling
- Support for less strict verification to better support S3-compatible services

### Azure Blob Storage-only

- Object listing: listing metadata (S3 does not list it)
- Setting the access tier of an object

### S3-only

- SigV4 handling for every request (crypto implementations again through [`crates/object-storage-crypto`](https://github.com/borink-org/object-storage/tree/master/crates/object-storage-crypto) or user-provided)
- Directory buckets (S3 Express One Zone), including CreateSession and signing with the session's credentials
- Uploads in parts: create and abort an upload, read a commit that fails under status 200, and have S3 check the length of the committed object
- Errors that S3 writes into the body of a success response, on every operation that reads the body

## What makes `borink-object-storage` unique?

- We try to give the caller as much control as possible (this sometimes makes the APIs a bit clunky), in particular over memory and buffers.
- We aim to very thoroughly test everything and make sure we capture as many of the real service's edge cases as possible (we e.g. measure exactly how many path segments Azure supports). In this process, we developed [borink-object-tests](https://github.com/borink-org/object-tests), which should hopefully help other developers of object storage clients as well.
- We aim to support Windows and Linux well, with the goal to also support low memory and freestanding targets (not yet validated).
- The `proto` crate has no dependencies. Any dependencies of other crates are optional and pluggable (you can provide your own crypto implementation) and use whatever HTTP client you wish. This also helps make our build very fast (cold release build takes <5s on a mid-spec laptop, debug build takes under a second).

## Development status and roadmap

The goal is a full-featured object storage library that supports both Azure Blob Storage and S3 (including S3-compatible services). The goal is to also include a lot of useful functionality around the basic operations, in particular authentication/authorization features (as usually the SDK's and existing libraries can be quite heavy). This includes things like AssumeRoleWithWebIdentity and OIDC token exchange (e.g. exchanging your GitHub Actions identity token for a short-lived Azure one).

The core library functionality is not expected to change a lot from now on, but there is no API stability yet. That will come in 1.0, which I'm planning to get to sooner rather than later. We have initial support for S3 and Azure. Until 0.1, do expect some significant churn. The main approach of the core library was already validated before, but the C/C++ layer might still go through some iterations.

Roadmap:
- S3 directory buckets, S3 Express One Zone, full Azure HNS compatibility -> 0.0.4 release
- S3 multipart -> 0.0.5 release
- ... potentially various other features: Azure snapshots, versions
- 0.1 release (with promise to try and keep the Rust API stable from now on, but no guarantee)
- ... support for various AWS and Azure authorization schemes -> 0.2 release
- Generic API (so layer over the providers) -> 0.3 release
- Convenience API that allocates -> 0.4 release
- C/C++ bindings for the provider-specific, generic and convenience APIs, will remain unstable and versioned separately
- Rust API stability promise -> 1.0
- ... potentially support various additional Azure/AWS features (e.g. appends, page blobs, Arrow listings)
- ... various improvements to the convenience layer and API and CLI that implements various non-core features that are coupled to the transport

## Usage notes

### Compressed objects

Azure and S3 store an object as opaque bytes and never compress it for you. If you upload compressed bytes and set `Content-Encoding`, the service stores and serves those bytes, and every byte range, length and offset counts them.

This crate passes such objects through. It reports the encoding in `ObjectMeta::content_encoding` and leaves the bytes alone, so you can decompress them yourself. Your HTTP client must do the same. Turn off its automatic decompression, such as the `gzip` feature of `reqwest` or of `ureq`. Those features decode the body and remove the headers that record it. The offsets and lengths that this crate reports would then no longer describe the bytes you receive.

## Limitations

- Currently only ASCII endpoints are supported. Object keys may contain Unicode and are percent-encoded for the request. If you have a use case for internationalized endpoints, please let us know and we'll enable them as an optional feature.
- On Azure, the only authorization currently supported is a Microsoft Entra ID OAuth 2.0 bearer token. In the future we will also include code for creating these tokens based on other secrets or even a managed identity. On S3, requests are signed with an access key and optional session token that you pass.

## LLM disclaimer

This project is heavily AI-assisted. It began simply by generating code that didn't allocate and had only a few dependencies, as a private proof of concept. That original proof of concept was then carefully reconstructed in multiple steps (you can see the PRs and commits). Note that basically none of the individual lines of code and API docs are written by hand, but the models were heavily steered and went through multiple rounds of review. The core code and APIs were all reviewed carefully. Care was taken for the API docs to be nice to read and not contain too much slop. Apologies if some stuff slipped through review, we're all still new to this era of software engineering (I still think that, as opposed to just pure vibe coding, this still counts as engineering! Feel free to disagree).
