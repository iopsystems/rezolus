# Acquisition-window sidecars — why they triple the columns, and what to do

- **Opened:** 2026-08-17
- **Status:** OPEN — design, pre-build. Every number below is measured; two
  stated open measurements gate the agent-side half.
- **Arc:** closes out the largest item left by
  [recorder resource footprint](2026-08-13-recorder-resource-footprint.md),
  which named per-metric window sidecars as the structural root cause behind
  the recorder's column count. Builds on the windows work in
  [all-sampler observation windows](2026-07-10-all-sampler-observation-windows.md).
- **Owner:** Brian Martin
- **Repos:** rezolus. A later phase would touch metriken; not this one.

## Why

Every metric in a `.rez` table carries `<m>:window_begin` and
`<m>:window_width` alongside its value, so a table is ~3× the columns of the
data in it. `cpu_usage` measured **6,206 columns**, and column count is
upstream of parquet writer memory, archive size and read cost at once.

The standing assumption — written into the previous entry — was that a BPF
sampler reads its maps once per tick, so all its metrics share one window and
the sidecars are pure redundancy. **That is wrong in three separate ways**, and
the real causes are cheaper to fix than the redundancy would have been.

## Measured

From a recording killed before finalize (so the WAL retains its rows), 100 ms
interval, 32-core Linux host under a live workload. Windows read out of WAL
cells, which carry them verbatim; column counts read from a sealed segment via
`parquet metadata`.

### `cpu_usage`, the worst table

| | |
|---|---|
| cells per row | 2,063 |
| of which **windowed** | **413** |
| distinct window `begin`s | **3** |
| distinct window `end`s | **413** |
| span (`max end − min begin`) | **1.22 ms** |
| median individual width | 20.4 µs |
| parquet columns | **6,206** (2,070 value + 2,068 begin + 2,068 width) |

### Across samplers

| sampler | cells | windowed | begins | ends |
|---|---|---|---|---|
| `cpu_usage` | 2,063 | 413 | 3 | 413 |
| `syscall_counts` | 542 | 16 | 1 | 1 |
| `scheduler_runqueue` | 394 | 99 | 4 | 99 |
| `cpu_tlb_flush` | 313 | 115 | 1 | 115 |
| `cpu_perf` | 206 | **0** | — | — |
| `cpu_bandwidth` | 140 | **0** | — | — |
| `cpu_migrations` | 114 | 64 | 1 | 64 |
| `cpu_frequency` | 96 | **0** | — | — |
| `cpu_l3` | 64 | **0** | — | — |
| `drivehealth` | 23 | 23 | 23 | 23 |
| `syscall_latency` | 16 | 16 | 16 | 16 |

## Three causes, none of them redundancy

**1. Most metrics have no window, and pay for two columns anyway.** 1,650 of
`cpu_usage`'s 2,063 cells are windowless, as are `syscall_counts`'s 526 of 542;
`cpu_perf`, `cpu_bandwidth`, `cpu_frequency`, `cpu_l3`, `cpu_dtlb` and
`cpu_branch` are windowless entirely. The writer emits a `:window_begin` and a
`:window_width` column per metric regardless, so **3,310 of `cpu_usage`'s 6,206
columns are all-null** — 53% of the table.

**2. A sampler is not one acquisition.** `cpu_usage`
(`src/agent/samplers/cpu/linux/usage/mod.rs:163`) is *seven* map reads: three
`cpu_counters` groups, three `packed_counters`, one `sparse_packed_counters`.
The three windowed groups are exactly the three distinct `begin`s. The
per-cgroup and per-task groups record no window at all, which is cause (1).

**3. The 413 `end`s are the sweep timing itself.**
`src/agent/bpf/counters.rs:157` takes one `Acquisition::begin()` and then
stamps every entry with `acq.window()` — and `window()`
(`src/agent/timing.rs:46`) reads the monotonic clock on each call. So one
acquisition produces one `begin` and an `end` per entry, deliberately: the
in-tree comment defends it as "honest (they were read later)", which it is.

The cost is the shape of the loop it sits in:

```rust
for cpu in 0..MAX_CPUS {                       // counters.rs:157
    for idx in 0..self.counters.len() {
        self.counters[idx].set_with_window(cpu, value, acq.window());
    }
}
```

`MAX_CPUS` is **1024** (`src/agent/mod.rs:50`) and the sweep is unconditional,
so on a 32-core host it walks 992 empty CPU slots and takes a clock reading for
every one. Across the three groups that is on the order of 20,000
`clock_gettime` calls per tick. **The window spread is mostly measuring the
cost of measuring**, and the "precision" of the per-entry ends is precision
about our own sweep.

## Proposed

Three changes, independent, in increasing blast radius.

**A. Emit sidecars only for metrics that have a window.** Recorder-side,
lossless, no format concepts change. `cpu_usage` 6,206 → ~2,896 (2.1×);
`syscall_counts` ~2.8×; the six fully-windowless tables drop to a third of
their columns. Open question to settle first: a reader must treat an *absent*
sidecar exactly as it treats an all-null one — believed true, since such a
metric has no window either way, but it is an assumption about
`metriken-query`'s pairing logic and wants checking.

**B. Bound the counter sweep to real CPUs.** Agent-side. ~32× less work per
`cpu_counters` refresh on the always-on process, and it narrows the window span
by the same factor. Must bound by *possible* CPUs
(`/sys/devices/system/cpu/possible`), not online, or a CPU that comes up
mid-recording is silently missed — `MAX_CPUS = 1024` presumably exists because
someone met a host where the bound was not obvious.

**C. Take the window around the region read, not around each entry.**
Agent-side. Two clock reads per group instead of one per entry, and one `begin`
and one `end` per map **by construction** — which is what a window is. With (B)
first, this makes windows *tighter*, not wider, because the sweep cost stops
being inside them. `cpu_usage`'s window columns go 826 → 6.

(A) and (B)+(C) compose to roughly 6,206 → ~2,076 columns, a 3× reduction, with
window semantics simpler than today's.

## Rejected, with reasons

- **Collapse each sampler's windows into one widened window.** Sound under this
  project's interval rules — widening never over-claims tightness — but
  measured at **60×**: a 1.22 ms span against 20.4 µs median widths. That takes
  BPF windows from ~1000× tighter than the scrape window to ~40×, gutting the
  property the windows arc exists to provide. Dead unless (B) and (C) land
  first, at which point it is unnecessary anyway.
- **One row per dimension (long-form tables).** The right data model for
  high-cardinality dimensions and it keeps everything in one table, but it does
  not address the null sidecars, which are the actual cost. It also needs
  `metriken-query` to expand rows into series by label — group-by in the
  engine. *Reopen:* if per-dimension acquisition (`drivehealth`,
  `syscall_latency`) ever gets large enough to matter; today those tables are
  23 and 16 cells.
- **Split tables per cohort.** Blocked on the reader: `RezReader::route`
  refuses any query whose metrics span more than one table, so splitting
  `drivehealth` per device turns "temperature across all drives" into an error.
  Cross-table alignment is a later phase. It would also raise segment count,
  which is what drives read cost.
- **Per-cohort window columns with a metric→cohort map in metadata.** The
  design that (3) seemed to call for, before it was clear the cohorts were an
  artifact of the sweep. (C) removes the need.

## Open measurements

1. **The clock-read share of the sweep.** ~40% is inferred from arithmetic
   against the 1.22 ms span, not measured. `perf` on the agent settles it, and
   it decides whether (C) is a performance change or only a format one.
2. **How large `possible` CPUs actually is on the fleet.** (B) is worth ~32× on
   a 32-core host and nothing at all on a host that really reports 1024.

## Notes

(B) and (C) change agent-emitted data and therefore every consumer and the
`rate()` bounds, so they belong under `docs/principles.md` and the
`reviewing-samplers` discipline rather than folded into a recorder change. (A)
is recorder-side and can land on its own.
