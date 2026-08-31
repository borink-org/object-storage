## Code adapted into this repository

### rusty-s3 — BSD-2-Clause

<https://github.com/paolobarbolini/rusty-s3>, 0.10.1, by Paolo Barbolini and Federico Guerinoni.

`borink-object-storage` is a rewrite of a heavily modified fork of `rusty-s3`. It shares no further structure, other than some basic design goals. However, at least one part survives in full: in `crates/object-storage-proto/src/path.rs`, the `OBJECT_KEY_ESCAPE` percent-encoding `AsciiSet`, is `rusty-s3`'s `FRAGMENT` set from `src/signing/util.rs`, added to the same `CONTROLS` base. 

`rusty-s3` is not a dependency of any crate here. When SigV4 signing lands, the canonical-query-string rules and the AWS test vectors are expected to come from the same source, and this section is where that gets recorded.

Its license is reproduced here as required:

```
BSD 2-Clause License

Copyright (c) 2020-2025, Paolo Barbolini <paolo@paolo565.org>
Copyright (c) 2020-2025, Federico Guerinoni <guerinoni.federico@gmail.com>

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## Inspiration and reference projects

None of the projects below is a dependency, and no code from any of them is in this tree. They were read while the API was designed: the object-storage SDKs for what an operation and its options are called, and the sans-I/O, serialization and allocator families for how a library takes its memory and its bytes from the caller. `docs/DESIGN.md` states the constraints they were read against.

Each row says what was read and whether this crate does the same thing. Many rows say it does not: a pattern that was rejected after reading it is still a debt to the project that stated it clearly.

Licenses are named so a reader can check them. Two of them (wolfSSL, mbedTLS) are not permissive in the way the rest are, which is a further reason nothing was copied from any of them.

### Object-storage SDKs

Read for the operation vocabulary: which operations exist, what their options are called, and how a client is layered.

| Project | License (SPDX) | Upstream | Read for |
| --- | --- | --- | --- |
| aws-sdk-rust | `Apache-2.0` | <https://github.com/awslabs/aws-sdk-rust> | The Smithy model in `aws-models/s3.json`, which names and shapes every S3 operation. This crate does not implement S3 yet; the model is what it will be written against. |
| smithy-rs | `Apache-2.0` | <https://github.com/smithy-lang/smithy-rs> | How the Smithy model becomes Rust: generated per-operation input, output and error types under a hand-written client. This crate generates nothing and has no builders, so the reading informed the S3 vocabulary and not the API shape. |
| azure-sdk-for-cpp | `MIT` | <https://github.com/Azure/azure-sdk-for-cpp> | One options struct per operation, and which Azure headers each operation sends. `PhysicalGet`, `PhysicalPut` and `PhysicalDelete` cover the same ground for the operations this crate implements. |
| azure-sdk-for-net | `MIT` | <https://github.com/Azure/azure-sdk-for-net> | `Azure.Storage.Blobs.netstandard2.0.cs`, which states the whole Azure blob surface in one file. It was the reference for what the options and results are called. |
| minio-cpp | `Apache-2.0` | <https://github.com/minio/minio-cpp> | `BaseClient` carrying the complete operation set with `Client` adding conveniences on top. This crate splits the same way: `Blobs` holds the operations, and the `layered` module holds helpers that use only its public API. |
| go-cloud (`blob`) | `Apache-2.0` | <https://github.com/google/go-cloud> | A portable interface deliberately kept to what every backend can do, with per-driver escape hatches for the rest. This crate is Azure-only today and has no generic layer; when it gets one, this is the shape it is planned to take. |
| Apache Arrow (C++ `FileSystem`) | `Apache-2.0` | <https://github.com/apache/arrow> | Object storage presented as a filesystem. This crate does not do that: it has no path type beyond an object key, no directory operations and no `Move`. Read to decide against it. |
| arrow-rs `object_store` | `Apache-2.0` | <https://github.com/apache/arrow-rs-object-store> | The Rust incumbent. It is what the README compares this crate against, and what a reader is most likely to be migrating from. |
| s3-rs (`shiguredo_s3`) | `Apache-2.0` | <https://github.com/shiguredo/s3-rs> | A sans-I/O S3 client that mirrors `aws-sdk-rust`'s names on purpose so callers can move between them, and `S3Request::expect_no_body`, which states in the request what the response must not have. `GetShape` carries the same kind of statement from `encode_get` to `accept_get_head`. |

### Sans-I/O protocol implementations

Read for the buffer contract: what a library that never touches a socket asks of its caller.

Two of these are in the crate:

| Project | License (SPDX) | Upstream | Read for |
| --- | --- | --- | --- |
| BearSSL | `MIT` | <https://bearssl.org/git/BearSSL> | All connection state in a caller-owned context, and `br_sslio_*` sitting on top of the engine rather than inside it. That layering is `Blobs` and `layered`. Its window-and-ack buffer pump is not here: a request head is written once and a body passes through untransformed, so there is nothing to pump. |
| http11-rs | `Apache-2.0` | <https://github.com/shiguredo/http11-rs> | `BodyKind::CloseDelimited`, which distinguishes a body whose length the head states from one it does not. `BodyWindow::expected_len` is `Option<u64>` for that reason. Its `DecoderLimits` has no counterpart here, because this crate parses no stream and so has nothing to cap. |

The rest were read and are not used:

| Project | License (SPDX) | Upstream | Read for |
| --- | --- | --- | --- |
| smoltcp | `0BSD` | <https://github.com/smoltcp-rs/smoltcp> | Evidence that a protocol as large as TCP/IP runs on caller-supplied slices. This crate borrows the caller's buffer directly and needs no `PacketBuffer`. |
| httparse | `MIT OR Apache-2.0` | <https://github.com/seanmonstar/httparse> | The caller-supplied `&mut [Header]` array, and `Partial` versus `Complete`. This crate parses no HTTP: the host fills a `ResponseHead` with slices it already holds. |
| picohttpparser | `MIT OR Artistic-1.0-Perl` | <https://github.com/h2o/picohttpparser> | The same contract in 90 lines of header, plus `last_len` for re-parsing a grown buffer cheaply. Neither is needed for a head that is read once. |
| llhttp | `MIT` | <https://github.com/nodejs/llhttp> | A caller-owned parser struct with pause and resume. Every call in this crate is one-shot, so there is no parser to own or resume. |
| embedded-tls | `Apache-2.0` | <https://github.com/drogue-iot/embedded-tls> | Record buffers handed over at construction and held for the connection. This crate holds no buffer between calls. |
| picotls | `MIT` | <https://github.com/h2o/picotls> | `ptls_buffer_t`, which starts in the caller's array and heap-allocates only if it outgrows it. This crate refuses instead, and returns the exact size needed. |
| lwip | `BSD-3-Clause` | <https://github.com/lwip-tcpip/lwip> | `memp` pools carved from static arrays, and custom pbufs for zero-copy over caller memory. This crate has no long-lived objects to pool. |
| nghttp2 | `MIT` | <https://github.com/nghttp2/nghttp2> | `nghttp2_mem`, an allocator vtable the caller supplies. Rejected: this crate allocates nothing, so it needs no allocator, not even an injected one. |
| Boost.Beast | `BSL-1.0` | <https://github.com/boostorg/beast> | A message layer with no I/O, the allocator as a template parameter, and explicit header and body limits. Same rejection as nghttp2. |
| mbedTLS | `Apache-2.0 OR GPL-2.0-or-later` | <https://github.com/Mbed-TLS/mbedtls> | Three separable knobs: I/O callbacks, a global calloc/free override, and a heap seeded from a static buffer. None applies to a crate with no heap. |
| wolfSSL | `GPL-3.0-or-later` (or commercial) | <https://github.com/wolfSSL/wolfssl> | `WOLFSSL_STATIC_MEMORY`: one caller buffer carved into size-bucketed pools. Read for the shape of the configuration only. |

### Serialization and parsers

Read for how a parser or encoder reports what it needs before it writes.

| Project | License (SPDX) | Upstream | Read for |
| --- | --- | --- | --- |
| jsmn | `MIT` | <https://github.com/zserge/jsmn> | Count mode: pass a null token array and be told how many tokens the input needs. `layered::get_requirements` is the same call — it encodes into an empty buffer and reads `CapacityError::required`. |
| QCBOR | `BSD-3-Clause` | <https://github.com/laurencelundblade/QCBOR> | `SizeCalculateUsefulBuf`, the same counting pass under another name, and `UsefulBuf` as one buffer type used everywhere. |
| Boost.JSON | `BSL-1.0` | <https://github.com/boostorg/json> | `null_resource`, a memory resource that fails on any allocation, used to prove a parse allocates nothing. `checks/no-allocator` proves the same thing by linking without an allocator at all. |
| nanopb | `Zlib` | <https://github.com/nanopb/nanopb> | Encoding and decoding a whole format with no dynamic allocation. Read as evidence the constraint is workable; nothing from it is here. |
| postcard | `MIT OR Apache-2.0` | <https://github.com/jamesmunns/postcard> | `max_size`, a compile-time worst-case size a host can provision against. Not used: this crate answers at runtime with the exact size instead, because the size depends on the object key. |
| serde-json-core | `MIT OR Apache-2.0` | <https://github.com/rust-embedded-community/serde-json-core> | A `no_std` serde surface over slices. Not used; this crate has no serde support. |
| simdjson | `Apache-2.0` | <https://github.com/simdjson/simdjson> | Allocating a parser's capacity once and reusing it across documents. Not used: nothing here is retained between calls. |

### Allocators and fixed-capacity storage

Read to decide what this crate should ask for when it needs storage. It ended up asking for `&mut [u8]`, so none of these is used, and none of their types appears in a signature here.

| Project | License (SPDX) | Upstream | Read for |
| --- | --- | --- | --- |
| foonathan/memory | `Zlib` | <https://github.com/foonathan/memory> | The full taxonomy — static storage, arena, stack, pool — and `static_allocator_storage<N>`, the caller-provided-memory base case. |
| heapless | `MIT OR Apache-2.0` | <https://github.com/rust-embedded/heapless> | Capacity as a const generic, and the owned-versus-view split that lets a container's capacity be erased. |
| talc | `MIT` | <https://github.com/SFBdragon/talc> | A `no_std` heap whose memory `Source` the caller claims explicitly. |
| embedded-alloc | `MIT OR Apache-2.0` | <https://github.com/rust-embedded/embedded-alloc> | The smallest version of the same idea: `Heap::empty()` in a `static`, then `init(start, size)`. |
| allocator-api2 | `MIT OR Apache-2.0` | <https://github.com/zakarumych/allocator-api2> | Per-call allocator injection on stable Rust, had this crate needed an allocator. |

### Error and outcome design

Read while `Error`, `InvalidPlan` and the three head outcomes were designed. The learnings are written up separately; what follows is the sources.

| Project | License (SPDX) | Upstream | Read for |
| --- | --- | --- | --- |
| Boost.Outcome, including Outcome.Experimental and `status-code` | `Apache-2.0 OR BSL-1.0` | <https://github.com/ned14/outcome> | `status_code<D>` as `{ domain, value }` in two machine words, with all behaviour on a constexpr domain and no `std::string` anywhere. Two things came from it. First, the split this crate makes between an outcome and an error: a response the service is entitled to give (not found, precondition failed, range not satisfiable) is a variant of `GetHeadOutcome`, and `Err` is reserved for "this crate cannot proceed". Second, that a boundary needs stable numbers, which is why `InvalidPlan`, `FailureClass` and `ServiceErrorKind` are `#[repr(u16)]` with their discriminants written out. Its `indirecting_domain`, which boxes a payload too large to erase, has no counterpart: nothing here allocates on the error path either. |
| SNAFU | `MIT OR Apache-2.0` | <https://github.com/shepmaster/snafu> | The context-selector approach to Rust library errors, and its rules on error types being per unit of fallibility rather than per crate. Not used: SNAFU builds errors that own their context, and an error here must fit in a fixed size and hold no allocation. |

### Other

| Project | License (SPDX) | Upstream | Read for |
| --- | --- | --- | --- |
| gcode-rs | `MIT OR Apache-2.0` | <https://github.com/Michael-F-Bryan/gcode-rs> | A parser in a different domain under the same no-allocation rule, read as a comparison against this design. |
| TigerBeetle | `Apache-2.0` | <https://github.com/tigerbeetle/tigerbeetle> | Deciding every limit up front rather than growing on demand, which is also NASA's Power of Ten rule 3. |
