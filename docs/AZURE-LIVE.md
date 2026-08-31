# Azure live tests

The manual workflow uses a short-lived GitHub OIDC token; no Azure credential is stored in GitHub.

## Azure

`az ad app create` and `az ad sp create` created `borink-object-storage-github-read` (`36916623-6d73-4698-9944-8efcb70537ec`; service-principal object ID `bdc02a8f-4712-41fa-8103-69a1a5aaf96b`).
`az ad app federated-credential create` added issuer `https://token.actions.githubusercontent.com`, audience `api://AzureADTokenExchange`, and the immutable-ID subject emitted by GitHub: `repo:borink-org@319807983/object-storage@1342734194:environment:azure-live`.
`az role assignment create` granted `Storage Blob Data Reader` only on `/subscriptions/f6706ae1-d259-498d-8302-cacf4634d368/resourceGroups/borink-storage-test/providers/Microsoft.Storage/storageAccounts/borinkstoragetest/blobServices/default/containers/borink-object-test`.
The `Borink Infra` identity (`infra@borink.com`; object ID `9ec695e1-f992-4295-9281-2755486d8772`) has container-scoped `Storage Blob Data Contributor` for local tests. Its subscription `Owner` role covers the management plane but does not grant blob data-plane access.

## GitHub

`gh api --method PUT repos/borink-org/object-storage/environments/azure-live` created the environment.
`gh variable set --env azure-live --repo borink-org/object-storage` set `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, and `AZURE_SUBSCRIPTION_ID`; these identifiers are not secrets.

The checked fixture is `borink-object-storage/azure-get-reference/a key+é.txt`, with the exact body `0123456789-azure-get-reference`.

With Azure CLI 2.88.0, storage account keys are under `keys[]`; use `--query 'keys[0].value'`, not `[0].value`.

## Running them

The workflow runs on every pull request and on demand, and the `azure-live` environment requires `tiptenbrink` to approve each run before it holds a token. Nothing reaches the storage account because someone opened a pull request; until the run is approved the check is pending, and a pending check is what stops the merge. `gh api --method PUT repos/borink-org/object-storage/environments/azure-live` set that reviewer.

A pull request from a fork is given no `id-token` permission however it is approved, so the job fails there rather than passing untested. Push the branch to this repository to have it run.

`cargo test -p azure-live -- --ignored --test-threads=1` runs the same suite locally against the `Borink Infra` identity.

The suite is serial by design: the write tests overwrite one key, and the listing tests empty one prefix, so two of them at once would read what the other wrote.

The listing tests own everything under `AZURE_LIST_PREFIX` and delete it before each test. `borink-object-storage/azure-list-scratch/` holds nothing else.
