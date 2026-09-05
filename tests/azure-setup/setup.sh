#!/usr/bin/env bash
# Puts everything the Azure suites need in place, and changes nothing that is.
#
#   setup.sh            create whatever is missing
#   setup.sh --check    say what is missing, create nothing, exit 1 if anything is
#
# What it makes, in order: the resource group; the two storage accounts, one
# with a hierarchical namespace, each with the one container the suites write
# in, an open firewall and, on the flat account, versioning; the three
# applications and their service principals; the role each one holds on each
# account; the federated credential the workflow signs in with; a client secret
# for each recorder identity, in `~/.config/borink/`; and the GitHub
# environment and its reviewer. `identities.env` names all of it, and an
# identifier that is made here is written back into that file.
#
# Run it signed in to `az` as an owner of the subscription and to `gh` as an
# administrator of the repository. Every step looks before it creates, so a
# run against a finished setup reports and does nothing. A new role assignment
# takes about a minute to propagate.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
identities=$here/identities.env
# shellcheck source=identities.env
source "$identities"

check=false
case "${1:-}" in
  "") ;;
  --check) check=true ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac
missing=0

# `need <what> <command...>`: reports `what` as missing and, unless checking,
# runs the command that creates it.
need() {
  local what=$1
  shift
  missing=$((missing + 1))
  if $check; then
    echo "missing:  $what"
  else
    echo "creating: $what"
    "$@"
  fi
}

have() {
  echo "in place: $1"
}

# Writes one identifier back into `identities.env`.
record() {
  local key=$1 value=$2
  sed -i "s|^$key=.*|$key=$value|" "$identities"
  printf -v "$key" '%s' "$value"
  echo "recorded: $key=$value"
}

scope_of() {
  echo "/subscriptions/$AZURE_SUBSCRIPTION_ID/resourceGroups/$AZURE_RESOURCE_GROUP/providers/Microsoft.Storage/storageAccounts/$1"
}

az account set --subscription "$AZURE_SUBSCRIPTION_ID"

# ---------------------------------------------------------- resource group

if az group show --name "$AZURE_RESOURCE_GROUP" --only-show-errors >/dev/null 2>&1; then
  have "resource group $AZURE_RESOURCE_GROUP"
else
  need "resource group $AZURE_RESOURCE_GROUP" \
    az group create --name "$AZURE_RESOURCE_GROUP" --location "$AZURE_LOCATION" --output none
fi

# -------------------------------------------------------- storage accounts

# `account <name> <hierarchical: true|false>`
account() {
  local name=$1 hierarchical=$2
  if az storage account show --name "$name" --resource-group "$AZURE_RESOURCE_GROUP" --only-show-errors >/dev/null 2>&1; then
    have "storage account $name"
    local hns
    hns=$(az storage account show --name "$name" --resource-group "$AZURE_RESOURCE_GROUP" --query 'isHnsEnabled' --output tsv)
    if [ "${hns,,}" != "$hierarchical" ] && ! { [ -z "$hns" ] && [ "$hierarchical" = false ]; }; then
      echo "wrong:    $name has a hierarchical namespace: ${hns:-false}, wanted $hierarchical; that cannot be changed, make another account" >&2
      exit 1
    fi
  else
    # No public access and no shared-key clients are needed; the suites use
    # bearer tokens. Shared-key access stays on, because that is what the
    # portal and `az storage` themselves default to.
    need "storage account $name" \
      az storage account create --name "$name" --resource-group "$AZURE_RESOURCE_GROUP" \
        --location "$AZURE_LOCATION" --kind StorageV2 --sku Standard_LRS \
        --min-tls-version TLS1_2 --allow-blob-public-access false \
        --enable-hierarchical-namespace "$hierarchical" --output none
    $check && return
  fi

  # The firewall must answer every request, or CI sees `403 AuthorizationFailure`
  # for a reason that is not the grant.
  local action
  action=$(az storage account show --name "$name" --resource-group "$AZURE_RESOURCE_GROUP" \
    --query 'networkRuleSet.defaultAction' --output tsv)
  if [ "$action" = Allow ]; then
    have "$name answers requests from anywhere"
  else
    need "$name answering requests from anywhere" \
      az storage account update --name "$name" --resource-group "$AZURE_RESOURCE_GROUP" --default-action Allow --output none
  fi

  # The corpus records the `<VersionId>` a versioned account writes beside
  # each object. A hierarchical account has no versioning to turn on.
  if [ "$hierarchical" = false ]; then
    local versioning
    versioning=$(az storage account blob-service-properties show --account-name "$name" \
      --resource-group "$AZURE_RESOURCE_GROUP" --query 'isVersioningEnabled' --output tsv)
    if [ "$versioning" = true ]; then
      have "$name keeps versions"
    else
      need "$name keeping versions" \
        az storage account blob-service-properties update --account-name "$name" \
          --resource-group "$AZURE_RESOURCE_GROUP" --enable-versioning true --output none
    fi
  fi

  # The container is made through the management plane, which an owner of the
  # subscription may use without any data-plane grant of their own.
  if [ "$(az storage container-rm exists --storage-account "$name" --resource-group "$AZURE_RESOURCE_GROUP" \
      --name "$AZURE_CONTAINER" --query exists --output tsv)" = true ]; then
    have "container $AZURE_CONTAINER on $name"
  else
    need "container $AZURE_CONTAINER on $name" \
      az storage container-rm create --storage-account "$name" --resource-group "$AZURE_RESOURCE_GROUP" \
        --name "$AZURE_CONTAINER" --output none
  fi
}

account "$AZURE_FLAT_ACCOUNT" false
account "$AZURE_HIERARCHICAL_ACCOUNT" true

# ------------------------------------------------------------- identities

# `application <prefix>`: the application `<prefix>_APP` names and its service
# principal, recording `<prefix>_CLIENT_ID` and `<prefix>_OBJECT_ID`.
application() {
  local prefix=$1 display client object
  display=$(eval "echo \"\$${prefix}_APP\"")
  client=$(az ad app list --display-name "$display" --query '[0].appId' --output tsv)
  if [ -n "$client" ]; then
    have "application $display"
  else
    need "application $display" true
    $check && return
    client=$(az ad app create --display-name "$display" --query appId --output tsv)
  fi
  [ "$client" = "$(eval "echo \"\$${prefix}_CLIENT_ID\"")" ] || record "${prefix}_CLIENT_ID" "$client"

  object=$(az ad sp show --id "$client" --query id --output tsv 2>/dev/null || true)
  if [ -n "$object" ]; then
    have "service principal of $display"
  else
    need "service principal of $display" true
    $check && return
    object=$(az ad sp create --id "$client" --query id --output tsv)
  fi
  [ "$object" = "$(eval "echo \"\$${prefix}_OBJECT_ID\"")" ] || record "${prefix}_OBJECT_ID" "$object"
}

application LIVE
application FIXTURES
application FIXTURES_ACCOUNT

# `grant <object id> <who> <role> <scope suffix> <account>`
grant() {
  local object=$1 who=$2 role=$3 suffix=$4 account=$5
  [ -n "$object" ] || return 0
  local scope
  scope="$(scope_of "$account")$suffix"
  local count
  count=$(az role assignment list --assignee "$object" --role "$role" --scope "$scope" --query 'length(@)' --output tsv)
  if [ "$count" != 0 ]; then
    have "$who: $role on $account$suffix"
  else
    need "$who: $role on $account$suffix" \
      az role assignment create --assignee-object-id "$object" --assignee-principal-type ServicePrincipal \
        --role "$role" --scope "$scope" --output none
  fi
}

for account in "$AZURE_FLAT_ACCOUNT" "$AZURE_HIERARCHICAL_ACCOUNT"; do
  # The workflow's identity and the recorder's: writing in the one container,
  # reading the whole blob service. The wider read is what lets a listing of a
  # container that is not there answer 404 rather than 403.
  for who in LIVE FIXTURES; do
    object=$(eval "echo \"\$${who}_OBJECT_ID\"")
    grant "$object" "$who" "Storage Blob Data Contributor" "/blobServices/default/containers/$AZURE_CONTAINER" "$account"
    grant "$object" "$who" "Storage Blob Data Reader" "/blobServices/default" "$account"
  done
  # The account-scoped recorder identity: writing anywhere.
  grant "$FIXTURES_ACCOUNT_OBJECT_ID" FIXTURES_ACCOUNT "Storage Blob Data Contributor" "/blobServices/default" "$account"
done

# The workflow presents a GitHub OIDC token for the `azure-live` environment.
# The subject is GitHub's immutable-identifier form, so a renamed repository
# keeps signing in.
if [ -n "$LIVE_CLIENT_ID" ]; then
  owner=${GITHUB_REPOSITORY%/*}
  owner_id=$(gh api "orgs/$owner" --jq .id 2>/dev/null || gh api "users/$owner" --jq .id)
  repository_id=$(gh api "repos/$GITHUB_REPOSITORY" --jq .id)
  subject="repo:$owner@$owner_id/${GITHUB_REPOSITORY#*/}@$repository_id:environment:$GITHUB_ENVIRONMENT"
  if az ad app federated-credential list --id "$LIVE_CLIENT_ID" --query "[?subject=='$subject'] | [0].name" --output tsv | grep -q .; then
    have "federated credential for $subject"
  else
    need "federated credential for $subject" \
      az ad app federated-credential create --id "$LIVE_CLIENT_ID" --parameters "$(jq -n --arg subject "$subject" \
        --arg name "github-${GITHUB_REPOSITORY//\//-}-$GITHUB_ENVIRONMENT" \
        '{name: $name, issuer: "https://token.actions.githubusercontent.com", subject: $subject, audiences: ["api://AzureADTokenExchange"]}')" \
        --output none
  fi
fi

# `secret <prefix> <file>`: a client secret for the identity `<prefix>` names,
# in `file`, made only when the file is not there. It is never printed.
secret() {
  local prefix=$1 file=$2 client
  client=$(eval "echo \"\$${prefix}_CLIENT_ID\"")
  [ -n "$client" ] || return 0
  if [ -r "$file" ]; then
    have "client secret at $file"
  else
    need "client secret at $file" true
    $check && return
    mkdir -p "$(dirname "$file")"
    (umask 077 && az ad app credential reset --id "$client" --display-name recorder --years 2 \
      --query password --output tsv > "$file")
  fi
}

secret FIXTURES "${AZURE_FIXTURES_SECRET_FILE:-$HOME/.config/borink/azure-fixtures.secret}"
secret FIXTURES_ACCOUNT "${AZURE_FIXTURES_ACCOUNT_SECRET_FILE:-$HOME/.config/borink/azure-fixtures-account.secret}"

# ----------------------------------------------------------------- GitHub

environment=repos/$GITHUB_REPOSITORY/environments/$GITHUB_ENVIRONMENT
if gh api "$environment" --jq '.protection_rules[].reviewers[].reviewer.login' 2>/dev/null | grep -qx "$GITHUB_REVIEWER"; then
  have "environment $GITHUB_ENVIRONMENT, approved by $GITHUB_REVIEWER"
else
  reviewer_id=$(gh api "users/$GITHUB_REVIEWER" --jq .id)
  need "environment $GITHUB_ENVIRONMENT, approved by $GITHUB_REVIEWER" \
    gh api --method PUT "$environment" --silent \
      --input <(jq -n --argjson id "$reviewer_id" '{reviewers: [{type: "User", id: $id}]}')
fi

# ------------------------------------------------------------------- done

if $check; then
  if [ "$missing" = 0 ]; then
    echo "everything is in place"
  else
    echo "$missing missing; run $0 to create them"
    exit 1
  fi
elif [ "$missing" != 0 ]; then
  echo "made $missing; a new role assignment takes about a minute to propagate"
fi
