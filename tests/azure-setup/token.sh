#!/usr/bin/env bash
# Prints a blob data-plane access token, good for about an hour.
#
#   token.sh live       the live suite's identity: writes in the live container
#   token.sh fixtures   the recorder's identity: writes in the fixtures container
#   token.sh account    the recorder's second identity: writes anywhere in the account
#
# Each tool has an identity that can write where that tool writes and nowhere
# else, so a live run cannot touch the fixtures and a recording cannot touch a
# live run, whatever either one's code does.
#
# On a workstation each identity signs in with the client secret that
# `setup.sh` put in `~/.config/borink/`, readable by you alone. The secret is
# exchanged for a token here and never becomes an environment variable, and
# your own `az` login is left as it is.
#
# In GitHub Actions there is no secret. The job's own OIDC token is exchanged
# for a token of the live identity instead. The other two are workstation
# identities.
#
# `identities.env` names the tenant and the applications. Both suites read the
# token from `AZURE_STORAGE_ACCESS_TOKEN`; `run.sh` beside each of them calls
# this when that is not set.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=identities.env
source "$here/identities.env"

endpoint=https://login.microsoftonline.com/$AZURE_TENANT_ID/oauth2/v2.0/token
scope=https://storage.azure.com/.default

case "${1:-}" in
  live)
    if [ -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ]; then
      # The job asks GitHub for a token naming the environment it runs in,
      # and Azure takes that token as the credential of the workflow identity.
      assertion=$(curl -sf -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
        "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=api%3A%2F%2FAzureADTokenExchange" | jq -re .value)
      curl -sf -X POST "$endpoint" \
        --data-urlencode grant_type=client_credentials \
        --data-urlencode "client_id=$LIVE_CLIENT_ID" \
        --data-urlencode client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer \
        --data-urlencode "client_assertion=$assertion" \
        --data-urlencode "scope=$scope" \
        | jq -re .access_token
      exit
    fi
    client=$LIVE_CLIENT_ID
    secret_file=${AZURE_LIVE_SECRET_FILE:-$HOME/.config/borink/azure-live.secret}
    ;;
  fixtures)
    client=$FIXTURES_CLIENT_ID
    secret_file=${AZURE_FIXTURES_SECRET_FILE:-$HOME/.config/borink/azure-fixtures.secret}
    ;;
  account)
    client=$FIXTURES_ACCOUNT_CLIENT_ID
    secret_file=${AZURE_FIXTURES_ACCOUNT_SECRET_FILE:-$HOME/.config/borink/azure-fixtures-account.secret}
    ;;
  *)
    echo "usage: $0 live|fixtures|account" >&2
    exit 2
    ;;
esac

if [ ! -r "$secret_file" ]; then
  echo "no client secret at $secret_file; tests/azure-setup/setup.sh makes one" >&2
  exit 1
fi

curl -sf -X POST "$endpoint" \
  --data-urlencode grant_type=client_credentials \
  --data-urlencode "client_id=$client" \
  --data-urlencode "client_secret=$(cat "$secret_file")" \
  --data-urlencode "scope=$scope" \
  | jq -re .access_token
