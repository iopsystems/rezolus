# Rezolus Design Principles

This document records the design principles Rezolus commits to. It exists for
two audiences:

- **Human contributors** evaluating a proposed change against the project's
  established design rules.
- **AI agents** working on the codebase, who use this document as a checklist
  before reviewing or writing instrumentation code.

The current scope is **BPF samplers**. Other scopes (userspace agent,
recorder/exporter, viewer) may be added as sibling top-level sections later;
the structure here is meant to grow without rework.

## BPF Samplers

The principles in this section apply to code under `src/agent/samplers/` and
`src/agent/bpf/`. They describe *why* the samplers are built the way they
are, not just *what* they do — the rationale matters because edge cases need
judgment, not rule-following.

The principles are listed in load-bearing order. Principle 1 is the
non-negotiable requirement; everything else is *how we meet it*.

### 1. Rezolus is a metrics agent, designed for fleetwide always-on production deployment

Not a tracer, not a profiler. The non-negotiable requirement: BPF probes are
light enough to leave on continuously across an entire fleet for the vast
majority of workloads. Every other principle in this section is *how we hold
to that target*.

*Caveat:* "light enough" is bounded by the rate of the kernel events being
instrumented and the per-probe entry cost. Workloads that drive any one hot
path at extreme rates — millions of packets per second, millions of context
switches per second, millions of syscalls per second, or millions of block
I/Os per second — can see measurable impact, both from the cumulative
entry/exit tax across all enabled probes and from the residual hashmap
lookups some samplers still need (notably the TCP packet samplers, where
the natural key is `struct sock*` — see principle 5). Less common patterns
can trip the same threshold for individual samplers (heavy mmap churn for
`cpu/tlb_flush`, throttle storms for `cpu/bandwidth`, PMU contention with
other consumers for `cpu/perf`) — the shape is always the same: one hot
path firing at an extreme rate. On those workloads the right answer is to
run Rezolus on a representative subset of machines, or to disable the
specific samplers driving the cost — not to treat it as a failure of the
design.

**How to apply.**
- Treat any change that increases per-event work in a hot path as an overhead
  regression unless explicitly justified.
- Reject features whose cost scales with workload throughput (per-event
  ringbuf submission, stack walks, expensive helpers in hot paths).

**What we refuse.**
- Stack walking, flame graphs, per-event records — those belong in tracing
  and profiling tools, not Rezolus.
- Anything whose overhead is proportional to workload throughput rather than
  to kernel events of interest at fixed cost per event.

### 2. CO-RE on vanilla kernels

Rezolus runs against stock kernels with BTF. If kernel patches or custom
modules were on the table, the BPF entry/exit overhead would not be worth
paying — we would build the agent differently. Architecture-specific
`vmlinux.h` is checked in at `src/agent/bpf/{x86_64,aarch64}/vmlinux.h` so
builds are reproducible against a known-good kernel ABI snapshot rather than
depending on the build host's headers.

**Two distinct BTF requirements.** CO-RE relocations need BTF, but it may be
kernel-provided *or* supplied as an external file via the `btf_path` config —
the kernel need not expose its own. In contrast, BTF-typed program/helper
attach (`tp_btf`, `fentry`/`fexit`, `bpf_get_current_task_btf`) needs the
*kernel's own* BTF (`/sys/kernel/btf/vmlinux`) and cannot be satisfied by a
file. On kernels that lack in-kernel BTF (e.g. NVIDIA Tegra/L4T), Rezolus
detects this at startup (`kernel_has_btf`) and loads `raw_tp` variants of the
affected hooks instead, keeping CO-RE via the external BTF.

**How to apply.**
- New BPF code must use CO-RE (`BPF_CORE_READ`, `bpf_core_read`) for kernel
  field access.
- Updates to `vmlinux.h` are deliberate, version-pinned, and per-arch — not
  silently regenerated.

**What we refuse.**
- Patches that depend on kernel modifications, custom modules, or
  build-host kernel headers.
- BPF features that have not landed in the supported vanilla kernel
  baseline.

### 3. Bounded constant work per probe; lock-free common case; no event streams for measurement

No loops over arbitrarily-sized structures, no stack walks, no walking
process trees. No expensive helpers (`bpf_get_current_comm`, `bpf_d_path`)
on hot paths. Increments are `__atomic_fetch_add(__ATOMIC_RELAXED)`; no BPF
spin locks. We design *for* the verifier, not against it: bounded loops,
explicit bounds checks, `__always_inline` helpers, `bpf_ringbuf_reserve` to
keep large structs off the BPF stack.

**How to apply.**
- Each probe does O(1) work in the worst case.
- BPF→userspace ringbufs/perf-buffers are reserved for rare metadata (new
  cgroup, task lifecycle), not per-measurement events.
- Defensive bounds checks (`if (cgroup_id >= MAX_CGROUPS) return 0;`) are
  expected, not paranoia.

**What we refuse.**
- Per-event submission of measurements to userspace; that scales with
  workload throughput and can drop under load.
- Helpers or patterns that the verifier rejects on supported kernels —
  reframe the code, do not fight the verifier.

### 4. Probe-attach preference, lowest overhead first

`tp_btf` ≥ `raw_tp` > `tracepoint` > `fentry`/`fexit` > `kprobe`/`kretprobe`.
Use `kprobe` only when the target is `notrace`/inlined or no tracepoint
exists.

*Why `fentry` over `kprobe`:* same compatibility envelope (CO-RE already
requires BTF, principle 2), substantially lower per-call overhead, and
`fexit` exposes entry args + return value in one program — often eliminating
the side-maps that `kprobe`+`kretprobe` or `kprobe`+exit-tracepoint pairs
need today.

*BTF caveat.* The ordering above assumes in-kernel BTF. On a CO-RE-only kernel
(external BTF file, no `/sys/kernel/btf/vmlinux`) it collapses to `raw_tp` >
`tracepoint` > `kprobe`; `tp_btf` and `fentry`/`fexit` are unavailable. Hooks
that prefer `tp_btf` should ship a `raw_tp` twin sharing one `__always_inline`
body, selected at runtime via `kernel_has_btf` and
`BpfBuilder::disabled_programs` (see `cpu/migrations`, `scheduler/runqueue`).

**How to apply.**
- New samplers default to the highest-preference attach the target supports.
- Latency-style samplers prefer `fexit` to collapse the entry/exit pair into
  a single program.
- Existing `kprobe` attaches are migration candidates (see drift section).

**What we refuse.**
- `kprobe` use when a tracepoint exists for the same event.
- `kretprobe` use when `fexit` is available.

### 5. Arrays over hashmaps when the key is a bounded integer

`BPF_MAP_TYPE_ARRAY` for any key that fits a bounded integer space (PID,
CPU, cgroup id, syscall nr). Accept the bounded memory cost — even when only
a fraction of the keyspace is live. Fall back to `BPF_MAP_TYPE_HASH` only
when the natural key is a kernel pointer (`struct request*`,
`struct sock*`) and there is no array-indexable equivalent.

*Why:* BPF hashmap update cost — spinlock contention on `task_switch`-class
hot paths — is the dealbreaker, not just cycles. The bounded memory cost
(e.g. `MAX_PID = 4M` × `u64` × 3 arrays in `scheduler/runqueue` ≈ 96 MB) is
acceptable; hashmap latency in a hot probe is not.

**How to apply.**
- New per-PID, per-CPU, per-cgroup-id, or per-syscall maps use
  `BPF_MAP_TYPE_ARRAY` with `BPF_F_MMAPABLE`.
- A new `BPF_MAP_TYPE_HASH` requires a comment justifying why the key
  cannot be a bounded integer.

**What we refuse.**
- Hashmaps on hot paths whose keys could be expressed as bounded integers
  with modest extra memory.
- Measurements with genuinely unbounded, non-pointer keys (e.g., per-flow
  tuple) handled by direct hashmap tracking; those need
  sampling/aggregation tricks instead.

### 6. Aggregate in BPF; mmap-direct from userspace

Counters and histograms live in `BPF_MAP_TYPE_ARRAY` maps with
`BPF_F_MMAPABLE`; userspace reads them via mmap with zero syscalls per
refresh.

*Why not `BPF_MAP_TYPE_PERCPU_ARRAY`:* it is not mmap-able, which would
force syscall reads on every refresh and defeat the design.

*Reads:* tolerate stale-but-aligned `u64` loads; we will catch the new
value next tick. Writes are `__atomic_fetch_add(__ATOMIC_RELAXED)`.

*Known ceiling:* `MAX_CPUS = 1024` is compile-time so BPF stride math stays
a constant; over-allocates on small machines, silently under-counts past
1024 CPUs. This is a known limitation.

**How to apply.**
- All hot-path counter/histogram maps use `BPF_F_MMAPABLE`.
- Userspace counter/histogram reads go through mmap, never through
  `bpf_map_lookup_elem`.

**What we refuse.**
- Per-refresh syscall reads of measurement data.
- BPF spin locks; userspace reader/writer locks on the read path.

### 7. Counter map layout matches the writer-concurrency pattern

Three counter strategies coexist in `src/agent/bpf/counters.rs`, and the
choice is principled:

- **`Counters`** (per-CPU bank, cacheline-padded, summed at userspace) — for
  hot-path totals where many CPUs increment the same logical counter and
  write contention is the dominant cost.
- **`CpuCounters`** (same map layout, exposed as per-CPU breakdown) — same
  writer pattern, but per-CPU detail surfaces NUMA imbalance and hot-CPU
  skew at exposition time.
- **`PackedCounters`** (dense, no padding, mmap-attached directly to a
  metric group) — for high-cardinality keyed counters where the natural
  cardinality (per-task, per-cgroup) already separates writers; padding
  would waste memory.

Padding solves cross-CPU contention. For keys whose natural cardinality
already separates writers (one task → one slot, one cgroup → one slot),
padding is wasted memory and packing wins.

**How to apply.**
- New samplers prefer `CpuCounters` over `Counters` (see also principle 9).
- Choose `PackedCounters` for high-cardinality keyed counters.
- Group widths (`COUNTER_GROUP_WIDTH` etc.) are sized to a whole number of
  cachelines (each cacheline is 8 × `u64` = 64 bytes); typical values are
  8 or 16.

**What we refuse.**
- Hand-rolling a fourth counter strategy when one of the three fits.
- Padding `PackedCounters`-style keyed slots where each slot has one
  natural writer.

### 8. H2 (HDR-style) histograms in BPF; bounded relative error

Buckets are parameterized by `(grouping_power, max_power)`; the codebase
standardizes on `grouping_power = 3` (~6.25% max relative error, 496
buckets) across all current histograms.

*Why not built-in / log2 / linear:* none give bounded relative error across
the wide value ranges Rezolus measures (latencies from ns to seconds, sizes
from bytes to GB).

*Why we accept the BPF-side complexity:* H2 indexing requires CLZ; BPF
cannot loop, so CLZ is implemented as a 6-deep branch tree
(`src/agent/bpf/histogram.h`).

*Tradeoff we accept:* histograms are not per-CPU sharded; bucket increments
may contend across CPUs. Bet: typical traffic spreads across enough buckets
that contention is rare. Per-bucket per-CPU shards would cost ~4 MB per
histogram (`MAX_CPUS × 496 × 8`) and we will not pay that. Possible
refinement: a small fixed shard count (e.g., 4) costs ~16 KB per histogram
and meaningfully reduces contention.

*Read shear:* userspace reads buckets non-atomically; this does not bias
percentile estimates meaningfully.

**How to apply.**
- Use `histogram_incr(&map, HISTOGRAM_POWER, value)` from
  `src/agent/bpf/helpers.h` with `HISTOGRAM_POWER = 3`.
- Deviate from `grouping_power = 3` only with a documented reason in the
  sampler.

**What we refuse.**
- Pure log2 / linear histograms when the value range demands bounded
  relative error.
- Per-bucket per-CPU sharding at full `MAX_CPUS` width.

### 9. The agent exposes detail; aggregation lives downstream

Two dimensions, same theme:

- **Distributions over summaries.** Histograms flow through the agent →
  exposition → recorder/viewer pipeline as full bucket arrays. Percentile
  choice and time-window choice live downstream (exporter, viewer,
  PromQL). The agent does not pre-compute p50/p99/etc.
- **Per-CPU over totals.** New samplers prefer `CpuCounters` and expose
  per-CPU values; callers wanting a total can sum downstream (the Rezolus
  Exporter is the natural place).

*Why:* you cannot recover a distribution from a percentile, but you can
compute any percentile from a distribution. Equivalently: per-CPU detail
surfaces NUMA imbalance, irq-affinity issues, and hot-CPU saturation;
summing is cheap and reversible — un-summing is impossible.

**How to apply.**
- Histograms are exported as full bucket arrays.
- New samplers default to `CpuCounters` (see principle 7).
- Existing `Counters`-using samplers are migration candidates (see drift
  section).

**What we refuse.**
- Pre-computing percentile scalars in the agent.
- Summing across CPUs in the agent for new samplers when per-CPU detail
  could be surfaced.

### 10. The agent never samples on its own clock; consumers drive cadence

Counters, gauges, and histograms live in mmap'd memory; the agent does no
periodic flushing or downsampling. Recorder, exporter, viewer's live mode,
and MCP queries each drive their own read cadence. Reading is just memory
reads against the mmap'd arrays — no syscalls, no agent-side work — so
multiple consumers at independent cadences are essentially free.

*Why this matters:* it cleanly separates "produce metrics" from "decide
when to record them," and makes mmap-direct reads the right architecture.
If the agent had to push at a cadence, it would need explicit timing logic.

**How to apply.**
- New samplers expose state through metrics, not through periodic
  callbacks.
- Refresh logic on the userspace side reads mmap'd state on demand.

**What we refuse.**
- Agent-side timers that flush, decay, or downsample metrics.
- Coupling between collection cadence and exposition cadence.

### 11. Prefer to combine samplers to share probes

`kprobe`/`tracepoint` entry+exit overhead per probe is the real tax, not
the work inside the probe. If two metrics need data from the same hook,
share one probe rather than attaching twice.

*Counter-pressure:* separate samplers are independently configurable via
the agent's TOML config; combining them merges that knob. Acceptable
tradeoff — overhead beats configurability granularity for samplers that
genuinely share a hook.

*Acknowledged drift:* the current codebase has duplicate attaches (see
drift section).

**How to apply.**
- Before adding a sampler that attaches to an already-instrumented hook,
  consolidate into the existing sampler.
- A combined sampler tracks all needed metrics from one probe; over-
  instrumenting cheaply beats double-paying entry/exit overhead.

**What we refuse.**
- Adding a second `kprobe`/`tracepoint` attach for a hook another sampler
  already covers, when consolidation is feasible.

### 12. Cross-cutting BPF infrastructure lives in shared headers, not per-sampler

Reusable BPF building blocks live in shared headers and are reached for by
new samplers, not reinvented:

- `src/agent/bpf/cgroup.h` — cgroup attribution helpers.
- `src/agent/bpf/task.h` — task tracking helpers.
- `src/agent/bpf/helpers.h` — `array_add`, `array_set_if_larger`,
  `histogram_incr`.
- `src/agent/bpf/histogram.h` — CLZ + H2 indexing.

*Why:* anti-drift insurance — the next sampler should not grow a duplicate
`clz()` or its own cgroup-walk routine.

**How to apply.**
- New samplers `#include` from `src/agent/bpf/` and call existing helpers.
- Reusable cross-sampler patterns are added to the shared headers, not
  copied into the next sampler.

**What we refuse.**
- A duplicate `clz()`, cgroup walker, or histogram indexer in a new
  sampler.

### 13. Userspace overhead is part of the budget

RSS, refresh CPU, and exposition cost are held to the same always-on bar
as the BPF side. The agent's userspace footprint is not a free axis to
trade against BPF cleanliness; both sides matter.

The recent RSS reduction work (~500 MB → ~80 MB), `SparseCounterGroup` for
high-cardinality metrics, mmap-direct reads avoiding intermediate copies,
and careful label cardinality discipline are all expressions of this
principle.

**How to apply.**
- A sampler's userspace refresh path is bounded constant or O(active
  keys), not O(full keyspace) and not proportional to workload throughput.
- High-cardinality metrics use sparse representations.

**What we refuse.**
- A clean BPF program paired with an O(N) userspace refresh.
- Cardinality growth without a sparse-representation strategy.

### 14. Tolerate benign races for monotone values

`array_set_if_larger` (`src/agent/bpf/helpers.h`) is a non-atomic
load+compare+store, used for high-water-mark tracking
(`bandwidth_periods`, `bandwidth_throttled_periods`, etc.). Losing an
occasional update is fine: the next observation catches up because the
value is monotone.

**How to apply.**
- Use relaxed atomics by default.
- Benign-race non-atomic patterns are acceptable for
  monotonically-increasing values where occasional missed updates
  self-heal, and must be commented as such.

**What we refuse.**
- Non-atomic patterns on values that are not monotone or where missed
  updates do not self-heal.
- Uncommented benign-race code; the comment is part of the contract.

### 15. Prefer BPF probes (or `perf_event_open`) over parsing procfs/sysfs

When the same metric is reachable from a BPF probe or a hardware/software
perf counter, prefer that over parsing `/proc` or `/sys` text files on
every refresh. Two reasons:

- **Cost.** A procfs/sysfs read is `rewind` + `read` + UTF-8 parse on
  every refresh, with the kernel formatting the file from internal
  state on each read. The cost scales with the file size and is paid in
  full at every sampling tick. A BPF counter is a single mmap'd `u64`
  load (principle 6). At high sampling frequencies the difference
  dominates the agent's userspace budget (principle 13).
- **Resolution.** Many procfs/sysfs counters are exposed at a
  coarser granularity than the underlying kernel state, or are
  pre-aggregated in ways we cannot influence. A BPF probe lets us
  define the aggregation we actually want.

Discovery is the explicit exception. Walking `/sys/devices/system/cpu`
once at startup to enumerate CPU IDs, reading
`/sys/bus/event_source/devices/.../type` to find a perf event source,
or listing `/sys/class/net` to enumerate interfaces is not on a hot
path and is fine.

A small set of metrics genuinely have no useful BPF or perf hook —
some kernel-internal gauges only surface through procfs. Those keep
parsing the file, but the choice should be deliberate, not the default.

**How to apply.**
- A new sampler whose data is reachable from a tracepoint, `fentry`,
  `kprobe`, or `perf_event_open` counter should use that, not parse a
  file on every refresh.
- One-time discovery via sysfs/procfs at startup is fine. Per-refresh
  parsing is the pattern to avoid.
- If procfs is the only viable source, document why in the sampler
  module so a later reviewer does not have to rediscover it.

**What we refuse.**
- Reading `/proc/stat`, `/proc/meminfo`, `/proc/vmstat`, or similar on
  every refresh when the same data is reachable from a BPF probe or a
  perf counter at comparable or lower cost.

---

## Reviewing or writing a sampler — operational checklist

A short pass an agent or human reviewer can run literally over a sampler
change. Each item is a yes/no question, or "justify in a comment."

- **Data source.** Is the metric reachable from a BPF probe (tracepoint /
  `fentry` / `kprobe`) or a `perf_event_open` counter? If yes, prefer
  that over parsing `/proc` or `/sys` on every refresh. One-time
  discovery via sysfs is fine; per-refresh text parsing is not. If
  procfs is genuinely the only source, justify it in a comment.
  (Principle 15.)
- **Hook deduplication.** Does another sampler already attach this hook? If
  yes, the change should consolidate the existing sampler rather than add
  a second attach. (Principle 11.)
- **Hook type.** Pick the lowest-overhead form the target supports.
  Default ordering: `tp_btf` ≥ `raw_tp` > `tracepoint` > `fentry`/`fexit`
  > `kprobe`/`kretprobe`. (Principle 4.)
- **Map structure.** Bounded-integer key → `BPF_MAP_TYPE_ARRAY` with
  `BPF_F_MMAPABLE`. Pointer key → `BPF_MAP_TYPE_HASH`, justified in a
  comment. Anything else needs explicit reasoning. (Principle 5.)
- **Counter layout.** Choose `Counters` / `CpuCounters` / `PackedCounters`
  based on the writer concurrency pattern (principle 7). Prefer
  `CpuCounters` over `Counters` for new samplers (principle 9). Group
  width is a whole-number multiple of a cacheline (8 × `u64` = 64 bytes).
- **Histogram parameters.** Use `histogram_incr(&map, HISTOGRAM_POWER,
  value)` with `HISTOGRAM_POWER = 3` unless there is a documented reason
  to deviate. (Principle 8.)
- **Increments.** `array_add` / `__atomic_fetch_add(__ATOMIC_RELAXED)`. No
  spin locks. Non-atomic patterns (`array_set_if_larger`) are acceptable
  only for monotone values and must be commented. (Principles 3, 14.)
- **Helpers in hot paths.** No `bpf_get_current_comm`, `bpf_d_path`,
  stack-walking helpers, or anything proportional to workload throughput.
  (Principle 3.)
- **Userspace reads.** mmap-direct; never `bpf_map_lookup_elem` for
  hot-path counters/histograms. (Principle 6.)
- **Latency-style samplers.** Prefer `fexit` to collapse the entry/exit
  pair into one program when BTF allows. (Principle 4.)
- **Ringbufs.** Only for rare metadata (new cgroup, task lifecycle,
  similar). Never for per-measurement events. (Principle 3.)
- **Shared headers.** Use `cgroup.h`, `task.h`, `helpers.h`,
  `histogram.h`. Do not duplicate `clz()`, cgroup-walk logic, etc.
  (Principle 12.)
- **Userspace cost.** Will the new userspace refresh path be O(active
  keys), bounded constant, or O(N) in some workload-driven metric? Prefer
  the first two. (Principle 13.)
- **Verifier-friendly patterns.** Defensive bounds checks
  (`if (id >= MAX) return 0;`), `__always_inline` helpers, branch trees
  in place of loops, `bpf_ringbuf_reserve` for large structs — these are
  expected, not signs of paranoia. (Principle 3.)
- **Per-sampler width macros are intentional.** A `COUNTER_GROUP_WIDTH N`
  define in each BPF program is sized to that sampler's metric count and
  is not a candidate for "deduplication." (`MAX_CPUS` is a different
  story — see the drift section for centralization.)

---

## Known drift / improvement candidates

Concrete items that violate or under-apply the principles above. As fixes
land, items are ticked off or removed; new drift gets added.

### Probe consolidation (principle 11)

Combine separate samplers that share a hook to halve the
entry/exit tax:

- `sched_switch`: `cpu/migrations` + `cpu/perf` + `scheduler/runqueue`
  (3 attaches → 1 sampler with a shared probe).
- `block_rq_complete`: `blockio/latency` + `blockio/requests` (2 → 1).
- `sys_enter` / `sys_exit`: `syscall/latency` + `syscall/counts` (2+2
  attaches → 1+1).

### `fentry` migration (principle 4)

No compatibility cost — CO-RE already requires BTF (principle 2):

- TCP: `tcp_sendmsg`, `tcp_retransmit_skb`, `tcp_rcv_state_process`,
  `tcp_rcv_established`, `tcp_cleanup_rbuf`.
- CFS bandwidth: `throttle_cfs_rq`, `unthrottle_cfs_rq`,
  `tg_set_cfs_bandwidth`.
- Other: `tlb_finish_mmu`, `cpuacct_account_field`.

### `fexit` collapse (principle 4)

Kprobe + exit-tracepoint pair → single `fexit` program:

- `tcp_v4_connect` / `tcp_v6_connect` paired today with
  `tracepoint/tcp/tcp_destroy_sock` for connect latency.

### `Counters` → `CpuCounters` evaluation (principle 9)

Each of these uses `Counters` today and should be evaluated for
`CpuCounters` migration to expose per-CPU detail (callers wanting totals
can sum downstream via the Rezolus Exporter):

- `network/traffic`, `network/interfaces`.
- `tcp/traffic`, `tcp/retransmit`.
- `blockio/requests`.
- `syscall/counts`.

### Centralize `MAX_CPUS = 1024` (principles 6, 12)

Currently duplicated in every BPF program. Move into a shared header.
Counter group widths stay per-sampler — those vary by metric count and
are sized intentionally.

### Procfs parsing in hot path (principle 15)

Per-refresh text parsing of pseudo-filesystems where a BPF or perf-counter
source is plausible:

- `memory/vmstat`: `rewind` + `read_to_string` + parse of `/proc/vmstat`
  on every refresh. Most VM events have function-level hooks that a BPF
  counter could increment directly; whichever entries genuinely have no
  hook would stay on procfs and be documented as such.
- `memory/meminfo`: same pattern for `/proc/meminfo`. Some entries are
  computed gauges with no clean BPF source and would have to remain;
  the goal is to move what *can* move and make the residual deliberate.

Lower priority but in the same family:

- `cpu/cores` reads `/sys/devices/system/cpu/online` per refresh. The
  file is tiny so the cost is small, but a BPF hook on `cpu_up` /
  `cpu_down` would remove the per-refresh read entirely.

Sysfs use elsewhere (`cpu/{frequency,l3,branch,dtlb}`,
`network/ethtool`) is one-time discovery (CPU enumeration, perf event
source IDs, interface listing) — not drift.

### Possible refinement: small per-CPU shard count for hot histograms (principle 8)

4 shards × 496 buckets × 8 bytes ≈ 16 KB per histogram, eliminates most
contention without the prohibitive ~4 MB cost of full per-CPU sharding.

### Low priority

NIC `*_tx_timeout` kprobes — fire only on stalls, `fentry` conversion is
not worth the churn.
