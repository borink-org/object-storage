# Recorded listing responses

Each file is one delimited `List Blobs` response as the service sent it on 2026-09-05, under service version `2026-04-06`, for the three objects the live suite seeds under its listing prefix: `a.txt`, `b.txt` and `nested/c.txt`, listed with `delimiter=/`. `a_group_of_keys_carries_what_the_account_keeps_for_it` in `tests/azure-live/tests/live.rs` printed them and they are pasted verbatim: the status line, the headers in the order they arrived, a blank line, and the body, byte-order mark included. Nothing in them is a secret.

Nothing reads these files. They record what the two kinds of account write for a group of keys, so that a change in either shows up as a diff.

| file | what it shows |
|---|---|
| `list-blobs-delimited-flat.http` | `borinkstoragetest`: a `<BlobPrefix>` holds a `<Name>` and nothing else. Each `<Blob>` carries `<VersionId>` and `<IsCurrentVersion>` beside its properties, because the account keeps versions. |
| `list-blobs-delimited-hierarchical.http` | `borinkstoragehnstest`: a `<BlobPrefix>` holds a `<Properties>` block as well, the same one an undelimited listing gives the directory as a `<Blob>`: its own creation time, last-modified and entity tag, `<ResourceType>directory</ResourceType>`, a zero length and empty content headers. Each `<Blob>` carries `<ResourceType>file</ResourceType>`. |
