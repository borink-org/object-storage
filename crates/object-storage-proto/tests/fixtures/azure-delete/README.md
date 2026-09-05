# Recorded removal responses

The heads that `crates/object-storage-proto/tests/azure_delete.rs` reads: a removal the service accepted, one of a key that held nothing, one whose condition did not hold, and one the service refused because the object had snapshots that the plan did not name.

Every file here is one response as the account sent it on Sat, 05 Sep 2026 11:30:54 GMT, under service version `2026-04-06`. `tests/azure-record` seeded the objects, sent the request and wrote what came back: the status line, the headers in the order they arrived, a blank line, and the body, byte-order mark included and to the last byte. A body that arrived in chunks is joined; the header that records the framing is kept as it arrived. Nothing in them is a secret. A request identifier names a request that is over, and the accounts hold nothing but this suite's own keys.

Do not edit these files. `docs/AZURE-TESTING.md` says how to record them again.

| file | request | account | identity | what it shows |
|---|---|---|---|---|
| `delete-accepted.http` | `DELETE /borink-object-storage/fixtures/remove/object.txt` | `borinkstoragetest` | container-scoped | an accepted removal: `202`, and no metadata, because the object is gone |
| `delete-missing.http` | `DELETE /borink-object-storage/fixtures/remove/object.txt` | `borinkstoragetest` | container-scoped | a removal of a key that holds nothing: `404 BlobNotFound`, an outcome rather than a fault |
| `delete-precondition-failed.http` | `DELETE /borink-object-storage/fixtures/remove/object.txt` | `borinkstoragetest` | container-scoped | a removal under `If-Match` with an entity tag the object does not have: `412 ConditionNotMet` |
| `delete-refused-for-snapshots.http` | `DELETE /borink-object-storage/fixtures/remove/object.txt` | `borinkstoragetest` | container-scoped | a removal naming the object alone, of an object that has snapshots: `409 SnapshotsPresent`. A plan that does not say what it takes with it does not take them |
| `delete-accepted-with-snapshots.http` | `DELETE /borink-object-storage/fixtures/remove/object.txt` | `borinkstoragetest` | container-scoped | the same removal, naming the snapshots as well: `202`, the same answer as any other accepted removal |
| `delete-container-missing.http` | `DELETE /no-such-container/object.txt` | `borinkstoragetest` | account-scoped | a removal addressed to a container that is not there, by an identity that may write anywhere in the account: `404 ContainerNotFound`, the same answer as the write and the read of that container |
| `delete-refused.http` | `DELETE /no-such-container/object.txt` | `borinkstoragetest` | container-scoped | a removal the identity is not allowed to make: `403 AuthorizationPermissionMismatch`. A removal needs the writing role, which covers one container here, so it is refused where a read of the same container answers `404 ContainerNotFound` under the wider reading role |
