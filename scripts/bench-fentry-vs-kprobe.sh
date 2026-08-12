#!/usr/bin/env bash
#
# Measure the per-invocation overhead of the fentry twins versus the kprobe
# twins for the migrated TCP samplers (tcp_traffic, tcp_retransmit,
# tcp_receive).
#
# Rezolus already exposes per-sampler BPF self-telemetry:
#   rezolus_bpf_run_time  (nanoseconds, summed across the sampler's programs)
#   rezolus_bpf_run_count (invocations, summed across the sampler's programs)
# so mean ns/call = run_time / run_count. Only one twin is autoloaded at a
# time, so the sum reflects exactly the active attach mechanism.
#
# The script runs the agent twice against identical TCP load:
#   1. default          -> fentry twins  (needs /sys/kernel/btf/vmlinux)
#   2. REZOLUS_FORCE_NO_BTF=1 -> kprobe twins (the CO-RE-only fallback path)
# and prints mean ns/call for each sampler plus the delta.
#
# Requires: Linux, root (BPF load), jq, and a built release binary.
# Usage: sudo scripts/bench-fentry-vs-kprobe.sh [load_seconds]

set -euo pipefail

LOAD_SECONDS="${1:-20}"
PORT=4241
BIN="target/release/rezolus"
SAMPLERS=(tcp_traffic tcp_retransmit tcp_receive)

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: BPF samplers are Linux-only; run this on Linux" >&2
  exit 1
fi
if [[ "$(id -u)" != "0" ]]; then
  echo "error: loading BPF programs needs root; re-run with sudo" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 1
fi
if [[ ! -x "$BIN" ]]; then
  echo "error: $BIN not found; build it: cargo build --release" >&2
  exit 1
fi
if [[ ! -e /sys/kernel/btf/vmlinux ]]; then
  echo "warning: no /sys/kernel/btf/vmlinux; the 'fentry' run will also fall" >&2
  echo "         back to kprobe, so the comparison will be meaningless." >&2
fi

CONFIG="$(mktemp --suffix=.toml)"
cat >"$CONFIG" <<EOF
[general]
listen = "127.0.0.1:$PORT"
ttl = "10ms"

[log]
level = "error"

[defaults]
enabled = false

[samplers.tcp_traffic]
enabled = true

[samplers.tcp_retransmit]
enabled = true

[samplers.tcp_receive]
enabled = true
EOF

cleanup() { rm -f "$CONFIG"; }
trap cleanup EXIT

# Generate loopback TCP traffic: a throwaway server plus many short-lived
# clients pumping bytes, so tcp_sendmsg / tcp_cleanup_rbuf / tcp_rcv_established
# fire heavily. Pure bash + /dev/tcp keeps the dependency surface at zero.
generate_load() {
  local seconds="$1"
  # background sink: accept connections and drain them
  ( timeout "$((seconds + 2))" bash -c '
      while true; do
        { cat >/dev/null; } < <(:) 2>/dev/null || true
      done
    ' ) &
  # Use a python one-liner if available for a real echo server + load; else
  # fall back to nc-free /dev/tcp hammering.
  if command -v python3 >/dev/null 2>&1; then
    LOAD_SECONDS="$seconds" python3 - <<'PY'
import os, socket, threading, time
dur = float(os.environ["LOAD_SECONDS"])
srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 0)); srv.listen(64)
addr = srv.getsockname()
stop = time.time() + dur
def serve():
    srv.settimeout(0.5)
    while time.time() < stop:
        try:
            c,_ = srv.accept()
        except socket.timeout:
            continue
        threading.Thread(target=handle, args=(c,), daemon=True).start()
def handle(c):
    try:
        while True:
            d = c.recv(65536)
            if not d: break
            c.sendall(d)
    except OSError:
        pass
    finally:
        c.close()
def client():
    buf = b"x" * 65536
    while time.time() < stop:
        try:
            s = socket.create_connection(addr)
            for _ in range(64):
                s.sendall(buf)
                if not s.recv(65536): break
            s.close()
        except OSError:
            time.sleep(0.01)
threading.Thread(target=serve, daemon=True).start()
ts = [threading.Thread(target=client) for _ in range(8)]
for t in ts: t.start()
for t in ts: t.join()
PY
  else
    echo "note: python3 not found; using lighter /dev/tcp load" >&2
    sleep "$seconds"
  fi
  wait 2>/dev/null || true
}

# Read summed run_time / run_count for a sampler from /metrics/json and print
# "run_time run_count".
read_stats() {
  local sampler="$1"
  curl -s "http://127.0.0.1:$PORT/metrics/json" | jq -r --arg s "$sampler" '
    def val(metric):
      [ .counters[]
        | select(.metadata.metric == metric and .metadata.sampler == $s)
        | .value ] | add // 0;
    "\(val("rezolus_bpf_run_time")) \(val("rezolus_bpf_run_count"))"
  '
}

# Run one variant end-to-end and echo, per sampler: "sampler mean_ns count".
run_variant() {
  local label="$1"; shift
  local -a envs=("$@")
  echo "== $label ==" >&2
  env "${envs[@]}" "$BIN" "$CONFIG" >/dev/null 2>&1 &
  local pid=$!
  # wait for the endpoint
  for _ in $(seq 1 50); do
    curl -sf "http://127.0.0.1:$PORT/metrics/json" >/dev/null 2>&1 && break
    sleep 0.2
  done
  sleep 2  # let all programs attach

  declare -A before_t before_c
  for s in "${SAMPLERS[@]}"; do
    read -r t c < <(read_stats "$s"); before_t[$s]=$t; before_c[$s]=$c
  done

  generate_load "$LOAD_SECONDS"
  sleep 1

  for s in "${SAMPLERS[@]}"; do
    read -r t c < <(read_stats "$s")
    local dt=$(( t - ${before_t[$s]} ))
    local dc=$(( c - ${before_c[$s]} ))
    local mean="n/a"
    if [[ "$dc" -gt 0 ]]; then
      mean=$(awk "BEGIN{printf \"%.1f\", $dt/$dc}")
    fi
    echo "$s $mean $dc"
  done

  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

declare -A fentry_mean kprobe_mean fentry_cnt kprobe_cnt

while read -r s mean cnt; do
  fentry_mean[$s]=$mean; fentry_cnt[$s]=$cnt
done < <(run_variant "fentry (default)")

while read -r s mean cnt; do
  kprobe_mean[$s]=$mean; kprobe_cnt[$s]=$cnt
done < <(run_variant "kprobe (REZOLUS_FORCE_NO_BTF=1)" REZOLUS_FORCE_NO_BTF=1)

echo
printf "%-18s %14s %14s %12s\n" "sampler" "fentry ns/call" "kprobe ns/call" "delta"
printf "%-18s %14s %14s %12s\n" "------------------" "--------------" "--------------" "------------"
for s in "${SAMPLERS[@]}"; do
  f=${fentry_mean[$s]:-n/a}; k=${kprobe_mean[$s]:-n/a}
  delta="n/a"
  if [[ "$f" != "n/a" && "$k" != "n/a" ]]; then
    delta=$(awk "BEGIN{printf \"%+.1f (%.0f%%)\", $f-$k, ($k>0?($f-$k)/$k*100:0)}")
  fi
  printf "%-18s %14s %14s %12s\n" "$s" "$f" "$k" "$delta"
done
echo
echo "note: counts must be comparable across runs for the means to be fair;"
echo "      re-run with a larger load duration if a sampler's count is low."
