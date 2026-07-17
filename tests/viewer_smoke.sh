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
# Examples include `gen_ab_fixtures`, the AB smoke section's fixture
# generator — kept out of the production binary.
cargo build --bin rezolus --example gen_ab_fixtures 2>&1 | tail -3

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

echo "==> POST /api/v1/save_with_selection in two-file compare returns a tarball"
SAVE_RESP="$LOGDIR/two-file-save.parquet.ab.tar"
curl -fsS -X POST \
    -H 'Content-Type: application/json' \
    -d '{"entries":[{"chartId":"smoke","section":"overview","sectionName":"Overview","groupName":"smoke","promql_query":"cpu_cores","chartOpts":{}}],"trim_columns":false}' \
    "http://127.0.0.1:$PORT_FILE/api/v1/save_with_selection" \
    -o "$SAVE_RESP"

ab_listing=$(tar tf "$SAVE_RESP" 2>&1 | tr '\n' ' ')
case "$ab_listing" in
    *baseline.parquet*experiment.parquet*ab.json*|*baseline.parquet*ab.json*experiment.parquet*|*experiment.parquet*baseline.parquet*ab.json*|*experiment.parquet*ab.json*baseline.parquet*|*ab.json*baseline.parquet*experiment.parquet*|*ab.json*experiment.parquet*baseline.parquet*) : ;;
    *) fail "two-file compare save: tarball missing expected entries" "$ab_listing" "baseline.parquet, experiment.parquet, ab.json" "$LOGDIR/file.log" ;;
esac

ab_manifest=$(tar xfO "$SAVE_RESP" ab.json)
eq "two-file compare save: manifest version" \
    "$(echo "$ab_manifest" | jq -r .version)" "1" "$LOGDIR/file.log"
[ -n "$(echo "$ab_manifest" | jq -r .baseline.alias)" ] \
    || fail "two-file compare save: empty baseline.alias" "" "non-empty" "$LOGDIR/file.log"
[ -n "$(echo "$ab_manifest" | jq -r .experiment.alias)" ] \
    || fail "two-file compare save: empty experiment.alias" "" "non-empty" "$LOGDIR/file.log"

echo "==> reload the saved tarball, save again, manifest survives round-trip"
PORT_ROUNDTRIP=18506
./target/debug/rezolus view "$SAVE_RESP" \
    --listen 127.0.0.1:$PORT_ROUNDTRIP \
    > "$LOGDIR/roundtrip.log" 2>&1 &
PID_ROUNDTRIP=$!
trap 'kill $PID_UPLOAD $PID_FILE $PID_AB $PID_PROXY $PID_ROUNDTRIP 2>/dev/null || true; wait 2>/dev/null || true' EXIT

wait_for_port $PORT_ROUNDTRIP || {
    echo "--- log for round-trip viewer ---"
    cat "$LOGDIR/roundtrip.log"
    exit 1
}

ROUNDTRIP_RESP="$LOGDIR/two-file-save-2.parquet.ab.tar"
curl -fsS -X POST \
    -H 'Content-Type: application/json' \
    -d '{"entries":[{"chartId":"smoke","section":"overview","sectionName":"Overview","groupName":"smoke","promql_query":"cpu_cores","chartOpts":{}}],"trim_columns":false}' \
    "http://127.0.0.1:$PORT_ROUNDTRIP/api/v1/save_with_selection" \
    -o "$ROUNDTRIP_RESP"

# Reload-then-save should carry the synthesizer's first-pass aliases.
first_baseline_alias=$(tar xfO "$SAVE_RESP" ab.json | jq -r .baseline.alias)
first_experiment_alias=$(tar xfO "$SAVE_RESP" ab.json | jq -r .experiment.alias)
roundtrip_baseline_alias=$(tar xfO "$ROUNDTRIP_RESP" ab.json | jq -r .baseline.alias)
roundtrip_experiment_alias=$(tar xfO "$ROUNDTRIP_RESP" ab.json | jq -r .experiment.alias)
eq "round-trip baseline alias preserved" \
    "$roundtrip_baseline_alias" "$first_baseline_alias" "$LOGDIR/roundtrip.log"
eq "round-trip experiment alias preserved" \
    "$roundtrip_experiment_alias" "$first_experiment_alias" "$LOGDIR/roundtrip.log"

kill $PID_ROUNDTRIP 2>/dev/null || true
wait $PID_ROUNDTRIP 2>/dev/null || true

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
selection_js=$(curl -fsS "http://127.0.0.1:$PORT_FILE/lib/selection/selection.js")
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
COMBINED_AB_TAR="$LOGDIR/combined-ab.parquet.ab.tar"

echo "==> parquet combine --ab produces a *.parquet.ab.tar tarball"
# Single-source fixtures so --ab can unambiguously assign each input to a
# side. Generated at test time (see `examples/gen_ab_fixtures.rs`) rather
# than checked in — keeps the tree free of binary blobs whose meaning
# would silently drift from the writer.
AB_A="$LOGDIR/ab_source_a.parquet"
AB_B="$LOGDIR/ab_source_b.parquet"
./target/debug/examples/gen_ab_fixtures "$AB_A" "$AB_B" \
    > "$LOGDIR/gen-ab.log" 2>&1 \
    || fail "gen_ab_fixtures failed" "$(cat "$LOGDIR/gen-ab.log")" "exit 0" "$LOGDIR/gen-ab.log"
./target/debug/rezolus parquet combine \
    "$AB_A" \
    "$AB_B" \
    -o "$COMBINED_AB_TAR" \
    --ab baseline=source-a experiment=source-b \
    > "$LOGDIR/combine-ab.log" 2>&1 \
    || fail "parquet combine --ab failed" "$(cat "$LOGDIR/combine-ab.log")" "exit 0" "$LOGDIR/combine-ab.log"

ab_listing=$(tar tf "$COMBINED_AB_TAR" 2>&1 | tr '\n' ' ')
case "$ab_listing" in
    *baseline.parquet*experiment.parquet*ab.json*|*baseline.parquet*ab.json*experiment.parquet*|*experiment.parquet*baseline.parquet*ab.json*|*experiment.parquet*ab.json*baseline.parquet*|*ab.json*baseline.parquet*experiment.parquet*|*ab.json*experiment.parquet*baseline.parquet*) : ;;
    *) fail "combined-AB tarball missing expected entries" "$ab_listing" "baseline.parquet, experiment.parquet, ab.json" "$LOGDIR/combine-ab.log" ;;
esac

echo "==> viewer auto-detects combined-AB tarball and reports compare_mode"
./target/debug/rezolus view "$COMBINED_AB_TAR" \
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

echo "==> Save as Report produces a trimmed parquet"
PORT_REPORT=18505
# Pin one chart and POST a tiny selection that touches one column.
SELECTION=$(cat <<JSON
{
  "version": 1,
  "saved_at": "2026-05-12T00:00:00Z",
  "entries": [
    {"chartId": "cpu_cores", "section": "/cpu", "promql_query": "cpu_cores"}
  ]
}
JSON
)
REPORT_PARQUET="$LOGDIR/report.parquet"
curl -fsS -X POST -H 'content-type: application/json' \
     -d "$SELECTION" \
     "http://127.0.0.1:$PORT_FILE/api/v1/save_with_selection" \
     --output "$REPORT_PARQUET" \
     || fail "save_with_selection failed" "" "exit 0" "$LOGDIR/file.log"
[ -s "$REPORT_PARQUET" ] || fail "report parquet is empty" "0" ">0" "$LOGDIR/file.log"

echo "==> trimmed report loads in a fresh viewer with report mode active"
./target/debug/rezolus view "$REPORT_PARQUET" \
    --listen 127.0.0.1:$PORT_REPORT \
    > "$LOGDIR/report.log" 2>&1 &
PID_REPORT=$!
trap 'kill $PID_UPLOAD $PID_FILE $PID_AB $PID_PROXY $PID_AB_COMBINED $PID_REPORT 2>/dev/null || true; wait 2>/dev/null || true' EXIT

wait_for_port $PORT_REPORT || {
    echo "--- log for report viewer ---"
    cat "$LOGDIR/report.log"
    exit 1
}

mode=$(curl -fsS "http://127.0.0.1:$PORT_REPORT/api/v1/mode")
eq "report mode flag"          "$(echo "$mode" | jq -r .report)"        "true" "$LOGDIR/report.log"
sections=$(curl -fsS "http://127.0.0.1:$PORT_REPORT/api/v1/sections")
section_count=$(echo "$sections" | jq -r '.data.sections | length')
eq "report mode section count" "$section_count"                          "0"    "$LOGDIR/report.log"

# Simple-capture mode: non-Rezolus parquet (Prometheus source).
# Fixture regeneration:
#   cat > /tmp/metrics <<'PROM'
#   # HELP http_requests_total Total HTTP requests
#   # TYPE http_requests_total counter
#   http_requests_total{method="get",route="/"} 42
#   http_requests_total{method="post",route="/api"} 7
#   # HELP queue_depth Pending items
#   # TYPE queue_depth gauge
#   queue_depth 3
#   PROM
#   ( cd /tmp && python3 -m http.server 18651 >/dev/null 2>&1 & echo $! > /tmp/prom.pid )
#   sleep 1
#   target/debug/rezolus record --interval 1s --duration 3s \
#       http://127.0.0.1:18651/metrics site/viewer/data/simple_capture.parquet
#   kill "$(cat /tmp/prom.pid)"
PORT_SIMPLE=18507
PARQUET_SIMPLE=site/viewer/data/simple_capture.parquet
echo "==> simple-capture: non-Rezolus parquet fixture"
./target/debug/rezolus view "$PARQUET_SIMPLE" \
    --listen 127.0.0.1:$PORT_SIMPLE \
    > "$LOGDIR/simple.log" 2>&1 &
PID_SIMPLE=$!
trap 'kill $PID_UPLOAD $PID_FILE $PID_AB $PID_PROXY $PID_AB_COMBINED $PID_REPORT $PID_SIMPLE 2>/dev/null || true; wait 2>/dev/null || true' EXIT

wait_for_port $PORT_SIMPLE || {
    echo "--- log for simple-capture viewer ---"
    cat "$LOGDIR/simple.log"
    exit 1
}

BASE="http://127.0.0.1:$PORT_SIMPLE"

echo "==> simple-capture: /api/v1/metrics returns non-empty typed catalog"
metrics_json=$(curl -fsS "$BASE/api/v1/metrics")
echo "$metrics_json" | jq -e '.metrics | length > 0' >/dev/null \
    || fail "simple-capture metrics catalog empty" "$metrics_json" "non-empty .metrics" "$LOGDIR/simple.log"
echo "$metrics_json" | jq -e '.metrics[0] | has("name") and has("metric_type") and has("series_count")' >/dev/null \
    || fail "simple-capture metrics entry missing required fields" "$(echo "$metrics_json" | jq '.metrics[0]')" "name+metric_type+series_count" "$LOGDIR/simple.log"

echo "==> simple-capture: /api/v1/sections has /source/ entry, no /cpu built-in"
sections_json=$(curl -fsS "$BASE/api/v1/sections")
echo "$sections_json" | jq -e '[.data.sections[].route] | any(startswith("/source/"))' >/dev/null \
    || fail "simple-capture sections missing /source/ route" "$sections_json" "route starting with /source/" "$LOGDIR/simple.log"
echo "$sections_json" | jq -e '[.data.sections[].route] | any(. == "/cpu") | not' >/dev/null \
    || fail "simple-capture sections has /cpu built-in (should be suppressed)" "$sections_json" "no /cpu route" "$LOGDIR/simple.log"

echo "==> simple-capture: /api/v1/timestamps returns non-empty timestamps array"
timestamps_json=$(curl -fsS "$BASE/api/v1/timestamps?source=127.0.0.1-18651")
echo "$timestamps_json" | jq -e '.timestamps | length > 0' >/dev/null \
    || fail "simple-capture timestamps empty" "$timestamps_json" "non-empty .timestamps" "$LOGDIR/simple.log"

echo
echo "ALL VIEWER SMOKE TESTS PASSED"
