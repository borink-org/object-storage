#!/usr/bin/env bash
# Grades the Rust crates against the recorded cases of borink-org/object-tests.
#
# Clones object-tests at the revision below into target/object-tests and builds
# its grader, then builds the adapter beside this script. Then it grades every
# offline suite against `expected-unsupported.json`, which lists the cases the
# adapter reports unsupported and why. A listed case that starts to pass fails
# the run as well as a case that regresses, so the list stays exact.
#
#     hosts/object-tests/grade.sh
#
# The revision fixes both the cases and the protocol the adapter speaks. After
# moving it, update the adapter to match. After that, or after a change that
# supports a case, rewrite the list and review its diff:
#
#     hosts/object-tests/grade.sh --record

set -euo pipefail

# Move this on purpose: a new revision can bring new cases, or a new protocol.
revision=7b2158343612d5d626b4db6253ff75b9ae1d400f
suites=(core operations s3-express vectors)

mode=--expected-unsupported
case ${1-} in
    "") ;;
    --record) mode=--record-unsupported ;;
    *) echo "usage: grade.sh [--record]" >&2; exit 2 ;;
esac

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../.." && pwd)
list=$here/expected-unsupported.json
tests=$root/target/object-tests

if [[ $(git -C "$tests" rev-parse HEAD 2>/dev/null) != "$revision" ]]; then
    rm -rf "$tests"
    git init --quiet "$tests"
    git -C "$tests" fetch --quiet --depth 1 \
        https://github.com/borink-org/object-tests.git "$revision"
    git -C "$tests" checkout --quiet FETCH_HEAD
fi

# Cargo runs from the root, so rustup picks the toolchain this repository pins.
cd "$root"
cargo build --release --locked --manifest-path "$tests/Cargo.toml"
cargo build --release --locked -p borink-object-tests-adapter

# The grader prints a report per case and a summary last. The summary is
# what this prints; the reports go to a log beside the checkout.
status=0
for suite in "${suites[@]}"; do
    log=$tests/$suite.log
    if "$tests/target/release/object-tests" grade "$tests/cases/$suite.json" \
        "$mode" "$list" -- "$root/target/release/object-tests-adapter" \
        > "$log"; then
        echo "$suite: $(tail -n 1 "$log")"
    else
        status=1
        echo "$suite failed, reports in $log: $(tail -n 1 "$log")" >&2
    fi
done
exit $status
