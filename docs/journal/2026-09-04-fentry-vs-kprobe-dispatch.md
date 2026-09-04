# fentry vs kprobe — measuring the dispatch cost that decides the migration

- **Opened:** 2026-09-04
- **Status:** **MEASURED — GO for the hot single-hook kprobe samplers.** On a
  clean `tcp_sendmsg` (kernel 6.12, delta), an fentry probe costs **48 ns/call**
  vs a kprobe's **110 ns/call** — **fentry is 61 ns/call (56%) cheaper**. The
  actual sampler migration (BTF-gated fentry twins with kprobe fallback) is
  follow-on work; this entry lands the measurement, the tool, and the several
  corrections it took to get an honest number.
- **Driver:** principle 4 prefers `fentry/fexit` over `kprobe/kretprobe`, but no
  sampler had migrated and the in-tree bench measured the wrong thing. 8 samplers
  still use kprobe: `cpu/{tlb_flush,bandwidth,usage}`, `network/interfaces`,
  `tcp/{traffic,receive,retransmit,connect_latency}`.
- **Owner:** Brian Martin

## The result

`scripts/bench-fentry-vs-kprobe.sh`, matched hand-written BPF twins (identical
bodies, differing only in `SEC()` + entry macro), fixed-count single-threaded
workload, `perf stat -e task-clock`, baseline-subtracted, 10 reps, `tcp_sendmsg`
made **clean** first (production agent stopped):

| arm | cpu-ms/run | cost/call |
|---|---|---|
| baseline | 2549 | — |
| kprobe | 2988 | **109.7 ns** |
| fentry | 2743 | **48.4 ns** |

fentry **61.3 ns/call cheaper (56%)**, SNR clean (kprobe reps within 45 ms). A
fresh kprobe still routes through the kprobe framework (pt_regs, pre/post
handlers) *on top of* ftrace; fentry is a lean BPF trampoline — so the classic
advantage holds even on a `KPROBES_ON_FTRACE` kernel.

## Why this took four tries — the corrections are the value

Each attempt measured the wrong thing in a different way; recording them so the
next person does not re-walk the path.

1. **`run_time` is blind to dispatch.** The original script computed ns/call from
   `rezolus_bpf_run_time / rezolus_bpf_run_count`. The kernel populates
   `run_time_ns` by bracketing only the JITed program *body* (`sched_clock()`
   around `dfunc()` in `__bpf_prog_run`) — it excludes the trampoline / kprobe
   entry. fentry and kprobe run the **same body** and differ **only** in
   dispatch, so `run_time` brackets out the exact thing being compared and would
   report ~0 forever. The script also assumed fentry twins existed; none did, so
   both its runs loaded kprobe — it measured kprobe-vs-kprobe noise. Dispatch is
   only visible to an **external** clock.

2. **Saturation hides the cost as throughput, not time.** A time-based load
   pinned to N cores saturates them, so the probe cost shows as *fewer* calls,
   not more CPU-ms (task-clock pins at 100%). Fix: a **fixed-count**,
   single-threaded, unsaturated workload — the cost lands as extra CPU time.

3. **The hardware PMU is contended.** `perf -e cycles` returned `<not counted>`
   because rezolus's own `cpu_perf` sampler holds the cycle counters. Fix:
   `task-clock`, a **software** event needing no PMU — and CPU-ns/call is what we
   want anyway.

4. **A pre-existing probe on the target inverts the result** — the subtlest, and
   it produced a *confident wrong conclusion* before it was caught. With bpftrace
   the `kfunc` (fentry) and `kprobe` programs are not byte-identical (kfunc
   marshals typed args), conflating codegen with dispatch — so the bench moved to
   **matched hand-written twins**. But even then, with the production `tcp_traffic`
   agent hooking `tcp_sendmsg`, the numbers read **kprobe 52 ns / fentry 91 ns**
   — "fentry is worse", and an initial writeup concluded *don't migrate*. The
   cause: our kprobe **joined production's existing ftrace site** (cheap
   incremental attach) while our fentry installed a **fresh** trampoline, and the
   production probe added ~600 ns/send to every arm's baseline besides. Stopping
   the agent so `tcp_sendmsg` was clean flipped it to the truth above. The bench
   now warns when the target function is already probed.

## Caveats on the magnitude (not the direction)

- One function, one host, kernel 6.12. Direction (fentry cheaper for a standalone
  hot hook) is solid; the exact 61 ns is host/function-specific.
- **Single hook vs consolidated.** kprobes on one function **share** a single
  ftrace dispatch, so a second kprobe sampler is cheap incremental (measured: the
  piggybacked kprobe was ~half a fresh one). fentries each need their own
  trampoline. So the win is clearest for a hook one sampler owns; a hook
  consolidated across several samplers (principle 11) shifts the calculus.
- SNR is bounded by the probed path: `tcp_sendmsg`'s ~1.2 µs/send dwarfs a
  tens-of-ns probe, so many reps are needed. A cheaper probed function would
  sharpen it but is less representative.

## Deferred / next

- **Migrate the hot single-hook kprobe samplers to fentry twins** — BTF-gated
  (fentry needs BTF; keep kprobe as the CO-RE-only fallback, principle 2), a twin
  per program like the tp_btf/raw_tp pattern. Order by hook rate; `tcp_traffic`
  (`tcp_sendmsg`/`tcp_cleanup_rbuf`, per-message) first. Re-measure each on a
  clean function with this bench before/after.
- **Consolidated hooks stay kprobe or get measured separately** — where several
  samplers share a function, the shared-ftrace economics differ; do not assume
  the 61 ns applies.
