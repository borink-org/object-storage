# Azure live tests

The manual workflow uses a short-lived GitHub OIDC token; no Azure credential is stored in GitHub.

## Azure

`az ad app create` and `az ad sp create` created `borink-object-storage-github-read` (`36916623-6d73-4698-9944-8efcb70537ec`; service-principal object ID `bdc02a8f-4712-41fa-8103-69a1a5aaf96b`).
`az ad app federated-credential create` added issuer `https://token.actions.githubusercontent.com`, audience `api://AzureADTokenExchange`, and the immutable-ID subject emitted by GitHub: `repo:borink-org@319807983/object-storage@1342734194:environment:azure-live`.
`az role assignment create` granted `Storage Blob Data Reader` and `Storage Blob Data Contributor` on `/subscriptions/f6706ae1-d259-498d-8302-cacf4634d368/resourceGroups/borink-storage-test/providers/Microsoft.Storage/storageAccounts/borinkstoragetest/blobServices/default/containers/borink-object-test`.

A second `Storage Blob Data Reader`, on the enclosing `blobServices/default`, is what lets a listing of a container that does not exist answer 404 rather than 403; see below. It widens reading to every container in the account and leaves writing scoped to the one. The account holds no other container.
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

## What the suite measured

Azure conditions a request on an entity tag written the way a listing writes it, without quotes, exactly as it does on the quoted form. `layered::quoted_etag` is therefore not a workaround for a service that refuses the listed form; it writes the spelling HTTP defines. `an_entity_tag_from_a_listing_conditions_a_read_quoted_or_not` holds that, and also holds the part that would change the answer: an unquoted tag that does not match must still refuse the read, because a service that discarded the header instead would leave the condition with no effect at all.

A listing of a container that is not there answers 404 `ContainerNotFound` only when the grant encloses the container being named. A credential scoped to one container is refused with 403 first, before Azure says whether any other container exists — deliberate, since a 404 there would tell an unauthorized caller which containers are real. That is why the read grant sits on `blobServices/default`: `listing_a_container_that_is_not_there_reports_that` cannot observe the 404 otherwise, and says so if it sees the 403 again.

## What the suite is still measuring

Three questions about object keys are open, and the tests that settle them assert the answer this crate currently assumes, so a failure names the correction rather than just failing.

`the_length_this_crate_allows_is_a_length_azure_allows` asks what unit Azure counts a key's 1024 characters in. This crate counts Unicode scalar values; Azure documents "characters" without saying which. Two probes separate the candidates: 1024 `é` is 1024 scalar values and 1024 UTF-16 code units but ~2007 bytes, while 500 `🦀` is 541 scalar values but 1041 UTF-16 code units. If Azure refuses the second alone it counts UTF-16 code units; if it refuses both it counts bytes; and `validate_put` should then count the same unit, because `InvalidPlan::Key` claims a key that passes it can become a request.

`a_key_of_many_segments_is_refused_where_azure_says_it_is` pins the documented maximum of 254 `/`-delimited segments, which this crate does not check at all.

`a_key_that_leans_on_a_slash_is_stored_under_the_name_it_was_given` asks what becomes of the slashes this crate deliberately leaves literal, so that a hierarchical-namespace path works. A trailing separator, a doubled one, a trailing dot and a `..` between separators are each written and then listed: the point is not whether Azure accepts them but whether it stores them under the name it was given, since a folded name would leave a caller addressing an object that is not there. A `..` is also the case a URL library may resolve before the request is sent, so a failure there may be the host rather than the service.
