# borink-object-storage-cxx

The C++ binding for [`borink-object-storage-proto`](../object-storage-proto):
a `cxx` bridge that builds HTTP request heads and reads HTTP response heads,
and a header of helpers over it. It opens no socket, reads no clock and
allocates once per session and never per request. Your application keeps its
HTTP client, its buffers and its memory budget.

No call returns a `Result` and no call throws, so a program built without
exceptions can use it: a failure arrives as a `Status`, which is two numbers
the core crate names, and `describe_status` writes the sentence for a pair.

## Linking it

```cmake
add_subdirectory(path/to/crates/object-storage-cxx object-storage-cxx)
target_link_libraries(your_target PRIVATE borink::object_storage)
```

`FetchContent` works the same way. The target carries three things: the static
archive that cargo builds, the declarations that `cxx` generates
(`borink-object-storage-cxx/src/lib.rs.h`), and `include/borink/object_storage.hpp`.
Building it needs a C++23 compiler and cargo, and nothing else — an HTTP
library is the host's business, not this crate's.

```cpp
#include "borink/object_storage.hpp"
```

## What is in the header

`whole()`, `bounded()` and `from()` name a byte range; `Read`, `Write` and
`Removal` are the shape of one request; `as_bytes`, `into` and `borrow` hand a
buffer to the bridge; `bytes_of` and `text_of` read a value the response head
may not have carried; `describe_into` writes what an outcome or a status says
into room you supply. All of it is inline, holds no state and allocates only
where it is documented to.

## A request, in four steps

1. `open_session` once for one container, and keep the `Session`.
2. Describe the request as a `GetShapeView`, `PutShapeView` or
   `DeleteShapeView`, and keep it while the request is in flight.
3. `encode_get` (or `encode_put`, `encode_delete`) writes the request head
   into your buffer and names each part of it by offset. Send them.
4. Name each response header with a `HeaderRef` and call `accept_get_head`
   with the same shape. The `Outcome` says what to do with the body.

The crate documentation has the whole story: `cargo doc --open --package
borink-object-storage-cxx`. [`hosts/cxx-curl`](../../hosts/cxx-curl) is a host
written against libcurl, and is what a program using this crate looks like.
