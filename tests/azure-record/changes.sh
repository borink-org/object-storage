#!/usr/bin/env bash
# Shows how the recorded corpus differs from the committed one, with the
# values that change on every recording masked: dates, entity tags, request
# identifiers, version identifiers and the trace the service writes in an
# error body. What remains is the service answering differently, which is
# what a reviewer of a re-recording wants to see.
#
#   changes.sh            against HEAD
#   changes.sh <commit>   against that commit
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
against=${1:-HEAD}
corpus=crates/object-storage-proto/tests/fixtures

mask() {
  sed -E \
    -e 's/[A-Z][a-z]{2}, [0-9]{2} [A-Z][a-z]{2} [0-9]{4} [0-9:]{8} GMT/<date>/g' \
    -e 's/0x8[0-9A-F]{14,15}/<etag>/g' \
    -e 's/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/<uuid>/g' \
    -e 's/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/<stamp>/g' \
    -e 's/(<NextMarker>)[^<]+(<\/NextMarker>)/\1<marker>\2/g' \
    -e 's/(Time:)<date>/\1<date>/g'
}

cd "$root"
changed=0
while IFS= read -r file; do
  [ -n "$file" ] || continue
  if ! diff -u --label "$against:$file" --label "$file" \
      <(git show "$against:$file" 2>/dev/null | mask) <(mask < "$file"); then
    changed=1
  fi
done < <(git diff --name-only "$against" -- "$corpus"; git ls-files --others --exclude-standard -- "$corpus")
[ "$changed" = 0 ] && echo "nothing"
exit 0
