# Recorded write responses

The heads that `crates/object-storage-proto/tests/azure_put.rs` reads: a write that stored the object, a write that lost the race to create it, and a write whose condition did not hold.

Every file here is one response as the account sent it on Sat, 05 Sep 2026 09:49:23 GMT, under service version `2026-04-06`. `tests/azure-record` seeded the objects, sent the request and wrote what came back: the status line, the headers in the order they arrived, a blank line, and the body, byte-order mark included and to the last byte. A body that arrived in chunks is joined; the header that records the framing is kept as it arrived. Nothing in them is a secret. A request identifier names a request that is over, and the accounts hold nothing but this suite's own keys.

Do not edit these files. `docs/AZURE-FIXTURES.md` says how to record them again.

| file | request | account | identity | what it shows |
|---|---|---|---|---|
| `put-created.http` | `PUT /borink-object-storage/fixtures/write/object.txt` | `borinkstoragetest` | container-scoped | a stored object: `201`, with the entity tag and last-modified it now has, and, on an account that keeps versions, the version this write made |
| `put-created-empty.http` | `PUT /borink-object-storage/fixtures/write/empty.bin` | `borinkstoragetest` | container-scoped | an object of no bytes, which is an object: the same `201`, under a stated length of zero |
| `put-lost-the-race-to-create.http` | `PUT /borink-object-storage/fixtures/write/object.txt` | `borinkstoragetest` | container-scoped | a write under `If-None-Match: *` to a key that already holds something: `409 BlobAlreadyExists` |
| `put-precondition-failed.http` | `PUT /borink-object-storage/fixtures/write/object.txt` | `borinkstoragetest` | container-scoped | a write under `If-Match` with an entity tag the object does not have: `412 ConditionNotMet` |
| `put-container-missing.http` | `PUT /no-such-container/object.txt` | `borinkstoragetest` | account-scoped | a write addressed to a container that is not there, by an identity that may write anywhere in the account: `404 ContainerNotFound`. The grant is settled first and this one covers the container, so the service goes on to look for it |
| `put-refused.http` | `PUT /no-such-container/object.txt` | `borinkstoragetest` | container-scoped | a write the identity is not allowed to make: `403 AuthorizationPermissionMismatch`. Its writing role covers one container and this request names another, and the service settles that before it looks for the container, so the answer says nothing about whether the container is there. A read of the very same container does say |
