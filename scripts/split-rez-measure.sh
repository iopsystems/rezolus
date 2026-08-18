#!/usr/bin/env bash
# Read-cost measurement for the split-table gate
# (docs/journal/2026-08-17-window-sidecar-cost.md).
#
# Usage: scripts/split-rez-measure.sh <wide-v3.rez>
#
# Produces three arms in one temp dir — a copy of the original file, a
# provenance-matched wide re-encode, and the split-table rewrite (same
# filesystem for all three, so device speed cannot skew the comparison) —
# then reports, per arm:
#   - file size and segment/table counts (sqlite3)
#   - `rezolus parquet metadata` wall time (open/manifest cost), 7 reps
#   - `rezolus mcp query` wall time per query, 7 reps
# One untimed warm-up pass per arm precedes the timed reps so every arm
# starts from the same (warm) cache state, and reps are round-robined
# across arms so host drift (turbo, thermals) cannot land on one arm.
# Output is TSV on stdout (arm labels orig/wide-rt/split); capture it into
# the journal entry. On failure the temp dir is KEPT for post-mortem.
#
# Linux-only: timing uses GNU date's %N nanoseconds.
set -euo pipefail

REZOLUS=${REZOLUS:-target/release/rezolus}
IN=${1:?usage: $0 <wide-v3.rez>}

command -v sqlite3 >/dev/null || { echo "sqlite3 CLI required" >&2; exit 1; }
[ -x "$REZOLUS" ] || { echo "rezolus binary not found/executable: $REZOLUS" >&2; exit 1; }
case "$(uname -s)" in Linux) ;; *) echo "warning: timing needs GNU date %N; non-Linux results are invalid" >&2 ;; esac

DIR=$(mktemp -d)
cleanup() {
  ec=$?
  if [ "$ec" -eq 0 ]; then rm -rf "$DIR"; else echo "arms kept for inspection: $DIR" >&2; fi
}
trap cleanup EXIT

# Every query below must resolve, or `mcp query` hard-exits mid-run under
# set -e; require ALL metrics up front so a bad input fails in seconds.
METRICS=$("$REZOLUS" mcp describe-metrics "$IN")
for m in cpu_usage cgroup_cpu_cycles scheduler_runqueue_wait; do
  # describe-metrics lists each metric alone on its own bulleted line
  # ("• <name>"), so anchor to the whole line rather than substring-match —
  # a plain `grep -F "$m"` would also hit e.g. cgroup_cpu_usage when
  # checking for cpu_usage, or match inside description prose.
  grep -qE "^• ${m}\$" <<<"$METRICS" \
    || { echo "missing metric: $m — pick queries from describe-metrics" >&2; exit 1; }
done

[ -e "$IN-wal" ] && { echo "input has an uncheckpointed WAL sidecar ($IN-wal): checkpoint or snapshot it first" >&2; exit 1; }

echo "# rewriting arms into $DIR" >&2
cp "$IN" "$DIR/orig.rez"
"$REZOLUS" parquet split-groups -i "$IN" -o "$DIR/wide-rt.rez" --wide
"$REZOLUS" parquet split-groups -i "$IN" -o "$DIR/split.rez"

ARMS=(orig wide-rt split)

# Queries chosen to stay within one acquisition-group table each (route()
# refuses cross-table queries).
QUERIES=(
  'sum(irate(cpu_usage[1m]))'
  'sum by (name) (irate(cgroup_cpu_cycles[5m]))'
  'sum(irate(scheduler_runqueue_wait[5m]))'
)

printf 'arm\tmetric\tvalue\n'
for a in "${ARMS[@]}"; do
  f="$DIR/$a.rez"
  printf '%s\tbytes\t%s\n' "$a" "$(stat -c %s "$f" 2>/dev/null || stat -f %z "$f")"
  printf '%s\tsegments\t%s\n' "$a" "$(sqlite3 "$f" 'SELECT count(*) FROM segments;')"
  printf '%s\ttables\t%s\n' "$a" "$(sqlite3 "$f" 'SELECT count(DISTINCT sampler) FROM segments;')"
done

# Untimed warm-up: bring every arm to the same cache state before timing.
for a in "${ARMS[@]}"; do
  f="$DIR/$a.rez"
  "$REZOLUS" parquet metadata -i "$f" >/dev/null
  for q in "${QUERIES[@]}"; do
    "$REZOLUS" mcp query "$f" "$q" >/dev/null
  done
done

# Timed reps, round-robined across arms.
for rep in $(seq 1 7); do
  for a in "${ARMS[@]}"; do
    f="$DIR/$a.rez"
    s=$(date +%s%N)
    "$REZOLUS" parquet metadata -i "$f" >/dev/null
    e=$(date +%s%N)
    printf '%s\tmetadata_ms\t%s\n' "$a" $(( (e - s) / 1000000 ))
    for q in "${QUERIES[@]}"; do
      s=$(date +%s%N)
      "$REZOLUS" mcp query "$f" "$q" >/dev/null
      e=$(date +%s%N)
      printf '%s\tquery_ms[%s]\t%s\n' "$a" "$q" $(( (e - s) / 1000000 ))
    done
  done
done
