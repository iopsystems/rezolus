# Acquisition-window sidecars — why they triple the columns, and what to do

- **Opened:** 2026-08-17
- **Status:** OPEN — design, pre-build. The recorder-side change is ready to
  land on its own; the two agent-side changes are specified but one open
  measurement gates the performance claim.
- **Arc:** closes out the largest item left by
  [recorder resource footprint](2026-08-13-recorder-resource-footprint.md),
  which named per-metric window sidecars as the structural root cause behind
  the recorder's column count. Builds on
  [all-sampler observation windows](2026-07-10-all-sampler-observation-windows.md).
- **Owner:** Brian Martin
- **Repos:** rezolus.

## Why

Every metric in a `.rez` table carries `<m>:window_begin` and
`<m>:window_width` alongside its value, so a table runs ~3× the columns of the
data in it. `cpu_usage` measured **6,206 columns**, and column count is upstream
of parquet writer memory, archive size and read cost at once.

The standing assumption — written into the previous entry — was that a BPF
sampler reads its maps once per tick, so all its metrics share one window and
the sidecars are pure redundancy. That is wrong, and what is actually going on
is cheaper to fix than the redundancy would have been.

## Measured

From a recording killed before finalize (so the WAL retains its rows), 100 ms
interval, 32-core Linux host under a live workload. Windows read out of WAL
cells, which carry them verbatim; column counts read from a sealed segment via
`parquet metadata`.

Two populations appear below and they are not the same set: a WAL row from one
tick held **2,063 cells**, while the sealed segment carried **2,068 metric
columns**. `task_cpu_usage` is `sparse_packed_counters`, so its cardinality
tracks live tasks and drifts tick to tick. Treat both as "about 2,065", not as
exact table geometry.

### Column composition (`cpu_usage`, one sealed segment)

| | |
|---|---|
| total columns | **6,206** |
| = metrics × 3 + `timestamp` + `:wall_offset` | 2,068 × 3 + 2 |
| metrics that actually have a window | **413** |
| **all-null sidecar columns** | **3,310** (53% of the table) |

### Windows are per acquisition, and a sampler has several

`cpu_usage`, one tick, windowed cells grouped by their `begin`:

| acquisition | entries | own span | median width |
|---|---|---|---|
| begin +0 ns | 81 | 21.3 µs | 12.2 µs |
| begin +325,089 ns | 166 | 65.1 µs | 37.5 µs |
| begin +1,162,198 ns | 166 | 58.1 µs | 34.0 µs |

**Overall span 1.22 ms, of which the widest single sweep is 5.3%.** The rest is
the gap *between* the three acquisitions. This is not uniform across samplers —
`scheduler_runqueue`'s four groups span 36.7 µs of which one sweep is 89.3%.

### Across samplers

| sampler | cells | windowed | acquisitions |
|---|---|---|---|
| `cpu_usage` | 2,063 | 413 | 3 |
| `syscall_counts` | 542 | 16 | 1 |
| `scheduler_runqueue` | 394 | 99 | 4 |
| `cpu_tlb_flush` | 313 | 115 | 1 |
| `cpu_perf` | 206 | **0** | — |
| `cpu_bandwidth` | 140 | **0** | — |
| `cpu_migrations` | 114 | 64 | 1 |
| `cpu_frequency` | 96 | **0** | — |
| `cpu_l3` | 64 | **0** | — |
| `drivehealth` | 23 | 23 | 23 |
| `syscall_latency` | 16 | 16 | 16 |

## What is actually going on

**Most metrics have no window and pay for two columns anyway.** 1,650 of that
tick's 2,063 cells are windowless, as are `syscall_counts`'s 526 of 542;
`cpu_perf`, `cpu_bandwidth`, `cpu_frequency`, `cpu_l3`, `cpu_dtlb` and
`cpu_branch` are windowless entirely. `rez.rs:262` pushes both sidecar fields
unconditionally for every column, so those metrics get two all-null columns each.

**A sampler is not one acquisition.** `cpu_usage`
(`src/agent/samplers/cpu/linux/usage/mod.rs:163`) is seven map reads: three
`cpu_counters` groups, three `packed_counters`, one `sparse_packed_counters`.
The three windowed groups are exactly the three distinct `begin`s; the
per-cgroup and per-task groups record no window at all.

**A window's `end` records our sweep position, not the observation.**
`src/agent/bpf/counters.rs:157` takes one `Acquisition::begin()` and stamps
every entry with `acq.window()`, which reads the monotonic clock on each call
(`src/agent/timing.rs:46`). One acquisition therefore yields one `begin` and an
`end` per entry. The in-tree comment defends this as "honest (they were read
later)", which it is — but what it encodes is where an entry sat in our
iteration order, which is a property of our loop rather than of the data.

## Proposed

Three independent changes. The first is recorder-side and lands alone; the
other two are agent-side and are independent of each other.

### 1. Emit sidecars only for metrics that have a window

Recorder-side, lossless, no format concepts change. `cpu_usage` 6,206 → ~2,896
columns (2.1×); `syscall_counts` ~2.8×; the six fully-windowless tables drop to
a third of their columns.

Settle first: a reader must treat an *absent* sidecar exactly as it treats an
all-null one. Believed true — such a metric has no window either way — but it is
an assumption about `metriken-query`'s pairing logic and wants checking.

### 2. Take the window around the region read, not around each entry

Agent-side. One `begin` and one `end` per map read, which is what an
acquisition is. `cpu_usage`'s window columns go 826 → 6, and the per-entry
clock reads go away.

**This widens the typical window ~1.75×** — 12–37 µs becomes 21–65 µs, measured
consistently across the three groups. That is the honest cost and it is
negligible where it lands: against a 1 s scrape interval the window's
contribution to a `rate()` band goes from 0.004% to 0.0065%; at 50 ms, 0.07% to
0.13%. Tens of microseconds against seconds.

What it buys beyond the columns is that the window stops encoding our iteration
order. An entry's `end` today says when the sweep reached it; after this it says
when the acquisition finished, which is a fact about the observation.

### 3. Bound the counter sweep to real CPUs

Agent-side, and independent of (2). `src/agent/bpf/counters.rs:157` sweeps
`0..MAX_CPUS` with `MAX_CPUS = 1024` (`src/agent/mod.rs:50`), unconditionally,
so on a 32-core host it walks 992 empty slots every refresh of every per-CPU
counter group.

Bound by *possible* CPUs (`/sys/devices/system/cpu/possible`), never by online,
or a CPU that comes up mid-recording is silently missed — `MAX_CPUS = 1024`
presumably exists because the bound was not obvious to someone.

## Rejected, with reasons

- **Collapse each sampler's windows into one widened window.** Not a magnitude
  problem but a semantic one: `cpu_usage`'s 1.22 ms span covers three
  *separate* acquisitions 325 µs and 1.16 ms apart, so a single window would
  claim one acquisition where there were three. (2) collapses within an
  acquisition, which is the boundary that means something.
- **One row per dimension (long-form tables).** This *would* fix the null
  sidecars — per-task metrics become rows and the window columns collapse to
  one pair per table — so the objection is not that it misses the problem. It
  is that it needs `metriken-query` to expand rows into series by label, i.e.
  group-by in the engine, and it multiplies row count by dimension cardinality.
  *Reopen:* if per-dimension acquisition (`drivehealth`, `syscall_latency`)
  grows; today those are 23 and 16 cells.
- **Split tables per cohort.** Blocked on the reader: `RezReader::route`
  refuses any query whose metrics span more than one table, so splitting
  `drivehealth` per device turns "temperature across all drives" into an error.
  It would also raise segment count, which drives read cost.
- **Per-cohort window columns with a metric→cohort map in metadata.** The
  design the cohort structure seemed to call for, before it was clear that (2)
  makes cohorts trivially equal to acquisitions.

## Open measurement

**The cost of the per-entry clock reads is not established, and the obvious
estimate does not survive.** 1,024 CPUs × ~8 counters is 8,192 iterations, which
against a measured 65 µs group span is 8 ns per iteration — below the cost of
one vDSO `clock_gettime`. So either the loop is shorter than that model, or the
window closes before the sweep ends because only the first ~32 CPU slots emit
metrics. If it is the latter, the sweep's real cost is *invisible* in the window
data rather than inflating it, which strengthens (3) and says nothing either way
about (2).

`perf` on the agent settles it. Until then, (2) is justified by the column count
and the semantics, not by a CPU claim, and (3) is justified by the redundant
work being self-evident from the loop bound.

## Notes

(2) and (3) change agent-emitted data and therefore every consumer and the
`rate()` bounds, so they belong under `docs/principles.md` and the
`reviewing-samplers` discipline rather than folded into a recorder change.
