# Recorded multipart responses

Every file here is one response as `borinkstoragetest` sent it on 2026-09-02,
under service version `2026-04-06`. The probes in
`tests/azure-live/tests/live.rs` captured them and they are pasted verbatim:
the status line, the headers in the order they arrived, a blank line, and the
body, byte-order mark included. Nothing in them is a secret. A request
identifier names a request that is over, and the account holds nothing but
this suite's own keys.

Nothing reads these files yet. They are here so that the multipart types can
be written against what Azure actually sent, and so that a later service
version that answers differently shows up as a diff.
`snapshot-hierarchical-409.http` came from `borinkstoragehnstest`.

| file | what it shows |
|---|---|
| `put-block-201.http` | a staged block answers with a CRC64 and no entity tag, no last-modified and no MD5 |
| `put-block-empty-400.http` | an empty block is refused, and the refusal names `Content-Length` |
| `put-block-mixed-length-400.http` | identifiers of two decoded lengths are refused when staged, not when committed, with `InvalidBlobOrBlock` |
| `put-block-list-201.http` | the commit answers like a whole-object write: entity tag, last-modified, CRC64, and a version identifier if the account keeps versions |
| `put-block-list-lost-create-409.http` | a commit that loses the race to create is `409 BlobAlreadyExists`, like `Put Blob`, and not the documented 412 |
| `put-block-list-condition-412.http` | a stale `If-Match` on the commit is `412 ConditionNotMet` |
| `put-block-list-unstaged-400.http` | a commit that names a block nobody staged is `400 InvalidBlockList` |
| `get-block-list-uncommitted-only.http` | both sections, the empty one written `<CommittedBlocks />`, and blocks ordered by identifier rather than by when they were staged |
| `get-block-list-committed-empty.http` | a committed listing of a key that holds only staged blocks is a 200 with an empty section and no entity tag, not a 404 |
| `get-block-list-after-the-commit.http` | a committed listing describes the blob: entity tag, last-modified and `x-ms-blob-content-length` |
| `get-block-list-both-empty.http` | after a whole-object write both sections are empty |
| `get-block-list-escaped-identifier.http` | an identifier with `+`, `/` and `=` comes back unescaped in the document |
| `get-block-list-absent-404.http` | a key that holds nothing at all answers 404 `BlobNotFound` |
| `snapshot-hierarchical-409.http` | an account with a hierarchical namespace has no snapshots, and names the feature it refuses |
