#!/usr/bin/env bash
# Records the corpus again, from both accounts, and shows what changed.
#
# The two tokens come from `AZURE_STORAGE_ACCESS_TOKEN` and
# `AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT` when they are set, and from
# `tests/azure-setup/token.sh fixtures` and `token.sh account` otherwise.
# Recordings take turns: the recorder takes a lock in the fixtures container
# and refuses to start while another run holds it.
#
# Then read what `changes.sh` prints: the diff with the values that change on
# every recording masked, so what is left is the service answering differently.
# A run that prints nothing there is the service answering as it did before.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
token=$root/tests/azure-setup/token.sh

if [ -z "${AZURE_STORAGE_ACCESS_TOKEN:-}" ]; then
  AZURE_STORAGE_ACCESS_TOKEN=$("$token" fixtures)
  export AZURE_STORAGE_ACCESS_TOKEN
fi
if [ -z "${AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT:-}" ]; then
  AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT=$("$token" account)
  export AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT
fi

cd "$root"
cargo run --locked -q -p azure-record
git --no-pager diff --stat -- crates/object-storage-proto/tests/fixtures | tail -1
echo "--- what changed beyond the values that always change:"
tests/azure-record/changes.sh
