#!/usr/bin/env bash
# Read-cost measurement for the split-table gate
# (docs/superpowers/specs/2026-08-17-acquisition-groups-design.md).
#
# Usage: scripts/split-rez-measure.sh <wide-v3.rez>
#
# Produces three arms in a temp dir — the original file, a provenance-matched
# wide re-encode, and the split-table rewrite — then reports, per arm:
#   - file size and segment/table counts (sqlite3)
#   - `rezolus parquet metadata` wall time (open/manifest cost), 7 reps
#   - `rezolus mcp query` wall time per query, 7 reps
# Output is TSV on stdout; capture it into the journal entry.
set -euo pipefail

REZOLUS=${REZOLUS:-target/release/rezolus}
IN=${1:?usage: $0 <wide-v3.rez>}
DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT

echo "# rewriting arms into $DIR" >&2
"$REZOLUS" parquet split-groups -i "$IN" -o "$DIR/wide-rt.rez" --wide
"$REZOLUS" parquet split-groups -i "$IN" -o "$DIR/split.rez"

ARMS=("$IN" "$DIR/wide-rt.rez" "$DIR/split.rez")

# Queries chosen to stay within one acquisition-group table each (route()
# refuses cross-table queries). Verify these metrics exist in the recording
# before trusting zeros:
"$REZOLUS" mcp describe-metrics "$IN" | grep -cE 'cpu_usage|cgroup_cpu_cycles|scheduler_runqueue_wait' >&2 \
  || { echo "expected metrics missing from $IN — pick queries from describe-metrics" >&2; exit 1; }
QUERIES=(
  'sum(irate(cpu_usage[1m]))'
  'sum by (name) (irate(cgroup_cpu_cycles[5m]))'
  'sum(irate(scheduler_runqueue_wait[5m]))'
)

printf 'arm\tmetric\tvalue\n'
for f in "${ARMS[@]}"; do
  printf '%s\tbytes\t%s\n' "$f" "$(stat -c %s "$f" 2>/dev/null || stat -f %z "$f")"
  printf '%s\tsegments\t%s\n' "$f" "$(sqlite3 "$f" 'SELECT count(*) FROM segments;')"
  printf '%s\ttables\t%s\n' "$f" "$(sqlite3 "$f" 'SELECT count(DISTINCT sampler) FROM segments;')"
done

for f in "${ARMS[@]}"; do
  for i in $(seq 1 7); do
    s=$(date +%s%N)
    "$REZOLUS" parquet metadata -i "$f" >/dev/null
    e=$(date +%s%N)
    printf '%s\tmetadata_ms\t%s\n' "$f" $(( (e - s) / 1000000 ))
  done
  for q in "${QUERIES[@]}"; do
    for i in $(seq 1 7); do
      s=$(date +%s%N)
      "$REZOLUS" mcp query "$f" "$q" >/dev/null
      e=$(date +%s%N)
      printf '%s\tquery_ms[%s]\t%s\n' "$f" "$q" $(( (e - s) / 1000000 ))
    done
  done
done
