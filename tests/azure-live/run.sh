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
# Every test owns its own keys, so the tests of one account run at once, as
# the harness runs them. Two accounts are two runs, because the suite reads
# which one it is on from `AZURE_HIERARCHICAL`; here they run side by side,
# each line of output marked with its account.
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

# The binary is built once, before the runs, so that neither run waits on the
# other's build.
(cd "$root" && cargo test --locked -p azure-live --no-run --quiet)

pids=()
for account in "${accounts[@]}"; do
  if [ "$account" = hierarchical ]; then
    hierarchical=1
  else
    hierarchical=
  fi
  (cd "$root" && AZURE_HIERARCHICAL=$hierarchical cargo test --locked -q -p azure-live -- --ignored "$@" 2>&1 \
    | sed "s/^/$account: /") &
  pids+=($!)
done
status=0
for pid in "${pids[@]}"; do
  wait "$pid" || status=1
done
exit $status
