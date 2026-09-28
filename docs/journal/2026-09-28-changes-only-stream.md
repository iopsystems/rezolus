# Sending only what changed: unchanged members, occupant liveness, and a long layout that carries values forward

- **Opened:** 2026-09-28
- **Status:** NO-GO — measured 2026-09-29 ("Measured" below), nothing built.
  Criterion 1 fails on both variants tried. Reopen condition under
  "Deferred / reopen".

## Problem

A group with slots is re-read and re-sent whole every tick. The V3 group
builder walks every member that has metadata (`create_v3`,
`src/agent/exposition/http/snapshot.rs`, the `for_each_metadata` walk near
line 2300) and puts each member's value in the row. The long layout then
stores one row per occupant per tick
([the dendro archive layout](2026-09-25-dendro-archive-layout.md)), whether
or not the value moved.

Measured 2026-09-28 by replaying two recordings through metriken-archive's
writer (metriken `docs/journal/2026-09-28-archive-writer.md`, "Gate
results"). The hosts are the ones the layout entry measured: the **busy
host** (5.22.0, 2.3 h) and the **quiet host** (5.18–5.20, 9.6 h).

- The quiet host's per-task table (`cpu_usage/cpu_usage_task`) is 507 MB in
  arrival order and 345 MB sorted, against the layout entry's 505.4 and
  348.7 MB. The busy host's is about 38 MB in arrival order.
- On the quiet host about 2,530 threads report every tick, so each ~104-tick
  segment holds about 263,000 rows.
- The value column is 87% of a segment's bytes (1.54 of 1.77 MB). Time and
  occupant columns are 10–180 kB each.
- 36–44% of readings equal the same thread's previous reading (three
  segments sampled).
- The readings that changed moved by a median of 0.88–0.94 ms per 1 s tick,
  p90 1.9–2.7 ms. At nanosecond resolution that is close to incompressible.

The same segments' value column re-encoded (kB per ~263,000-row segment):

| encoding | kB |
|---|---|
| PLAIN + LZ4 (the writer on 2026-09-28) | 1,050 |
| PLAIN + zstd (the writer's default since metriken-archive 0.2.5) | 553 |
| DELTA_BINARY_PACKED + LZ4 | 811 |
| DELTA_BINARY_PACKED + zstd | 798 |

So on this table, not writing unchanged readings removes at most about 40% of
rows. zstd removes about 47% of the value column on its own, with no change
in meaning. Neither is an order of magnitude here. Groups whose members are
mostly idle (idle cgroups, softirq counters on quiet CPUs) may gain more;
that is not measured.

## Rejected: per-event submission from the tracepoint

The tempting form is a ringbuf record from the probe each time a value
changes, forwarded as a stream of updates. The principles refuse it:

- Principle 1 (`docs/principles.md`, lines 49–52): "Reject features whose cost
  scales with workload throughput (per-event ringbuf submission, stack
  walks, expensive helpers in hot paths)."
- Principle 3 (lines 101–108): ringbufs "are reserved for rare metadata (new
  cgroup, task lifecycle), not per-measurement events", and it refuses
  "Per-event submission of measurements to userspace; that scales with
  workload throughput and can drop under load."
- Principle 10 (line 287): "The agent never samples on its own clock;
  consumers drive cadence."

The per-task counter is updated from `cpuacct_account_field`
(`src/agent/samplers/cpu/linux/usage/mod.bpf.c`, `handle_cpuacct_account_field`),
which fires on every accounting event. A record per firing would scale with
the workload, and a dropped record would be a lost reading with no way to tell.
A cumulative counter read from mmap loses nothing: the next read includes it.

## Design

Three parts. Each can be measured alone, and the first two need no format
change.

### 1. The agent sends only members whose value changed

**Counters need no BPF change.** `handle_cpuacct_account_field` already
returns before touching `task_cpu_usage` when both deltas are zero
(`mod.bpf.c`, "Skip updating metrics if there's no change"). The value is
otherwise only written when a task instance starts (`handle_new_task`,
zeroed) and when it exits (`account__sched_process_exit`, zeroed). So for a
counter slot, "unchanged since the last read" is exactly "the value equals
the last value this reader saw". That comparison belongs in the agent's group
walk, beside the per-member identity fold in `create_v3`. It costs one 64-bit
compare per member, on a walk that already reads every member's value.

**A per-slot epoch in BPF is only worth it for histograms and gauges.**
No sampler has a histogram group with slots today (`docs/backlog.md`,
"Agent — histogram groups with slots"), so this is for when one does.
Comparing a histogram member's buckets costs one comparison per bucket (496
per slot in the configuration that backlog item names). For those, the probe
that updates the slot also stores the current read epoch into an
`BPF_F_MMAPABLE` per-slot array: one store, bounded, in line with principle 3.
The agent bumps the epoch at each read and sends a member only if its stored
epoch is newer than the last read's. A gauge set to the value it already had
counts as a change under the epoch and not under a comparison. Which one is
wanted depends on the gauge.

**Where the filter applies.** The comparison is per consumer, not global: the
scrape cache (`/metrics`) and each `/metrics/stream` subscription have their
own "last read". The stream already filters per connection by group
(`last_window` in `src/agent/exposition/http/mod.rs`, the `keep` closure
passed to `FrameProducer::interval` in `frames.rs`). Per-member filtering is
the same idea one level down, and it needs its own per-connection state: last
value per (group, slot), or last epoch.

**How an unchanged member is written.** A group row already carries
`Vec<Option<u64>>` values (`WalGroupRow`, metriken-segment `wal.rs`), but
`None` means "not a member this tick". An unchanged member needs a third
state that is not "absent". Either:
- the schema keeps listing it, and the row carries a changed-member bitmap; or
- the row's schema lists only changed members, and the index's membership (or
  the occupant stream) is what says a member is still there.

The second fits the long layout better (part 3). The first keeps V3 group rows
self-describing. This is open (see "Open questions").

### 2. Liveness: telling idle from gone

With unchanged members absent, a reader cannot tell an idle thread from an
exited one without a separate signal. Principle 3 allows ringbufs for
exactly this: "rare metadata (new cgroup, task lifecycle)".

What exists today:
- **Tasks, both ends.** `task_info` (new task or reused PID, with retry via
  `METADATA_PENDING` in `handle_new_task`) and `task_exit` (from
  `sched_process_exit`) ringbufs in `usage/mod.bpf.c`, handled in
  `usage/mod.rs` (`handle_task_exit`, registered at the `ringbuf_handler`
  call).
- **Cgroups, start only.** `handle_new_cgroup` (`src/agent/bpf/cgroup.h`)
  detects a new or reused css id by its serial number and sends
  `cgroup_info`. Nothing is sent when a cgroup is removed. A removed cgroup
  is noticed only when its id is reused.
- **Per-CPU groups** need none: their members are the online CPUs.

Two gaps follow:
- **A dropped `task_exit` is not retried.** The comment in
  `account__sched_process_exit` says so: `sched_process_exit` fires once, and
  a dropped event leaves the task's metadata as a member reporting 0 until
  the PID is reused. Today that is a phantom series at a constant 0. Under
  changes-only it would be a phantom idle occupant that the reader keeps
  alive and carries forward. The agent needs a liveness check that does not
  depend on the event, for example comparing `task_start_times` (already
  mmap'd) against the member's recorded start time at each read.
- **Cgroup removal has no event.** Changes-only for cgroup groups needs one:
  a probe on cgroup release (to be chosen per principle 4), or a periodic
  check against the cgroup filesystem at the sampler's own cadence
  (principle 17).

The archive records liveness per occupant: the tick it was first seen, and
the tick it was last known alive. The occupant stream (`<table>/occupants`)
already carries first sight and a restatement every 300 s. An end-of-life
row would complete it.

### 3. Long layout v2: rows on change, values carried forward on read

In metriken-segment:
- Rows only for occupants whose value changed that tick.
- Per-occupant liveness in the occupant stream (part 2).
- A new layout marker value (the footer's `metriken.layout`, today `long`),
  so a v1 reader refuses a v2 segment rather than misreading it.

Every reader follows: metriken-query's long reader (`metriken_query::long`),
WAL-tail materialization (`materialize_long_wal_tail`), metriken-archive's
`ArchiveReader`, and the browser viewer, which uses the same crates. For each
occupant, within its liveness range:
- **Fill forward** the last value at each tick with no row, so `rate()` and
  `irate()` see zero increase rather than no sample, and a series does not
  leave `count(...)` or `sum(...)` at the staleness lookback edge.
- **Reconstruct a window** for each filled tick. An unchanged reading still
  had an acquisition window, and the uncertainty bands come from windows. A
  group's window is one per tick (principle 18), so the filled tick can take
  the group's window for that tick, provided the table keeps one window row
  per tick even when no occupant changed.

## Open questions

- Which form marks an unchanged member in a V3 group row (bitmap, or a
  schema of changed members only)? It is a wire-format change for
  `/metrics/rows`, `/metrics/stream` and the WAL row either way.
- Per-consumer state in the agent: the scrape cache serves many consumers
  from one pass. Does changes-only apply to the stream only, with scrapes
  still sending every member?
- Does the reader fill forward at read time for every query, or does
  compaction materialize a dense copy for old data?
- zstd (a writer setting) removes about as much from the per-task table as
  changes-only does. Is the combination worth the format change on the
  tables that matter? That depends on the measurement below.

## Measured (2026-09-29): criterion 1 fails

Both real recordings were replayed twice through metriken-archive 0.2.7's
writer with its defaults (zstd-3, arrival order, seal at 8 MiB, 900 rows or
5 min): once whole, and once with unchanged readings removed from every
group with slots. Groups without slots were identical in both. Two ways of
removing a reading were tried:

- **Rows dropped** (the design above): an occupant's row is left out of a
  tick when none of its values changed since it last sent one. This leaves
  out the per-occupant liveness the occupant stream would gain, so it
  understates layout v2's size.
- **Values nulled**: every row is kept and each unchanged value is written
  as null. This keeps the tick-to-tick row alignment. It needed a local
  writer change to keep a slot whose values are all null (not committed).

Bytes of long tables plus their occupant streams:

| | unchanged | rows dropped | values nulled |
|---|---|---|---|
| busy host (5.22.0, 2.3 h) | 83.0% of occupant rows | 40.5 → 30.4 MB, **−25.0%** | −12.4% |
| quiet host (5.18–5.20, 9.6 h) | 49.2% of occupant rows | 272.4 → 317.2 MB, **+16.5%** | +10.6% |

The quiet host's per-task table (`cpu_usage/cpu_usage_task`) is 212 of its
272 MB, and it decides the result:

| per-task table, quiet host | rows | value column | occupant column |
|---|---|---|---|
| whole | 76.2 M | 191.8 MB | 14.9 MB |
| rows dropped | 47.2 M | 207.5 MB | 50.6 MB |
| values nulled (29.0 M nulls) | 76.2 M | 219.7 MB | 15.1 MB |

The mechanism: in arrival order every tick lists the same occupants in the
same order, so a tick's block of values nearly repeats the previous one, and
zstd stores an unchanged value as part of a long match against it at almost
no cost. Removing unchanged values takes out little, and it breaks the
alignment the remaining values were matched against. Dropping rows also
turns the occupant column from a repeating sequence into an irregular subset,
3.4 times larger. Nulling keeps the occupant column but not the value
column: parquet stores only non-null values, and which ones are present
changes every tick. The busy host has heavy thread churn, so its ticks were
already poorly aligned; its per-task table still shrank 53.5% with rows
dropped.

Where removal pays is a table that almost never changes:
`cpu_bandwidth` (97–99%), `network_ethtool` (96%), `drivehealth`
(48–95%) and `cpu_usage_cgroup_exited` (64–90%) with rows dropped. Each is
under 1 MB, so the totals barely move.

Criteria 2–5 were not measured: criterion 1 decides the design by itself.

## GO / NO-GO

**Verdict: NO-GO.** Criterion 1 fails on both hosts and both variants
("Measured" above). GO only if all of these hold:
1. **Size, across all long tables on both real hosts.** Rows and bytes
   removed by changes-only, per table and in total, not just per-task. It
   must reduce the archive by at least 25% beyond what zstd alone gives, or
   it is not worth a layout version.
2. **Agent cost, measured.** The per-refresh cost of the comparison (and of
   the epoch store, for histograms) in µs at fleet-representative scale,
   from the agent's `sampling latency` debug line, per principle 16. It must
   be no more than the cost of sending the unchanged members it removes.
3. **Probe cost, if the epoch is built.** Per-event cost of the added store
   on `cpuacct_account_field` and the histogram probes, measured as
   principle 16 asks, not assumed small.
4. **Reader cost.** Query latency with fill-forward no worse than v1 on the
   same data, for `sum(rate(...))` over the table and for a single series.
5. **Liveness.** A thread whose exit event is dropped is detected within one
   read, and a removed cgroup within the sampler's own cadence. Checked by
   filling the ringbuf in a test, as the per-task completeness work did
   (`docs/backlog.md`, "Agent — per-task CPU usage completeness").

## Deferred / reopen

- **Reopen** when dendro's `CompactSpec` sort key (`docs/backlog.md`,
  "dendro archives (6.0)") lands and segments sorted by
  `(occupant, timestamp)` still carry long runs of repeated values that their
  encoding does not already collapse. Sorted, an unchanged value sits next to
  the same value rather than a tick away, which is where a changes-only
  layout or a column encoding could remove it without the misalignment
  measured here.
- zstd for segments: shipped as the writer's default (zstd-3,
  metriken-archive 0.2.5), and the baseline the measurement above uses.
- Coarser units for task CPU time (µs, not ns) would remove about 10 bits of
  noise per delta. It changes what the agent reports; not measured.
- Dropped-`task_exit` liveness check: needed by part 2, and useful today too
  (it would end the phantom constant-0 series).

## Cross-references

- [The layout of a rezolus dendro archive](2026-09-25-dendro-archive-layout.md):
  the long layout and occupant stream this extends.
- [Recording to dendro archives](2026-09-28-dendro-writer-adoption.md)
  (#1326 through #1346): the writer path this would change.
- metriken `docs/journal/2026-09-28-high-cardinality-stack.md`, phase 5
  (members that come and go), and `docs/journal/2026-09-28-archive-writer.md`
  (the writer and its gate results).
