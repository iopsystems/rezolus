#!/usr/bin/env bash
# Capture a pair of parquets for A/B compare-mode manual verification.
#
# Assumes a Rezolus agent is already running locally and listening on
# http://localhost:4241. Records two 60-second windows back-to-back; the
# second run should be under a visibly different load (e.g., different
# sysctl, different workload), so the compare view shows deltas worth
# eyeballing.
#
# Output:
#   data/test_fixtures/baseline.parquet
#   data/test_fixtures/experiment.parquet
#
# Usage:
#   data/test_fixtures/compare-demo.sh
#
# To exercise the comparison UI:
#   target/release/rezolus view \
#       data/test_fixtures/baseline.parquet \
#       data/test_fixtures/experiment.parquet

set -euo pipefail

REZOLUS="${REZOLUS:-./target/release/rezolus}"
AGENT_URL="${AGENT_URL:-http://localhost:4241}"
DURATION="${DURATION:-60s}"
OUT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Recording baseline (${DURATION}) from ${AGENT_URL}..."
"$REZOLUS" record --duration "$DURATION" "$AGENT_URL" "$OUT_DIR/baseline.parquet"

echo ""
echo "Apply workload or config change for the experiment now, then press Enter."
read -r

echo "Recording experiment (${DURATION}) from ${AGENT_URL}..."
"$REZOLUS" record --duration "$DURATION" "$AGENT_URL" "$OUT_DIR/experiment.parquet"

echo ""
echo "Done. Launch compare view with:"
echo "  $REZOLUS view $OUT_DIR/baseline.parquet $OUT_DIR/experiment.parquet"
