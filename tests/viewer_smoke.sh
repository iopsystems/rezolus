#!/usr/bin/env bash
# tests/viewer_smoke.sh — end-to-end smoke test for `rezolus view`.
#
# Spawns the viewer in four configurations (upload-only, file mode, A/B
# file mode, proxy enabled) on adjacent ports, hits the public API
# endpoints, and asserts the expected mode + payload shape. Also covers
# experiment attach / detach via /api/v1/captures/experiment.
#
# Live mode is intentionally skipped — it requires a running rezolus
# agent and isn't reproducible from a dev/CI machine without one.
#
# Requirements: cargo, bash 4+, curl, jq. Listens on 18500-18503.
# Exits 0 on full pass, 1 on first failed assertion (with the failing
# server's log printed for context).

set -euo pipefail
cd "$(dirname "$0")/.."

PORT_UPLOAD=18500
PORT_FILE=18501
PORT_AB=18502
PORT_PROXY=18503

PARQUET_FILE=site/viewer/data/cachecannon.parquet
PARQUET_AB_A=site/viewer/data/AB_base.parquet
PARQUET_AB_B=site/viewer/data/AB_base_pin.parquet

LOGDIR=$(mktemp -d -t rezolus-smoke-XXXXXX)
echo "logs: $LOGDIR"

# Build before launching anything so a compile failure surfaces early.
cargo build --bin rezolus 2>&1 | tail -3

# Don't pop browser tabs during the smoke test.
export REZOLUS_NO_OPEN=1

# Launch all four. Each writes its log to $LOGDIR so failures can be
# triaged without re-running.
./target/debug/rezolus view \
    --listen 127.0.0.1:$PORT_UPLOAD \
    > "$LOGDIR/upload.log" 2>&1 &
PID_UPLOAD=$!

./target/debug/rezolus view "$PARQUET_FILE" \
    --listen 127.0.0.1:$PORT_FILE \
    > "$LOGDIR/file.log" 2>&1 &
PID_FILE=$!

./target/debug/rezolus view "$PARQUET_AB_A" "$PARQUET_AB_B" \
    --listen 127.0.0.1:$PORT_AB \
    > "$LOGDIR/ab.log" 2>&1 &
PID_AB=$!

./target/debug/rezolus view \
    --proxy-allow=httpbin.org \
    --listen 127.0.0.1:$PORT_PROXY \
    > "$LOGDIR/proxy.log" 2>&1 &
PID_PROXY=$!

cleanup() {
    kill "$PID_UPLOAD" "$PID_FILE" "$PID_AB" "$PID_PROXY" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup EXIT

# Wait for each port to accept connections (up to 30 s). Bash's
# /dev/tcp is built-in and works on both macOS and Linux.
wait_for_port() {
    local port=$1 tries=30
    while ! (echo > /dev/tcp/127.0.0.1/"$port") 2>/dev/null; do
        sleep 1
        tries=$((tries - 1))
        if [ "$tries" -le 0 ]; then
            echo "FAIL: port $port did not open within 30 s"
            return 1
        fi
    done
}

for port in $PORT_UPLOAD $PORT_FILE $PORT_AB $PORT_PROXY; do
    wait_for_port "$port" || {
        echo "--- log for port $port ---"
        case $port in
            $PORT_UPLOAD) cat "$LOGDIR/upload.log" ;;
            $PORT_FILE)   cat "$LOGDIR/file.log" ;;
            $PORT_AB)     cat "$LOGDIR/ab.log" ;;
            $PORT_PROXY)  cat "$LOGDIR/proxy.log" ;;
        esac
        exit 1
    }
done

# Failure surfaces: print the failing assertion + the most relevant
# server log so triage doesn't need a re-run.
fail() {
    local label=$1 got=$2 want=$3 logfile=$4
    echo "FAIL: $label"
    echo "  got:  $got"
    echo "  want: $want"
    if [ -n "$logfile" ] && [ -s "$logfile" ]; then
        echo "--- $logfile ---"
        cat "$logfile"
    fi
    exit 1
}

eq() {
    local label=$1 got=$2 want=$3 logfile=${4:-}
    if [ "$got" != "$want" ]; then
        fail "$label" "$got" "$want" "$logfile"
    fi
}

mode_at() { curl -fsS "http://127.0.0.1:$1/api/v1/mode"; }

echo "==> /api/v1/mode (all four)"
mode=$(mode_at $PORT_UPLOAD)
eq "upload mode loaded"      "$(echo "$mode" | jq -r .loaded)"       "false"    "$LOGDIR/upload.log"
eq "upload mode url_loading" "$(echo "$mode" | jq -r .url_loading)"  "disabled" "$LOGDIR/upload.log"

mode=$(mode_at $PORT_FILE)
eq "file mode loaded"        "$(echo "$mode" | jq -r .loaded)"       "true"     "$LOGDIR/file.log"

mode=$(mode_at $PORT_AB)
eq "A/B compare_mode"        "$(echo "$mode" | jq -r .compare_mode)" "true"     "$LOGDIR/ab.log"

mode=$(mode_at $PORT_PROXY)
eq "proxy url_loading"       "$(echo "$mode" | jq -r .url_loading)"  "proxy"    "$LOGDIR/proxy.log"

echo "==> /api/v1/sections returns navigation"
sections=$(curl -fsS "http://127.0.0.1:$PORT_FILE/api/v1/sections")
echo "$sections" | jq -e '.data.sections | length > 0' >/dev/null \
    || fail "sections payload empty" "$sections" "non-empty .data.sections" "$LOGDIR/file.log"

echo "==> /data/<section>.json returns groups"
overview=$(curl -fsS "http://127.0.0.1:$PORT_FILE/data/overview.json")
echo "$overview" | jq -e '.groups | length > 0' >/dev/null \
    || fail "overview payload empty" "$(echo "$overview" | head -c 200)" "non-empty .groups" "$LOGDIR/file.log"

echo "==> /api/v1/query_range returns matrix"
rq=$(curl -fsS "http://127.0.0.1:$PORT_FILE/api/v1/query_range?query=cpu_cores&start=0&end=10000000000&step=60")
eq "range query status" "$(echo "$rq" | jq -r .status)" "success" "$LOGDIR/file.log"

echo "==> /api/v1/load_url forbidden when proxy disabled"
lu=$(curl -fsS -X POST -H 'content-type: application/json' \
     -d '{"url":"https://httpbin.org/bytes/100"}' \
     "http://127.0.0.1:$PORT_UPLOAD/api/v1/load_url")
eq "load_url forbidden envelope" "$(echo "$lu" | jq -r .errorType)" "forbidden" "$LOGDIR/upload.log"

echo "==> /api/v1/load_url proxies when allowed (httpbin returns non-parquet → invalid_parquet)"
# Network test: tolerate a transient httpbin outage by warning rather
# than failing. The response shape (envelope with errorType) is what
# we're really verifying.
if lu=$(curl -fsS --max-time 10 -X POST -H 'content-type: application/json' \
        -d '{"url":"https://httpbin.org/bytes/100"}' \
        "http://127.0.0.1:$PORT_PROXY/api/v1/load_url"); then
    eq "load_url proxied errorType" "$(echo "$lu" | jq -r .errorType)" "invalid_parquet" "$LOGDIR/proxy.log"
else
    echo "    (skipped: httpbin.org unreachable)"
fi

echo "==> POST /api/v1/captures/experiment attaches experiment"
curl -fsS -X POST --data-binary @"$PARQUET_AB_B" \
    -H 'Content-Type: application/octet-stream' \
    -H "x-rezolus-filename: $(basename "$PARQUET_AB_B")" \
    "http://127.0.0.1:$PORT_FILE/api/v1/captures/experiment" >/dev/null
sleep 1
mode=$(mode_at $PORT_FILE)
eq "experiment attached → compare_mode" "$(echo "$mode" | jq -r .compare_mode)" "true" "$LOGDIR/file.log"

echo "==> DELETE /api/v1/captures/experiment detaches"
curl -fsS -X DELETE "http://127.0.0.1:$PORT_FILE/api/v1/captures/experiment" >/dev/null
sleep 1
mode=$(mode_at $PORT_FILE)
eq "experiment detached → compare_mode" "$(echo "$mode" | jq -r .compare_mode)" "false" "$LOGDIR/file.log"

echo "==> static assets respond 200"
for path in / /about /lib/style.css; do
    status=$(curl -fsS -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT_UPLOAD$path" || true)
    eq "static $path" "$status" "200" "$LOGDIR/upload.log"
done

echo "==> served JS bundle has the Notebook rename + new Selection sidebar"
selection_js=$(curl -fsS "http://127.0.0.1:$PORT_FILE/lib/selection.js")
echo "$selection_js" | grep -q "notebookStore" \
    || fail "selection.js missing notebookStore identifier" "" "notebookStore present" "$LOGDIR/file.log"
echo "$selection_js" | grep -q "loadedSelectionStore" \
    || fail "selection.js missing loadedSelectionStore identifier" "" "loadedSelectionStore present" "$LOGDIR/file.log"
echo "$selection_js" | grep -q "LoadedSelectionView" \
    || fail "selection.js missing LoadedSelectionView" "" "LoadedSelectionView present" "$LOGDIR/file.log"
# Sanity: the old identifiers should be gone (modulo the unrelated
# `toggleSelection`, `isSelected`, `selectionCardTitle`, etc. which
# stay — only the workspace-store identifiers were renamed).
if echo "$selection_js" | grep -E "(\bselectionStore\b|\bSelectionView\b|\bpersistSelection\b)" >/dev/null; then
    fail "selection.js still has old workspace identifiers" \
         "$(echo "$selection_js" | grep -nE '(\bselectionStore\b|\bSelectionView\b|\bpersistSelection\b)' | head -3)" \
         "no old store identifiers" "$LOGDIR/file.log"
fi

PORT_AB_COMBINED=18504
COMBINED_AB_PARQUET="$LOGDIR/combined-ab.parquet"

echo "==> parquet combine --ab produces a combined-AB file"
# Use single-source fixtures so --ab can unambiguously assign each input
# to a side. The AB_base/AB_base_pin files are already multi-source and
# can't be used here.
./target/debug/rezolus parquet combine \
    site/viewer/data/ab_source_a.parquet \
    site/viewer/data/ab_source_b.parquet \
    -o "$COMBINED_AB_PARQUET" \
    --ab baseline=source-a experiment=source-b \
    > "$LOGDIR/combine-ab.log" 2>&1 \
    || fail "parquet combine --ab failed" "$(cat "$LOGDIR/combine-ab.log")" "exit 0" "$LOGDIR/combine-ab.log"

ab_meta=$(./target/debug/rezolus parquet metadata -i "$COMBINED_AB_PARQUET" 2>&1 | grep ab_containers || true)
[ -n "$ab_meta" ] || fail "metadata missing ab_containers" "$ab_meta" "ab_containers: …" "$LOGDIR/combine-ab.log"

echo "==> viewer auto-detects combined-AB and reports compare_mode"
./target/debug/rezolus view "$COMBINED_AB_PARQUET" \
    --listen 127.0.0.1:$PORT_AB_COMBINED \
    > "$LOGDIR/ab-combined.log" 2>&1 &
PID_AB_COMBINED=$!
trap 'kill $PID_UPLOAD $PID_FILE $PID_AB $PID_PROXY $PID_AB_COMBINED 2>/dev/null || true; wait 2>/dev/null || true' EXIT

wait_for_port $PORT_AB_COMBINED || {
    echo "--- log for combined-AB viewer ---"
    cat "$LOGDIR/ab-combined.log"
    exit 1
}

mode=$(curl -fsS "http://127.0.0.1:$PORT_AB_COMBINED/api/v1/mode")
eq "combined-AB combined_ab"  "$(echo "$mode" | jq -r .combined_ab)"  "true" "$LOGDIR/ab-combined.log"
eq "combined-AB compare_mode" "$(echo "$mode" | jq -r .compare_mode)" "true" "$LOGDIR/ab-combined.log"
eq "combined-AB loaded"       "$(echo "$mode" | jq -r .loaded)"       "true" "$LOGDIR/ab-combined.log"

echo
echo "ALL VIEWER SMOKE TESTS PASSED"
