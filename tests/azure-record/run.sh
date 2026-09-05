#!/usr/bin/env bash
# Records the corpus again, from both accounts, and shows what changed.
#
# The two tokens come from `AZURE_STORAGE_ACCESS_TOKEN` and
# `AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT` when they are set, and from
# `tests/azure-setup/token.sh` otherwise. Then read the diff: a run that
# changes nothing but the dates, the entity tags, the versions and the request
# identifiers is the service answering as it did before. Anything else is the
# thing worth looking at, and worth saying in the commit message.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
token=$root/tests/azure-setup/token.sh

if [ -z "${AZURE_STORAGE_ACCESS_TOKEN:-}" ]; then
  AZURE_STORAGE_ACCESS_TOKEN=$("$token")
  export AZURE_STORAGE_ACCESS_TOKEN
fi
if [ -z "${AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT:-}" ]; then
  AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT=$("$token" account)
  export AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT
fi

cd "$root"
cargo run --locked -q -p azure-record
git --no-pager diff --stat -- crates/object-storage-proto/tests/fixtures
