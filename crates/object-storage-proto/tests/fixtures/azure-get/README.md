# Recorded read responses

The heads that `crates/object-storage-proto/tests/azure_responses.rs` reads: a whole object, a range, a range past the end of one, a condition that held and one that did not, and the statuses a read answers with when there is nothing to return.

Every file here is one response as the account sent it on Sat, 05 Sep 2026 10:56:18 GMT, under service version `2026-04-06`. `tests/azure-record` seeded the objects, sent the request and wrote what came back: the status line, the headers in the order they arrived, a blank line, and the body, byte-order mark included and to the last byte. A body that arrived in chunks is joined; the header that records the framing is kept as it arrived. Nothing in them is a secret. A request identifier names a request that is over, and the accounts hold nothing but this suite's own keys.

Do not edit these files. `docs/AZURE-TESTING.md` says how to record them again.

| file | request | account | identity | what it shows |
|---|---|---|---|---|
| `get-whole.http` | `GET /borink-object-storage/fixtures/read/object.txt` | `borinkstoragetest` | container-scoped | a whole read: the length, the entity tag, the last-modified and the encoding the object is stored under, which the reader carries rather than decodes |
| `head-metadata.http` | `HEAD /borink-object-storage/fixtures/read/object.txt` | `borinkstoragetest` | container-scoped | the same object asked for by its metadata alone: the head of a read, with no body to follow it |
| `get-range.http` | `GET /borink-object-storage/fixtures/read/object.txt` | `borinkstoragetest` | container-scoped | a bounded range: `Content-Range` states the bytes returned and the size of the object they came from |
| `get-range-past-the-end.http` | `GET /borink-object-storage/fixtures/read/object.txt` | `borinkstoragetest` | container-scoped | a range whose end is past the end of the object: the service answers with every byte it has from the start of the range, which is maximal satisfaction of the request |
| `get-range-not-satisfiable.http` | `GET /borink-object-storage/fixtures/read/object.txt` | `borinkstoragetest` | container-scoped | a range that starts past the end: `416`, with `bytes */N` naming the size of the object, which is the only place that size is stated |
| `get-not-modified.http` | `GET /borink-object-storage/fixtures/read/object.txt` | `borinkstoragetest` | container-scoped | a read under `If-None-Match` with the object's own entity tag: `304`, with the tag and no body |
| `get-precondition-failed.http` | `GET /borink-object-storage/fixtures/read/object.txt` | `borinkstoragetest` | container-scoped | a read under `If-Match` with an entity tag the object does not have: `412`, which names no error code |
| `get-missing.http` | `GET /borink-object-storage/fixtures/read/absent.txt` | `borinkstoragetest` | container-scoped | a read of a key that holds nothing: `404 BlobNotFound`, named in the head and repeated in the body |
| `get-container-missing.http` | `GET /no-such-container/object.txt` | `borinkstoragetest` | container-scoped | a read addressed to a container that is not there: `404 ContainerNotFound`, which names the container rather than the object, and is a different outcome from a key that holds nothing |
| `get-unauthenticated.http` | `GET /borink-object-storage/fixtures/read/object.txt` | `borinkstoragetest` | container-scoped | a read whose token is not a token at all: `401 InvalidAuthenticationInfo`, which is not the same answer as a token the service accepts and an identity it refuses |
