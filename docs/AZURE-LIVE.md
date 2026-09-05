# Azure live tests

The workflow authenticates with a short-lived GitHub OIDC token. No Azure credential is stored in GitHub.

## Azure setup

Two storage accounts in resource group `borink-storage-test`, both with container `borink-object-test`: `borinkstoragetest` (flat namespace) and `borinkstoragehnstest` (hierarchical namespace, HNS). The suite runs against both because they behave differently.

Service principal `borink-object-storage-github-read` (app `36916623-6d73-4698-9944-8efcb70537ec`, object ID `bdc02a8f-4712-41fa-8103-69a1a5aaf96b`), created with `az ad app create` and `az ad sp create`. Its federated credential (`az ad app federated-credential create`) has issuer `https://token.actions.githubusercontent.com`, audience `api://AzureADTokenExchange` and subject `repo:borink-org@319807983/object-storage@1342734194:environment:azure-live` (the immutable-ID form GitHub emits).

Roles on each account, assigned with `az role assignment create`:

```
SCOPE=/subscriptions/f6706ae1-d259-498d-8302-cacf4634d368/resourceGroups/borink-storage-test/providers/Microsoft.Storage/storageAccounts/<account>
az role assignment create --assignee-object-id bdc02a8f-4712-41fa-8103-69a1a5aaf96b --assignee-principal-type ServicePrincipal \
  --role "Storage Blob Data Contributor" --scope "$SCOPE/blobServices/default/containers/borink-object-test"
az role assignment create --assignee-object-id bdc02a8f-4712-41fa-8103-69a1a5aaf96b --assignee-principal-type ServicePrincipal \
  --role "Storage Blob Data Reader" --scope "$SCOPE/blobServices/default"
```

The reader role is on the whole blob service, not the container, so that listing a container that does not exist answers 404 instead of 403. Writing stays scoped to the one container. A new assignment takes about a minute to propagate.

The account firewall must allow requests by default (`az storage account update -n <account> -g borink-storage-test --default-action Allow`). The HNS account was initially set to deny by default with one allowed IP, which made every CI request fail with `403 AuthorizationFailure` (an RBAC refusal is `403 AuthorizationPermissionMismatch` instead).

The read reference blob `borink-object-storage/azure-get-reference/a key+é.txt` must exist on both accounts with the exact body `0123456789-azure-get-reference`:

```
az storage blob upload --account-name <account> --container-name borink-object-test --auth-mode login \
  --name 'borink-object-storage/azure-get-reference/a key+é.txt' --file ref.txt
```

For local runs, the `Borink Infra` identity (`infra@borink.com`, object ID `9ec695e1-f992-4295-9281-2755486d8772`) has container-scoped `Storage Blob Data Contributor`. Its subscription `Owner` role does not grant blob data-plane access.

With Azure CLI 2.88.0, storage account keys are under `keys[]`: use `--query 'keys[0].value'`.

## GitHub setup

`gh api --method PUT repos/borink-org/object-storage/environments/azure-live` created the `azure-live` environment and set `tiptenbrink` as its required reviewer. `gh variable set --env azure-live --repo borink-org/object-storage` set `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` and `AZURE_SUBSCRIPTION_ID` (not secrets).

## Running the suite

The workflow runs on every pull request and on demand, once a reviewer approves the run. Until then the check is pending, which blocks the merge. A pull request from a fork never gets the `id-token` permission, so push the branch to this repository to run it.

Locally:

```
cargo test -p azure-live -- --ignored --test-threads=1
```

with `AZURE_STORAGE_ENDPOINT`, `AZURE_STORAGE_CONTAINER`, `AZURE_BLOB_KEY`, `AZURE_PUT_KEY`, `AZURE_LIST_PREFIX`, `AZURE_MULTIPART_PREFIX` and `AZURE_STORAGE_ACCESS_TOKEN` set as in the workflow, plus `AZURE_HIERARCHICAL=1` for the HNS account (only the exact value `1` counts). Under the container-scoped local identity, `listing_a_container_that_is_not_there_reports_that` fails with 403 on the flat account.

The tests must run serially: they share one write key and empty their listing and multipart prefixes before each test.

## Writing a probe

`addressable` refuses keys that Azure would rename or reject, so a probe that measures such a limit cannot send the key through this crate. Write the request by hand, as `raw_put`, `raw_delete` and `snapshot` do, and remove what you wrote the same way. When you add a rule to `addressable`, move the probe that measured it in the same commit.

## What the suite measured

Each item is checked by a test in `tests/azure-live/tests/live.rs`, which fails with an explanation if Azure changes its behaviour.

Conditions and listings:

- An entity tag from a listing (unquoted) works in `If-Match` exactly like the quoted form, and a wrong unquoted tag still refuses the read. `layered::quoted_etag` is a convenience, not a workaround.
- Listing a container that does not exist answers 404 `ContainerNotFound` only if the grant covers the whole blob service. A container-scoped grant gets 403 first.
- An invalid marker is `400 InvalidQueryParameterValue`, a truncated one `400 InvalidInput`. A marker names a place in the container, not a session, and can be reused.
- `max_results` above 5000 is answered with 5000 and a marker, not refused.
- A listing body is always UTF-8. A key with an invalid byte is refused with 400, and an invalid byte in a query value is echoed as U+FFFD. A control character in a query value is `400 InvalidQueryParameterValue`. This is why the reader may refuse a body that is not UTF-8.
- A group of keys in a delimited listing is a `<BlobPrefix>` with a `<Name>` and nothing else on the flat account. On the HNS account it also holds the directory's own `<Properties>` block: creation time, last-modified, entity tag, `<ResourceType>directory</ResourceType>` and a zero length. The reader takes the name alone; the rest is reachable through `ListEntry::raw`. Both responses are recorded in `crates/object-storage-proto/tests/fixtures/azure-listing/`.

Object keys:

- Length is counted in UTF-16 code units, limit 1024. `validate_put` counts the same way.
- ASCII control characters (`U+0000`–`U+001F`, `U+007F`) are refused with 400. `U+0085` is accepted. `U+FFFE` and `U+FFFF` are stored, and a listing then writes the name percent-encoded with `Encoded="true"`, whole, separators included. So every `%` in an encoded name begins an escape.
- A dot at the end of any segment is dropped (`dotseg./x` becomes `dotseg/x`), and `.`/`..` segments are resolved by the HTTP client before sending. `addressable` refuses both.
- A trailing, doubled or space-preceded separator, a dot inside a segment and leading dots survive unchanged on the flat account.
- Path segment limit is 255 on the flat account (documented as 254) and 61 on the HNS account, both found by bisection. `addressable` enforces 255.

HNS account differences:

- An undelimited listing reports a directory as an `EntryKind::Directory` entry with no length and no trailing separator. A delimited listing reports it as a prefix with the separator, on both accounts.
- An empty path segment is removed: `double//slash` is stored as `double/slash`. `addressable` does not cover this yet, since the crate is not told which kind of account it talks to.
- No snapshots: `?comp=snapshot` is `409 FeatureNotYetSupportedForHierarchicalNamespaceAccounts`. `x-ms-delete-snapshots` on a delete is still accepted.
- A directory that still holds anything cannot be deleted (409), so the prefix-emptying helper deletes the longest key first.

Multipart (`Put Block`, `Put Block List`, `Get Block List`; not supported by this crate yet, responses recorded in `crates/object-storage-proto/tests/fixtures/azure-multipart/`):

- A committed block list of a key with only staged blocks is 200 with an empty `<CommittedBlocks />` and no entity tag, not 404. Only a key with nothing at all is 404 `BlobNotFound`.
- A commit that loses the race to create is `409 BlobAlreadyExists`, like `Put Blob`, not the documented 412. A stale `If-Match` is `412 ConditionNotMet`.
- Block identifiers of different decoded lengths are refused when staged (`400 InvalidBlobOrBlock`). A commit naming an unstaged block is `400 InvalidBlockList`. An empty block is `400 InvalidHeaderValue`.
- A commit naming no blocks creates an empty object.
- `Get Block List` orders blocks by identifier, not staging order. Staging one identifier twice keeps the last content. A `Put Block` answers with a CRC64 and no entity tag, last-modified or MD5.
- Staged blocks are invisible to reads until committed, and a whole-object write to the key discards them. There is no abort operation and none is needed.
