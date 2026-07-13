#!/usr/bin/env bash
# Backend-data validation of the display / decimation pipeline against SYNTHETIC
# data with known properties.
#
#   1. builds + runs examples/gen_display_testdata to write a baseline and an
#      experiment parquet (deterministic gauge spikes, bursty counter, latency
#      histogram with tail spikes; the experiment carries a ~2x regression),
#   2. starts the viewer in COMPARE mode (both captures loaded),
#   3. runs tests/display_synthetic.test.mjs against the live API, asserting the
#      decimation guarantees exactly (spike preservation, budget, window
#      resolution, envelope ordering, heatmap decode, A/B delta).
#
# Exit 0 = all assertions passed. On failure the viewer log is dumped.
set -euo pipefail

cd "$(dirname "$0")/.."
PORT="${PORT:-18811}"
TMP="$(mktemp -d)"
BASE="$TMP/synth_base.parquet"
EXP="$TMP/synth_exp.parquet"
VIEWER_PID=""

cleanup() {
    [[ -n "$VIEWER_PID" ]] && kill "$VIEWER_PID" 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT

echo "==> building generator + viewer"
cargo build -q --example gen_display_testdata
cargo build -q -p rezolus

echo "==> generating synthetic fixtures"
./target/debug/examples/gen_display_testdata "$BASE" "$EXP"

echo "==> starting compare-mode viewer on 127.0.0.1:$PORT"
./target/debug/rezolus view "$BASE" "$EXP" --listen "127.0.0.1:$PORT" >"$TMP/viewer.log" 2>&1 &
VIEWER_PID=$!

ready=""
for _ in $(seq 1 40); do
    if curl -sf "127.0.0.1:$PORT/api/v1/mode" >/dev/null 2>&1; then ready=1; break; fi
    sleep 0.5
done
if [[ -z "$ready" ]]; then
    echo "!! viewer never came up; log:" >&2
    cat "$TMP/viewer.log" >&2
    exit 1
fi

echo "==> running backend-data assertions"
if VIEWER_URL="http://127.0.0.1:$PORT" COMPARE=1 node --test tests/display_synthetic.test.mjs; then
    echo "==> PASS"
else
    echo "!! assertions failed; viewer log:" >&2
    cat "$TMP/viewer.log" >&2
    exit 1
fi
