#!/usr/bin/env bash
#
# Measure the per-invocation DISPATCH cost of fentry vs kprobe on a hot kernel
# function, to decide whether migrating a kprobe-based sampler to an fentry twin
# is worth it (principle 4 prefers fentry/fexit over kprobe/kretprobe).
#
# Why not rezolus_bpf_run_time / run_count
# ----------------------------------------
# An earlier version computed ns/call from `rezolus_bpf_run_time /
# rezolus_bpf_run_count`. That is WRONG for this question: the kernel populates
# run_time_ns by bracketing only the JITed program body (sched_clock() around
# dfunc() in __bpf_prog_run), so it excludes the trampoline / kprobe-trap entry.
# fentry and kprobe run the SAME body and differ ONLY in how the kernel gets
# there (a BPF trampoline call vs an int3 breakpoint exception) -- so run_time
# brackets out the exact thing being compared and reports ~0 delta regardless.
# The dispatch cost is only visible from outside the program.
#
# Method
# ------
# Two HAND-WRITTEN BPF programs with byte-identical bodies (one map increment),
# differing only in SEC() + entry macro -- i.e. only in the attach mechanism:
#   SEC("kprobe/<fn>")  int BPF_KPROBE(prog) { bump(); return 0; }
#   SEC("fentry/<fn>")  int BPF_PROG(prog)   { bump(); return 0; }
# This avoids the bpftrace pitfall of kfunc vs kprobe emitting DIFFERENT bodies
# (kfunc marshals typed args), which would conflate codegen with dispatch.
#
# A fixed-count, single-threaded workload calls <fn> a known N times (default: N
# sends on a loopback TCP socket -> N tcp_sendmsg), pinned to one CPU. We time
# the workload's own CPU cost with `perf stat -e task-clock` under three states:
#   baseline  nothing of ours attached
#   kprobe    kp.o loaded + autoattached
#   fentry    fe.o loaded + autoattached
# Unsaturated + single-threaded, so the probe cost lands as extra CPU time
# rather than reduced throughput; identical bodies, so the delta is dispatch:
#   kprobe cost/call = (kprobe_cpu_ns - baseline_cpu_ns) / N
#   fentry cost/call = (fentry_cpu_ns - baseline_cpu_ns) / N
#   dispatch delta   = kprobe cost/call - fentry cost/call
#
# task-clock is a SOFTWARE event (no PMU): an always-on agent (rezolus's own
# cpu_perf sampler) typically holds the hardware cycle counters, and
# `perf -e cycles` then returns "<not counted>". task-clock needs no PMU.
#
# Requires: Linux, root, clang, bpftool (>=5.16, for `autoattach`), perf,
# python3, taskset, a mounted bpffs, /sys/kernel/btf/vmlinux, and libbpf's
# bpf_helpers.h (searched below; override with LIBBPF_INCLUDE=/path).
#
# Usage: sudo scripts/bench-fentry-vs-kprobe.sh [function] [calls] [reps]
#   function  hot kernel function (default: tcp_sendmsg). For a non-tcp_sendmsg
#             target, adapt the workload to call it a known number of times.
#   calls     sends per workload run (default: 2000000)
#   reps      measured repetitions per arm (default: 5)

set -euo pipefail

FN="${1:-tcp_sendmsg}"
CALLS="${2:-2000000}"
REPS="${3:-5}"

die() { echo "error: $*" >&2; exit 1; }
[[ "$(uname -s)" == "Linux" ]] || die "Linux only"
[[ "$(id -u)" == "0" ]] || die "needs root; re-run with sudo"
for c in clang perf python3 taskset; do command -v "$c" >/dev/null 2>&1 || die "$c is required"; done
BPFTOOL="$(command -v bpftool || echo /usr/sbin/bpftool)"
[[ -x "$BPFTOOL" ]] || die "bpftool not found (try: apt-get install bpftool)"
"$BPFTOOL" prog help 2>&1 | grep -q autoattach || die "bpftool too old (needs 'autoattach')"
[[ -e /sys/kernel/btf/vmlinux ]] || die "no /sys/kernel/btf/vmlinux; fentry needs BTF"
mount | grep -q 'type bpf' || die "bpffs not mounted (mount -t bpf bpf /sys/fs/bpf)"

# Locate libbpf's bpf_helpers.h: env override, system, or the copy libbpf-sys
# vendors into the cargo registry (present after any rezolus build).
LIBBPF_INCLUDE="${LIBBPF_INCLUDE:-}"
if [[ -z "$LIBBPF_INCLUDE" ]]; then
  # Under sudo, $HOME is root's; also probe the invoking user's home.
  for d in /usr/include/bpf /usr/local/include/bpf \
           "$HOME"/.cargo/registry/src/*/libbpf-sys-*/libbpf/src \
           ${SUDO_USER:+/home/"$SUDO_USER"}/.cargo/registry/src/*/libbpf-sys-*/libbpf/src \
           /home/*/.cargo/registry/src/*/libbpf-sys-*/libbpf/src \
           /root/.cargo/registry/src/*/libbpf-sys-*/libbpf/src; do
    [[ -f "$d/bpf_helpers.h" ]] && { LIBBPF_INCLUDE="$d"; break; }
  done
fi
[[ -n "$LIBBPF_INCLUDE" && -f "$LIBBPF_INCLUDE/bpf_helpers.h" ]] \
  || die "bpf_helpers.h not found; set LIBBPF_INCLUDE=/path/to/libbpf/src"

WORK="$(mktemp -d)"
SRV_PID=""
cleanup() {
  [[ -n "$SRV_PID" ]] && kill "$SRV_PID" 2>/dev/null || true
  sudo rm -rf /sys/fs/bpf/fkbench_kp /sys/fs/bpf/fkbench_fe 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# --- matched BPF twins: identical body, only the mechanism differs -----------
"$BPFTOOL" btf dump file /sys/kernel/btf/vmlinux format c > "$WORK/vmlinux.h"
cat > "$WORK/kp.bpf.c" <<C
#include "vmlinux.h"
#include "bpf_helpers.h"
#include "bpf_tracing.h"
struct { __uint(type, BPF_MAP_TYPE_ARRAY); __uint(max_entries, 1); __type(key, __u32); __type(value, __u64); } cnt SEC(".maps");
SEC("kprobe/$FN")
int BPF_KPROBE(prog) { __u32 k = 0; __u64 *v = bpf_map_lookup_elem(&cnt, &k); if (v) __sync_fetch_and_add(v, 1); return 0; }
char _license[] SEC("license") = "GPL";
C
sed -e "s|kprobe/$FN|fentry/$FN|" -e "s|BPF_KPROBE(prog)|BPF_PROG(prog)|" \
    "$WORK/kp.bpf.c" > "$WORK/fe.bpf.c"
for t in kp fe; do
  clang -O2 -g -target bpf -D__TARGET_ARCH_x86 -I "$LIBBPF_INCLUDE" \
    -c "$WORK/$t.bpf.c" -o "$WORK/$t.o" || die "compile $t failed"
done

# --- fixed-count workload: N sends -> N <fn> ---------------------------------
cat > "$WORK/server.py" <<'PY'
import socket, threading, sys
srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 0)); srv.listen(16)
open(sys.argv[1], "w").write(str(srv.getsockname()[1]))
def handle(c):
    try:
        while c.recv(1 << 20): pass
    except OSError: pass
    finally: c.close()
while True:
    c, _ = srv.accept()
    threading.Thread(target=handle, args=(c,), daemon=True).start()
PY
cat > "$WORK/client.py" <<'PY'
import socket, sys
port = int(open(sys.argv[1]).read()); n = int(sys.argv[2])
s = socket.create_connection(("127.0.0.1", port))
buf = b"x" * 256
for _ in range(n): s.send(buf)
s.close()
PY
python3 "$WORK/server.py" "$WORK/port" & SRV_PID=$!
for _ in $(seq 1 50); do [[ -s "$WORK/port" ]] && break; sleep 0.1; done
[[ -s "$WORK/port" ]] || die "server did not come up"

# Read the counter map value (0 if not loaded). Confirms the probe fired.
count_val() { "$BPFTOOL" map dump name cnt 2>/dev/null | awk -F'"value": ' '/value/{gsub(/[ ,}]/,"",$2);print $2;exit}'; }

# One measured run under $kind. Prints "cpu_ms invocations".
run_arm() {
  local kind="$1" perf_out="$WORK/perf" pin="/sys/fs/bpf/fkbench_$kind"
  local before=0 after=0
  if [[ "$kind" != "none" ]]; then
    local obj="$WORK/kp.o"; [[ "$kind" == "fentry" ]] && obj="$WORK/fe.o"
    "$BPFTOOL" prog loadall "$obj" "$pin" autoattach 2>/dev/null || die "loadall $kind failed"
    sleep 2  # settle
    before="$(count_val)"; before="${before:-0}"
  fi
  taskset -c 1 perf stat -e task-clock -x, -o "$perf_out" \
    -- python3 "$WORK/client.py" "$WORK/port" "$CALLS" 2>/dev/null
  local ms invs=0
  ms="$(awk -F, '$3=="task-clock"{print $1}' "$perf_out")"
  if [[ "$kind" != "none" ]]; then
    after="$(count_val)"; after="${after:-0}"; invs=$(( after - before ))
    sudo rm -rf "$pin"
  fi
  echo "$ms $invs"
}

echo "== fentry vs kprobe dispatch on $FN ($CALLS sends/run, $REPS reps) ==" >&2

# Warn if $FN is ALREADY probed (e.g. by a running rezolus agent). Our kprobe
# would then join that existing ftrace site -- cheap incremental attach -- while
# our fentry installs a fresh trampoline, so kprobe looks artificially cheap and
# the delta is wrong. Measured: with a live agent hooking tcp_sendmsg, kprobe
# read 52ns and fentry 91ns (fentry "worse"); stopping the agent so the function
# was clean flipped it to kprobe 110ns / fentry 48ns (fentry 56% cheaper, the
# truth). For a standalone dispatch number, stop anything hooking $FN first.
pre="$("$BPFTOOL" link show 2>/dev/null | grep -c "$FN" || true)"
if [[ "${pre:-0}" -gt 0 ]]; then
  echo "WARNING: $FN already has $pre probe(s) attached (a running agent?)." >&2
  echo "         Our kprobe will join that ftrace site (cheap incremental) while" >&2
  echo "         fentry installs fresh -- the delta will be WRONG. Stop whatever" >&2
  echo "         hooks $FN for a standalone number (e.g. systemctl stop rezolus)." >&2
fi

declare -A MS
for rep in $(seq 1 "$REPS"); do
  for kind in none kprobe fentry; do
    read -r ms invs < <(run_arm "$kind")
    MS[$kind]="${MS[$kind]:-} $ms"
    echo "  rep$rep $kind: cpu_ms=$ms invocations=$invs" >&2
  done
done

mean() { awk '{s=0;n=0;for(i=1;i<=NF;i++){s+=$i;n++} printf "%.2f", s/n}' <<<"$1"; }
bm=$(mean "${MS[none]}"); km=$(mean "${MS[kprobe]}"); fm=$(mean "${MS[fentry]}")

echo
printf "%-10s %14s\n" "arm" "cpu-ms/run"
printf "%-10s %14s\n" "baseline" "$bm"
printf "%-10s %14s\n" "kprobe" "$km"
printf "%-10s %14s\n" "fentry" "$fm"
echo
awk -v bm="$bm" -v km="$km" -v fm="$fm" -v n="$CALLS" 'BEGIN{
  kpc=(km-bm)*1e6/n; fec=(fm-bm)*1e6/n;
  printf "kprobe  cost/call: %8.1f ns\n", kpc;
  printf "fentry  cost/call: %8.1f ns\n", fec;
  printf "dispatch delta   : %8.1f ns/call  (kprobe - fentry)\n", kpc-fec;
  if (kpc>0) printf "                   %8.1f%% cheaper with fentry\n", (kpc-fec)/kpc*100;
}'
echo
echo "note: identical hand-written bodies, so the delta is dispatch alone. A"
echo "      real sampler adds its own body cost on top of BOTH, unchanged by"
echo "      the swap. SNR: the empty probe is tens of ns vs a ~1.2us/send"
echo "      python base; raise calls/reps if a signal sinks into run variance."
