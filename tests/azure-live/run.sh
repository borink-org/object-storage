#!/usr/bin/env bash
# Runs the live suite against the real accounts.
#
#   run.sh                      both accounts, the flat one first
#   run.sh flat                 one account
#   run.sh hierarchical
#   run.sh flat -- lists_       what follows `--` goes to the test harness
#
# The token comes from `AZURE_STORAGE_ACCESS_TOKEN` when that is set, and from
# `tests/azure-setup/token.sh` otherwise: the container-scoped identity on a
# workstation, the workflow identity in GitHub Actions. The workflow runs this
# script and nothing else, so what CI does is what you can do here.
#
# The suite is serial by design: its tests overwrite one key and empty one
# prefix each, so two at once would read what the other wrote. Two accounts
# are two runs, because the suite reads which one it is on once, from
# `AZURE_HIERARCHICAL`.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)

accounts=()
case "${1:-}" in
  "" | --) accounts=(flat hierarchical) ;;
  flat | hierarchical) accounts=("$1") ;;
  *) echo "usage: $0 [flat|hierarchical] [-- <test harness arguments>]" >&2; exit 2 ;;
esac
[ $# -gt 0 ] && shift
[ "${1:-}" = -- ] && shift

if [ -z "${AZURE_STORAGE_ACCESS_TOKEN:-}" ]; then
  AZURE_STORAGE_ACCESS_TOKEN=$("$root/tests/azure-setup/token.sh")
  export AZURE_STORAGE_ACCESS_TOKEN
fi
# Nothing below prints the token, and in Actions the log would hide it anyway.
[ -n "${GITHUB_ACTIONS:-}" ] && echo "::add-mask::$AZURE_STORAGE_ACCESS_TOKEN"

for account in "${accounts[@]}"; do
  echo "--- azure-live: $account account"
  if [ "$account" = hierarchical ]; then
    export AZURE_HIERARCHICAL=1
  else
    unset AZURE_HIERARCHICAL
  fi
  (cd "$root" && cargo test --locked -p azure-live -- --ignored --test-threads=1 "$@")
done
