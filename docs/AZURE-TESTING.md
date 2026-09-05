# Testing against Azure

Everything that claims to know what Azure sends is checked against bytes Azure sent. Those bytes are recorded once, committed, and read offline; the service is asked again only on purpose. This document says which tests there are, what each one is allowed to assume, how the recorded corpus is made, and how the accounts and identities behind it are set up.

## The layers

From the offline to the live, each layer asks one question.

**Unit tests in the crate**, `#[cfg(test)]` in `crates/object-storage-proto/src/`. Pure functions: date arithmetic, percent-decoding, the XML scanner, error-code discriminants. No I/O and no fixtures.

**Offline tests on recorded responses**, `crates/object-storage-proto/tests/`. Where a test says what Azure answers, it reads a response from `tests/fixtures/` and asserts against those bytes. `azure_list.rs`, `azure_responses.rs`, `azure_put.rs` and `azure_delete.rs` are of this kind; `azure_get.rs` and `azure_head_offsets.rs` assert what this crate encodes, which needs no response. `tests/recorded/mod.rs` reads a file into a status, the head this crate keeps, and a fresh copy of the body.

Values that change with every recording, an entity tag, a last-modified, a version, a marker, a request identifier, are not asserted by value. A test asserts that the reader returned what the document holds and that a date parses. What is asserted by value is what the recorder seeded: the keys, the lengths, the kinds and the codes.

**Offline tests on documents written by hand**, `azure_list_grammar.rs` and a few places in `azure_responses.rs`. A reader also has to refuse what no service sends, and take shapes the service has stopped sending. Those documents cannot be recorded, so a test writes one out and says above it why the service will not send one: an arithmetic that has to be refused, a spelling only a hand-written document has, or a head the service no longer writes. Every truncation of a page and every single-byte change to one are the two sweeps in the grammar file. This is the one place a hand-written document is allowed, and each one carries its reason.

**The corpus agrees with itself**, `azure_corpus.rs`. Every file in a group parses, names the service version this crate asks for, has a row in the group's notes, and is read by some test. The one group nothing reads, `azure-multipart`, is exempt only while its notes say so. This runs with every `cargo test`, so drift between the files, the notes and the tests is caught offline rather than the next time someone records.

**Host loopback tests**, `hosts/ureq/tests/`. A `TcpListener` on `127.0.0.1` reads the request the host puts on the wire and answers with a recorded response. This is the only place the request head is checked as an HTTP message. The request line asserted is the one the corpus notes say produced that response.

**Link and ABI checks**: `checks/no-allocator` links the crate with no global allocator, `checks/freestanding` links a bare-metal image, and `crates/object-storage-c` is tested from C and C++. None of them touch Azure.

**The live suite**, `tests/azure-live/`, every test `#[ignore]`. It runs the real code against the real accounts and asks whether Azure still behaves as the crate and the corpus say. It is optional: run it when a rule about the service is added or doubted, and in CI once a reviewer approves. A live test that fails with an explanation is the service having changed, and the recorder is how the change gets into the corpus.

`cargo test --workspace` runs everything but the live suite, with no credentials and no network.

## The recorded corpus

`crates/object-storage-proto/tests/fixtures/` holds one file per response, in groups: `azure-listing`, `azure-get`, `azure-put`, `azure-delete`, `azure-multipart`. `tests/azure-record` writes all of them in one run. It empties its prefix on both accounts, seeds the objects that provoke each response, sends the requests, writes what came back, empties the prefix again, and writes a `README.md` in each group naming every file, the request that produced it, the account that answered, the identity that sent it and what the response shows. Do not edit the files or the notes; record them again.

A file is the status line as it arrived, every header in arrival order with its name lower-cased, a blank line, and the body to the last byte. A body that arrived in chunks is joined; the header that records the framing stays as it arrived. `tests/support/src/recorded.rs` spells the format once, for the recorder that writes it and the tests that read it.

Nothing in a file is a secret. A request identifier names a request that is over, and the accounts hold nothing but the suites' own keys. The two `*-unauthenticated.http` files carry the tenant's authorization URI, which the service tells any client that asks it badly.

A request the crate encodes is built through the crate, so the recorded response answers a request this crate actually produces. `Account::raw` in the recorder writes one by hand, for an operation the crate does not encode or a key `addressable` refuses.

### Recording again

```
tests/azure-record/run.sh
```

It takes about twenty seconds, then prints the diff summary. A run that changes nothing but the dates, the entity tags, the versions and the request identifiers is the service answering as it did before. Anything else is the thing worth looking at, and worth saying in the commit message.

It records from both accounts in one run because a group holds files from each and the notes name every file in the group. Its prefix, `borink-object-storage/fixtures/`, is a constant: a recorded name is part of the response and so part of what a test reads back, and a run under another prefix would rewrite every file for no reason.

### Adding a response

`tests/azure-record/src/corpus.rs` is the index of the corpus. Every recorded response is one `session.capture` call, and above it stands the state it was recorded against. Add the seeds, add the call, say in `shows` what the response demonstrates rather than what the request asked for, and record again. Then read the new file from a test, or `azure_corpus.rs` will say that nothing does.

### What cannot be recorded

A response the service no longer sends. `azure_responses.rs` writes out a `304` carrying an entity tag, because the recorded one carries none, and a head that leaves the error code out, because the service names it in a header on every refusal. Both are shapes a reader still has to take.

A page a small container cannot hold. An empty page that still names a next one needs a page whose keys were all filtered out of it, which takes more keys than this corpus seeds.

## The accounts and the identities

Two storage accounts in resource group `borink-storage-test`, each with the container `borink-object-test`: `borinkstoragetest` has a flat namespace and keeps versions, `borinkstoragehnstest` has a hierarchical namespace. The suites run against both because the two behave differently: a hierarchical account lists its directories as entries, drops an empty path segment, takes a quarter of the path segments, and has no snapshots.

`tests/support/src/azure.rs` names the accounts, the container and the two prefixes, `borink-object-storage/live/` for the live suite and `borink-object-storage/fixtures/` for the recorder, and reads the environment every suite shares:

| variable | meaning |
|---|---|
| `AZURE_STORAGE_ACCESS_TOKEN` | a blob data-plane token; `tests/azure-setup/token.sh` prints one |
| `AZURE_HIERARCHICAL` | exactly `1` makes the live suite run against the hierarchical account |
| `AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT` | the recorder's second token; `token.sh account` prints it |
| `AZURE_FLAT_ENDPOINT`, `AZURE_HIERARCHICAL_ENDPOINT`, `AZURE_STORAGE_CONTAINER` | other accounts or another container, for whoever wants them |

The live suite runs against one account per run and the recorder against both per run. That is the one place the two differ, and it follows from what each writes: a live run is one job in CI, one account each so the two run at once, while a recording is one corpus, whose notes name files from both accounts.

Three service principals, all in `tests/azure-setup/identities.env` with their identifiers:

- `borink-object-storage-github-live` is the workflow's identity. It signs in with a federated credential for the `azure-live` environment of this repository, so no Azure secret is stored in GitHub. It holds `Storage Blob Data Contributor` on the container and `Storage Blob Data Reader` on the whole blob service of each account. The wider read is what lets a listing of a container that does not exist answer `404 ContainerNotFound`: a container-scoped grant is refused with 403 before the service says whether any other container is there.
- `borink-object-storage-fixtures` is the recorder's identity, with the same two roles. It signs in with a client secret, because it runs from a workstation. Running the live suite locally under it sees exactly what CI sees, the 404 included.
- `borink-object-storage-fixtures-account` holds `Storage Blob Data Contributor` on the whole blob service of each account. Azure settles the grant before it looks for the container, so a write to a container that is not there is `403 AuthorizationPermissionMismatch` under the container-scoped identity and `404 ContainerNotFound` under this one. The corpus records both, and this identity records nothing else. Measured on 5 September 2026 for `Put Blob`, `Delete Blob` and `Get Blob`: the read is 404 under both, because both may read across the account.

The `Borink Infra` user identity has container-scoped `Storage Blob Data Contributor` on both accounts. Its subscription `Owner` role covers the management plane and grants no blob data-plane access, and under it the container-not-there listing test fails with 403. Use `token.sh` for local runs instead.

### Setting it all up

```
tests/azure-setup/setup.sh --check
tests/azure-setup/setup.sh
```

Signed in to `az` as an owner of the subscription and to `gh` as an administrator of the repository, `setup.sh` makes whatever is missing and changes nothing that is in place: the resource group; the two accounts, with the firewall answering requests from anywhere and versioning on the flat one; the container on each; the three applications and their service principals; every role assignment above; the federated credential, whose subject is GitHub's immutable-identifier form so a renamed repository keeps signing in; a client secret for each recorder identity, written to `~/.config/borink/azure-fixtures.secret` and `~/.config/borink/azure-fixtures-account.secret`, readable by you alone and never printed; and the `azure-live` environment with `tiptenbrink` as its required reviewer. An identifier it creates is written back into `identities.env`. `--check` reports instead of creating and exits 1 if anything is missing. A new role assignment takes about a minute to propagate.

The firewall matters: the hierarchical account was once set to deny by default with one allowed IP, and every CI request failed with `403 AuthorizationFailure`, which is not the `403 AuthorizationPermissionMismatch` of a missing grant.

### Tokens

```
tests/azure-setup/token.sh            # the container-scoped identity
tests/azure-setup/token.sh account    # the account-scoped identity
```

Each exchanges the client secret in `~/.config/borink/` for a data-plane token good for about an hour. The secret never becomes an environment variable and your own `az` login is left alone. In GitHub Actions, where there is no secret, `token.sh` exchanges the job's OIDC token for a token of the workflow identity instead. Both `run.sh` scripts call it when `AZURE_STORAGE_ACCESS_TOKEN` is not set.

## Running the live suite

```
tests/azure-live/run.sh                        # both accounts
tests/azure-live/run.sh hierarchical           # one of them
tests/azure-live/run.sh flat -- lists_         # a filter for the test harness
```

The workflow in `.github/workflows/azure-get-live.yml` runs that script and nothing else, one job per account, once `tiptenbrink` approves the run. Until then the check is pending, which blocks the merge. A pull request from a fork never gets the `id-token` permission, so push the branch to this repository to run it.

Every test owns its keys under a segment named after it, so the tests of one account run at once and a run takes as long as its slowest test rather than the sum. The read reference is the one shared object; the suite writes it the first time a run reads it, so a container that holds nothing is enough to run against. Locally `run.sh` runs the two accounts side by side as well.

### Writing a probe

`addressable` refuses keys that Azure would rename or reject, so a probe that measures such a limit cannot send the key through this crate. Write the request by hand, as `raw_put`, `raw_delete` and `snapshot` do, and remove what you wrote the same way. When you add a rule to `addressable`, move the probe that measured it in the same commit. What a probe measures that the corpus should hold goes into `corpus.rs`, so the offline tests read the bytes rather than the probe's memory of them.

### What the suite measured

Each item is checked by a test in `tests/azure-live/tests/live.rs`, which fails with an explanation if Azure changes its behaviour.

Conditions and listings:

- An entity tag from a listing (unquoted) works in `If-Match` exactly like the quoted form, and a wrong unquoted tag still refuses the read. `layered::quoted_etag` is a convenience, not a workaround.
- Listing a container that does not exist answers 404 `ContainerNotFound` only if the grant covers the whole blob service. A container-scoped grant gets 403 first.
- An invalid marker is `400 InvalidQueryParameterValue`, a truncated one `400 InvalidInput`. A marker names a place in the container, not a session, and can be reused.
- `max_results` above 5000 is answered with 5000 and a marker, not refused.
- A listing body is always UTF-8. A key with an invalid byte is refused with 400, and an invalid byte in a query value is echoed as U+FFFD. A control character in a query value is `400 InvalidQueryParameterValue`. This is why the reader may refuse a body that is not UTF-8.
- A group of keys in a delimited listing is a `<BlobPrefix>` with a `<Name>` and nothing else on the flat account. On the hierarchical account it also holds the directory's own `<Properties>` block: creation time, last-modified, entity tag, `<ResourceType>directory</ResourceType>` and a zero length. The reader takes the name alone; the rest is reachable through the entry's properties. Both pages are recorded in `azure-listing/`.

Object keys:

- Length is counted in UTF-16 code units, limit 1024. `validate_put` counts the same way.
- ASCII control characters (`U+0000`–`U+001F`, `U+007F`) are refused with 400. `U+0085` is accepted. `U+FFFE` and `U+FFFF` are stored, and a listing then writes the name percent-encoded with `Encoded="true"`, whole, separators included. So every `%` in an encoded name begins an escape.
- A dot at the end of any segment is dropped (`dotseg./x` becomes `dotseg/x`), and `.`/`..` segments are resolved by the HTTP client before sending. `addressable` refuses both.
- A trailing, doubled or space-preceded separator, a dot inside a segment and leading dots survive unchanged on the flat account.
- Path segment limit is 255 on the flat account (documented as 254) and 61 on the hierarchical account, both found by bisection. `addressable` enforces 255.

Hierarchical account differences:

- An undelimited listing reports a directory as an `EntryKind::Directory` entry with no length and no trailing separator. A delimited listing reports it as a prefix with the separator, on both accounts.
- An empty path segment is removed: `double//slash` is stored as `double/slash`. `addressable` does not cover this yet, since the crate is not told which kind of account it talks to.
- No snapshots: `?comp=snapshot` is `409 FeatureNotYetSupportedForHierarchicalNamespaceAccounts`. `x-ms-delete-snapshots` on a delete is still accepted.
- A directory that still holds anything cannot be deleted (409). The account's DFS endpoint removes a directory and everything under it with one request (`DELETE ...dfs.core.windows.net/container/dir?recursive=true`), which is how the live suite empties a prefix there; the recorder, which leaves only a few keys behind, deletes the longest key first instead.

Multipart (`Put Block`, `Put Block List`, `Get Block List`; not supported by this crate yet, responses recorded in `azure-multipart/`):

- A committed block list of a key with only staged blocks is 200 with an empty `<CommittedBlocks />` and no entity tag, not 404. Only a key with nothing at all is 404 `BlobNotFound`.
- A commit that loses the race to create is `409 BlobAlreadyExists`, like `Put Blob`, not the documented 412. A stale `If-Match` is `412 ConditionNotMet`.
- Block identifiers of different decoded lengths are refused when staged (`400 InvalidBlobOrBlock`). A commit naming an unstaged block is `400 InvalidBlockList`. An empty block is `400 InvalidHeaderValue`.
- A commit naming no blocks creates an empty object.
- `Get Block List` orders blocks by identifier, not staging order. Staging one identifier twice keeps the last content. A `Put Block` answers with a CRC64 and no entity tag, last-modified or MD5.
- Staged blocks are invisible to reads until committed, and a whole-object write to the key discards them. There is no abort operation and none is needed.

## Notes on the tools

With Azure CLI 2.88.0, storage account keys are under `keys[]`: use `--query 'keys[0].value'`. The suites never need a key; bearer tokens are enough for everything they do.
