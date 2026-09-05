#!/usr/bin/env bash
# Runs the live suite against the real accounts.
#
#   run.sh                      both accounts, side by side
#   run.sh flat                 one account
#   run.sh hierarchical
#   run.sh flat -- lists_       what follows `--` goes to the test harness
#
# The token comes from `AZURE_STORAGE_ACCESS_TOKEN` when that is set, and from
# `tests/azure-setup/token.sh live` otherwise: the live identity, signing in
# with a workstation secret or, in GitHub Actions, with the job's OIDC token.
# The workflow runs this script and nothing else, so what CI does is what you
# can do here.
#
# Every key the run writes sits under a segment named after the run, `TEST_RUN`,
# so runs never see each other and nothing is cleaned up: a lifecycle rule on
# the account removes what runs leave behind after a day. In Actions the run
# is named after the workflow run; here, after the clock.
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
  AZURE_STORAGE_ACCESS_TOKEN=$("$root/tests/azure-setup/token.sh" live)
  export AZURE_STORAGE_ACCESS_TOKEN
fi
# Nothing below prints the token, and in Actions the log would hide it anyway.
[ -n "${GITHUB_ACTIONS:-}" ] && echo "::add-mask::$AZURE_STORAGE_ACCESS_TOKEN"

if [ -z "${TEST_RUN:-}" ]; then
  if [ -n "${GITHUB_RUN_ID:-}" ]; then
    TEST_RUN=gh-$GITHUB_RUN_ID-${GITHUB_RUN_ATTEMPT:-1}
  else
    TEST_RUN=$(date -u +%Y%m%dT%H%M%SZ)-$$
  fi
  export TEST_RUN
fi
echo "--- azure-live: run $TEST_RUN"

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
