#!/usr/bin/env bash
# Capture an A/B pair for compare-mode manual verification.
#
# Assumes a Rezolus agent is already running locally and listening on
# http://localhost:4241. Records two 60-second windows back-to-back; the
# second run should be under a visibly different load (e.g., different
# sysctl, different workload), so the compare view shows deltas worth
# eyeballing. Each run is labeled with its arm, and the two are combined
# into one two-recording archive, which `rezolus view` shows as an A/B
# comparison.
#
# Output:
#   data/test_fixtures/baseline.dendro
#   data/test_fixtures/experiment.dendro
#   data/test_fixtures/ab.dendro
#
# Usage:
#   data/test_fixtures/compare-demo.sh
#
# To exercise the comparison UI:
#   target/release/rezolus view data/test_fixtures/ab.dendro

set -euo pipefail

REZOLUS="${REZOLUS:-./target/release/rezolus}"
AGENT_URL="${AGENT_URL:-http://localhost:4241}"
DURATION="${DURATION:-60s}"
OUT_DIR="$(cd "$(dirname "$0")" && pwd)"

# The recorder refuses to overwrite an existing archive.
rm -f "$OUT_DIR/baseline.dendro" "$OUT_DIR/experiment.dendro" "$OUT_DIR/ab.dendro"

echo "Recording baseline (${DURATION}) from ${AGENT_URL}..."
"$REZOLUS" record --duration "$DURATION" --url "$AGENT_URL" --label arm=baseline \
    -o "$OUT_DIR/baseline.dendro"

echo ""
echo "Apply workload or config change for the experiment now, then press Enter."
read -r

echo "Recording experiment (${DURATION}) from ${AGENT_URL}..."
"$REZOLUS" record --duration "$DURATION" --url "$AGENT_URL" --label arm=experiment \
    -o "$OUT_DIR/experiment.dendro"

"$REZOLUS" recording combine "$OUT_DIR/baseline.dendro" "$OUT_DIR/experiment.dendro" \
    -o "$OUT_DIR/ab.dendro"

echo ""
echo "Done. Launch compare view with:"
echo "  $REZOLUS view $OUT_DIR/ab.dendro"
