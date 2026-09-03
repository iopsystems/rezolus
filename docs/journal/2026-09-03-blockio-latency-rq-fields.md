# blockio latency — drop the in-flight map, read the kernel's own rq timestamps

- **Opened:** 2026-09-03
- **Status:** **SHIPPED, measured.** The three block-IO latency phases
  (`blockio_queue_latency`, `blockio_device_latency`, `blockio_total_latency`,
  shipped in #1124) are now computed from the kernel's own `struct request`
  timestamps at `block_rq_complete` alone, rather than bracketing
  `block_rq_insert` → `block_rq_issue` → `block_rq_complete` with a
  pointer-keyed hash map. One probe replaces three; the map is gone.
  **+10.2% throughput / −505 ns/IO** under saturation, device/total
  distributions bit-identical to the old method (see *Results*). Every
  assumption behind the swap was probed on real hardware first — see *Go/no-go*.
- **Driver:** a true-cost measurement of the map (below) found the sampler's
  dominant per-IO cost is the `start` hash, not the probe dispatch.
- **Owner:** Brian Martin

## Why

`blockio_latency` (now `blockio_device_latency`) tracked each in-flight request
in a `BPF_MAP_TYPE_HASH` keyed by `struct request *`: `insert` wrote a stamp,
`issue` read+updated it, `complete` read+deleted it — three hash operations per
IO on a shared 65536-entry map, plus two extra probe attaches.

A true-cost measurement of that design (hv01, 32-core, kernel 6.12,
`null_blk` mq-deadline, fio randread pinned to 8 isolated guest-free cores,
`perf stat -C`, sampler-off vs sampler-on, production agent's own `block_rq_*`
programs held identical in both arms so they cancel):

- Attaching the sampler dropped throughput **12.7%** (339,398 → 296,208 IOPS,
  4 reps, sd < 1k).
- Marginal cost **~9,400 cycles/IO** (≈ 2,900 ns/IO) at the measured 3.22 GHz.
- Critically, the marginal work ran at **0.34 IPC** — 3,165 added
  instructions/IO, the *same* low IPC as the whole IO path. That is not
  dispatch (a trampoline is tens of instructions at high IPC); it is
  **memory-stall-bound**, i.e. the shared hash doing insert+lookup+delete with
  cross-core cacheline bouncing on its buckets/locks.

So the cost to remove is the *map*, not the probes. (The differential cost of
the phase split itself, branch-vs-main, was a separate, clean **+31.2 ns/IO**
measured on `delta` for #1124; that number is unaffected by this change.)

## The idea

The kernel already stores per-request timestamps *in the request*
(`src/agent/bpf/x86_64/vmlinux.h:24800-24802`):

```c
u64 alloc_time_ns;      // request allocation began (before the tag-wait)
u64 start_time_ns;      // request init (≈ enters the queue)
u64 io_start_time_ns;   // dispatched to the device
```

That is co-located per-request state with no side table and no index — the one
genuinely hashless option (see *Why not a hashless map* below). At
`block_rq_complete` alone:

- device = `now − io_start_time_ns`
- queue  = `io_start_time_ns − start_time_ns`
- total  = `now − start_time_ns`

`alloc_time_ns` is a distinct, earlier stamp — `start_time_ns − alloc_time_ns`
is the wait for a free request tag (a deeper backpressure signal than queue
wait). It is **not** derivable from the other two, and is **0% populated**
without an active iocost/iolatency controller, so we do not use it. Noted here
only so the next reader does not rediscover it: if such a controller is active,
a fourth "tag-allocation wait" phase is available for free.

## Go/no-go — every assumption probed on real hardware (delta / hv01)

Measured with `bpftrace` reading the fields at `block_rq_complete` under fio,
against a real 14.6 TB disk (through `md0`) and a `null_blk` device.

1. **Field population.** `start_time_ns` **100%** (1,694,394/1,694,394);
   `io_start_time_ns` **99.998%** (32 zeros, all flush-like writes) on real IO;
   **99.9998%** on null_blk. `alloc_time_ns` **0%**. **PASS** for the two we
   need.
2. **Value equivalence vs the current map method** (both run on the same
   requests, 1.74M IOs): device mean 399.8 µs (current) vs 402.6 µs (fields),
   **+0.7%**; queue mean 61.3 µs vs 61.4 µs, **+0.2%**; tail distributions match
   bucket-for-bucket. Queue is in fact *cleaner* with fields: the current method
   shows a spurious 512 ns–2 µs floor on every request (the cost of traversing
   between two tracepoints), while fields report a true **0** for the 933k
   requests that never queued. **PASS.**
3. **Clock domain.** `start_time_ns`/`io_start_time_ns` are the same
   `CLOCK_MONOTONIC` base as `bpf_ktime_get_ns()` — if they were not, device
   latency could not match within 0.7%. **PASS.**
4. **Partial completions (double-count risk).** `block_rq_complete` can fire
   once per *partial* completion (`blk_update_request`), and without the map's
   delete-on-first there is no implicit dedup. But the completion is
   self-identifying: at the tracepoint (before `__data_len` is decremented) the
   *final* completion is the one where `nr_bytes == rq->__data_len`; a partial
   has `nr_bytes < __data_len`. Measured: **100.0%** of 112,540 completions had
   `nr_bytes == __data_len`, 0 partials, 0 `>` cases — the marker is exact for
   every genuine single completion, and the equivalence is tight because
   `blk_update_request` returns "more to do" precisely when
   `nr_bytes < remaining`. Recording only when `nr_bytes == __data_len` gives
   stateless dedup. **PASS** (the `<` branch could not be exercised — real
   partials need SCSI residual / specific drivers we cannot force — but it
   follows from the kernel logic and the single-shot case confirms the field
   semantics).
5. **Requeue.** 0 requeues observed on the test workloads; `block_rq_requeue`
   is left unhooked. A requeue re-dispatches, updating `io_start_time_ns`, so
   device would measure from the *last* dispatch — the same direction as the old
   code's `issue`-overwrite behaviour. **PASS with a caveat** (not stress-tested).

**Not a single-fire completion hook.** We checked whether a stateless probe on
a once-per-request completion function could avoid the `__data_len` gate:
`blk_mq_end_request` fired **10 times against 1.3M** `block_rq_complete` events —
modern kernels complete through the batch path — so there is no such hook. The
`nr_bytes == __data_len` gate is the dedup.

### Field-population gating, and why we neither add nor need a blk-stat consumer

`io_start_time_ns` is populated only when a blk-stat consumer sets
`RQF_IO_STAT | RQF_STATS` on the request. We **cannot** register such a consumer
from Rezolus: it is kernel code (a module), and Rezolus is CO-RE / no-module
(principle 2); BPF cannot set the queue/request flags. We also do not **need**
to: writeback throttling (`CONFIG_BLK_WBT=y`) is a blk-stat consumer active by
default on every physical block queue — measured `wbt_lat_usec=75000` on the
fleet's sda/sdb — so the field is populated independent of the sysfs `iostats`
knob (confirmed: `io_start_time_ns` stays populated with `iostats=0`). The only
userspace lever is writing `iostats=1`, which mutates the monitored system and
is out of character for a passive agent; we do not do it.

Robustness is handled in-handler instead: **gate device/queue on
`io_start_time_ns != 0`.** A device with no stat consumer at all (rare) then
yields empty device/queue while `total` still works from the less-gated
`start_time_ns` — graceful degradation, no wrong data, self-correcting per
request, no startup mutation.

## Why not a hashless map

`struct request` offers `tag`/`internal_tag` as bounded ids, but they do not
coerce to a usable array index: tags are **per-hardware-queue** (not globally
unique across devices sharing the tracepoint), **not stable across the request
lifetime** (`internal_tag` at alloc only with a scheduler; `tag` at dispatch;
neither valid at every hook for every config), and **large and
device-dependent** in range. A per-CPU tag array would remove the cross-core
bounce but only works when completion lands on the issuing CPU
(`rq_affinity`-dependent) — a miss loses the sample. And BPF has no
`request_local_storage` (it has sk/task/inode/cgroup storage, not request), so
there is no way to stash BPF-managed state on the request itself. The only
truly hashless per-request storage is inside `struct request` — which is exactly
the kernel timestamps this effort reads.

## What changed

`src/agent/samplers/blockio/linux/latency/mod.bpf.c`:
- Removed the `start` hash map and `struct rq_stamps`.
- Removed `block_rq_insert` and `block_rq_issue` programs and their handlers;
  only `block_rq_complete` (btf + raw_tp twins) remains.
- `handle_block_rq_complete` reads `start_time_ns`/`io_start_time_ns`/`__data_len`
  via CO-RE, gates on `nr_bytes == __data_len` (final completion) and
  per-phase `plausible_span`, and records device/queue/total.

`…/mod.rs`: dropped the insert/issue map arms, the removed programs from
`disabled_programs` and `log_prog_instructions`, and updated the module doc.

`stats.rs`, the acquisition groups, the dashboard, and the metric names are
**unchanged** — this is a mechanism swap behind identical output.

## Results

Built the branch and `main` on `delta` and A/B'd them under identical load.

- **Verifier & load:** the single `block_rq_complete` program (451 instructions,
  up from the old complete's 387 because it now does the field reads and gates)
  loads clean — sampler reports healthy. The old build loaded three programs
  (insert 27, issue 357, complete 387 = **771 instructions**); the new is **451
  in one program**.
- **Overhead (the point):** `null_blk` mq-deadline, fio randread pinned to 4
  cores, `perf stat -C`, 3 reps, new-vs-old:
  - throughput **696,287 → 767,347 IOPS, +10.2%** (sd < 4.2k).
  - **29,895 → 27,253 cycles/IO — 2,642 cycles/IO (≈505 ns/IO at 5.23 GHz)
    saved.** This is the map cost recovered; it is contention-dependent (the
    original hv01 measurement put the full sampler's map-bound cost at ~9,400
    cycles/IO across *8* saturated cores — more cores, more cross-core bounce),
    so the absolute figure scales with core count, but the win is real and
    measured on both hosts.
  - per-refresh userspace latency (mmap read) mean **45.8 → 38.8 µs**.
- **Correctness (value equivalence, same fio, 2.4M IOs/arm):**
  - `blockio_device_latency` and `blockio_total_latency`: p50/p99/p999 buckets
    **identical** to the old method (131/131, 153/153, 156/156).
  - `blockio_queue_latency`: tail **identical** (p99 bucket 133/133); p50 differs
    exactly as the go/no-go A/B predicted — new reports a true **0** for the
    requests that never queued, where the old method sat at its ~512 ns–2 µs
    tracepoint-traversal floor (bucket 65). The cleaner reading, not a
    regression.
- Both BPF targets (x86_64, aarch64) compile against the checked-in `vmlinux.h`.

## Deferred / reopen

- **Requeue under load** — `block_rq_requeue` unhooked; device measures from the
  last dispatch. Reopen if a requeue-heavy workload shows anomalous device tails.
- **True partial completions** — the `nr_bytes < __data_len` branch is unexercised
  (no way to force partials here). Reopen if a device/driver known to do partial
  completions (SCSI residual) shows count inflation or low-biased percentiles.
- **Tag-allocation wait** — `alloc_time_ns` gives a fourth phase (wait for a free
  request tag) when an iocost/iolatency controller is active; 0% populated
  otherwise. A future effort could expose it where available.
