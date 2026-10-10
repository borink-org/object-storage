#!/usr/bin/env bash
# Grades the TypeScript client against the recorded cases of
# borink-org/object-tests, through the adapter beside this script: once in Bun,
# and once in workerd, the runtime of Cloudflare Workers.
#
# Takes the object-tests revision that hosts/object-tests/grade.sh pins, and
# shares its checkout in target/object-tests. Then it grades every offline
# suite against `expected-unsupported.json`, which lists the cases the adapter
# reports unsupported and why. A listed case that starts to pass fails the run
# as well as a case that regresses, so the list stays exact. Both runtimes are
# held to the one list.
#
#     typescript/object-tests/grade.sh
#
# After a change that supports a case, rewrite the list and review its diff:
#
#     typescript/object-tests/grade.sh --record

set -euo pipefail

suites=(operations management vectors large)

mode=--expected-unsupported
case ${1-} in
    "") ;;
    --record) mode=--record-unsupported ;;
    *) echo "usage: grade.sh [--record]" >&2; exit 2 ;;
esac

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
package=$(cd "$here/.." && pwd)
root=$(cd "$package/.." && pwd)
list=$here/expected-unsupported.json
tests=$root/target/object-tests
revision=$(sed -n 's/^revision=//p' "$root/hosts/object-tests/grade.sh")

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
(cd "$package" && bun install --frozen-lockfile --silent)

# Grades every suite with the adapter command that follows the runtime's
# name. The grader prints a report per case and a summary last. The summary
# is what this prints; the reports go to a log beside the checkout.
status=0
grade() {
    local runtime=$1
    shift
    for suite in "${suites[@]}"; do
        local log=$tests/typescript-$runtime-$suite.log
        if "$tests/target/release/object-tests" grade "$tests/cases/$suite.json" \
            "$mode" "$list" -- "$@" > "$log"; then
            echo "$runtime $suite: $(tail -n 1 "$log")"
        else
            status=1
            echo "$runtime $suite failed, reports in $log: $(tail -n 1 "$log")" >&2
        fi
    done
}

grade bun bun "$here/adapter.ts"

# The worker answers on a port that is free now.
bun build "$here/workerd/worker.ts" --target=browser --format=esm \
    --external cloudflare:sockets --outfile "$here/workerd/dist/worker.js" > /dev/null
port=$(bun -e 'const server = Bun.listen({hostname: "127.0.0.1", port: 0, socket: {data() {}}});
console.log(server.port); server.stop();')
"$package/node_modules/.bin/workerd" serve "$here/workerd/config.capnp" \
    --socket-addr "http=127.0.0.1:$port" > "$tests/typescript-workerd.log" 2>&1 &
workerd=$!
trap 'kill $workerd 2> /dev/null || true' EXIT
for _ in $(seq 100); do
    curl --silent --output /dev/null "http://127.0.0.1:$port" && break
    sleep 0.1
done
export WORKER_URL=http://127.0.0.1:$port
grade workerd env -u HTTP_PROXY -u http_proxy bun "$here/workerd/forward.ts"
exit $status
