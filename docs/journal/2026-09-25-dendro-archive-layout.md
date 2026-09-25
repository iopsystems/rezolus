# The layout of a rezolus dendro archive

- **Opened:** 2026-09-25
- **Status:** **OPEN — design, nothing built.** Intent-first: this is what the
  reshaping `recording upgrade --to dendro` will write, and what the 6.0 writer
  (#1224) must then write identically. The per-task table's long layout is
  proposed and gated on a measurement (below).
- **Driver:** #1301 converts a `.rez` into a dendro archive by copying segment
  bytes unchanged, which carries the `.rez` layout's costs across with them.
  Measured on the converted archives (below), segment footers were 42% and 73%
  of the bytes, and on a busy host the per-task table was 90% nulls. The layout
  has to change, and the converter is where it can change first, since it
  works offline on finished recordings.
- **Owner:** Brian Martin

## Measured: what the `.rez` layout costs

Two recordings from `~/Downloads`, converted with #1301 (bytes unchanged) and
read with dendro's API; the harness read each segment's parquet footer and
data.

| | older recording (5.18–5.20 agent) | 5.22.0 recording |
|---|---|---|
| size, span | 1.28 GB, 9.6 h | 581 MB, 2.3 h |
| footer bytes / segment bytes | 529 / 1,272 MB (42%) | 423 / 579 MB (73%) |
| `cpu_usage_task`: distinct slots overall | 6,644 | 396,117 |
| `cpu_usage_task`: slots per segment, median / max | 2,550 / 2,847 | 14,011 / 38,388 |
| `cpu_usage_task`: non-null cells | 98.0% | 10.6% |
| `cpu_usage_task`: field metadata | 72.7 MB | 91.4 MB |

The largest 5.22.0 task segment was 35.6 MB for 301 rows: a 32.1 MB footer, of
which the `ARROW:schema` entry (the Arrow schema with every field's metadata)
was 27.5 MB. Every column carries its slot's full label set; for this table
that is `cgroup`, `comm`, `pid`, `tgid`, `__uid__`, `id`, `sampler`, `metric`,
`metric_type`, `unit`, repeated in every segment the slot appears in.

**The width is not reassignment.** None of the 37,644 columns in that segment
carried a `#N` generation suffix (#1232). The task table's slot is the PID
(`79x14` is PID 14), so its slot space is unbounded and its width is the
number of distinct PIDs a segment saw. On a host with heavy process churn,
that is tens of thousands of columns, 90% null.

Every other group table in both recordings falls into one of two classes:

- **No slots** (plain members): `memory_meminfo_read`, `syscall_latency_latencies`,
  the `tcp_*` and `blockio_*` groups. Nothing about identity to change.
- **Bounded slots:** per-CPU groups (16 slots on these hosts), cgroup groups
  (31–57), device groups (2–3). Fixed per recording, 88–100% full. A cgroup
  slot is a reused BPF map index, so its occupant changes over time.

`cpu_usage_task` is the only unbounded-slot table in either recording.

## What each recording can tell us

Dated from `git log` and the release tags:

| change | merged | first release |
|---|---|---|
| acquisition groups, SnapshotV3 (#1070) | 2026-08-20 | 5.18.0 |
| recorder records the agent's `version` (#1211) | 2026-09-14 | 5.21.0 |
| a relabeled slot gets its own column (#1232) | 2026-09-16 | 5.21.0 |
| slot identity captured with the values (#1249) | 2026-09-18 | 5.21.0 |
| `__uid__` minted per assignment (#1276) | 2026-09-23 | 5.21.0 |
| identity index in `caller_rows`, `--stream` only (#1260, #1272) | 2026-09-21, -22 | 5.21.0 |

- **5.21 and later** record each occupant exactly: a relabeled slot opens a
  `#N` column, and every column carries its occupant's `__uid__`. A `--stream`
  recording also carries the identity index itself.
- **5.18–5.20** do not. Before #1232 a slot that changed occupant inside a
  segment kept writing into the first occupant's column, under the first
  occupant's labels; in #1232's words, "no way for any later read to recover
  the split." Before #1249, labels and values were read at different times,
  so a value at a transition tick can carry the other occupant's labels.

The four older recordings here are schema 3 with no `version` and no
`producer_epoch`, so they come from that range.

**Decided (2026-09-25): a conversion records what the file records.** It does
not try to recover mid-segment changes in a 5.18–5.20 recording. A
counter-reset heuristic could find *where* some happened (task counters are
zeroed on exit and on PID reuse, `cpu/linux/usage/mod.bpf.c:230`, `:427`,
since #675), but never *who* came next, whose labels are not in the file.

## The layout

A rezolus dendro archive is a dendro archive (`FORMAT.md` in
iopsystems/dendro) whose sources, streams and caller rows have the following
meaning. Nothing here changes dendro.

### Sources

One source per recording. `labels` are the recording's labels as `.rez` has
them (`source`, `host`, any `record --label`). `metadata` is the recording's
metadata, plus:

- `producer_version`: dendro's key, from rezolus's `version` where present
  (#1301 already writes it).
- `identity`: `"uid"` when the recording's occupants carry a real `__uid__`,
  `"labels"` when they do not. A reader or a person can then see why one
  recording's series carry uids and another's do not, rather than inferring
  it from their absence. Decided by the data, not the version string: a
  recording is `"uid"` if any slot column in any group table carries
  `__uid__`.
- `rez_layout`: the version of this layout the source was written in,
  starting at `1`. A reader refuses a version above its own.

### Streams

One stream per table, named by the table key it has today: `<sampler>/<group>`
for a group table, `<sampler>` for a per-sampler (SnapshotV2) table,
`prometheus/scrape` for a Prometheus target. Time columns are unchanged:
`timestamp`, `:wall_offset`, and the table-level `:window_begin` /
`:window_width` (or per-metric window columns in a SnapshotV2 table).

### Field metadata: fixed facts only

A column's field metadata holds what is true of that column for the life of
the stream, which is what dendro's `FORMAT.md` §8 asks: "a fact that changes
over time belongs in `caller_rows` … never in field metadata."

- **Kept:** the storage keys (`metric`, `metric_type`, `unit`,
  `grouping_power`, `max_value_power`; `docs/labels.md`), `sampler`, labels
  that are fixed per column (`op` on `syscall_counts_cgroup`, `kind` on
  softirq, `cpu`), and `id`, the slot. `id` stays because the indexed reader
  finds a column's slot from it (`crates/rez/src/indexed.rs:237`).
- **Moved to the index:** a slot's identity labels, the ones
  `SlotIdentity::set` writes (`src/agent/identity.rs:311`): `comm`, `pid`,
  `tgid`, `cgroup`, `name`, `device`, `mount`, the hardware-sensor keys, and
  `__uid__`. `docs/labels.md`'s identity list omits `name`, which cgroup slots
  set (`src/agent/bpf/mod.rs:339`); that document needs the fix.
- **Dropped:** `source` and `endpoint`, the per-column provenance older
  recorders wrote. Provenance is the source's (#1258, #1259).

**How the converter tells identity labels from fixed ones.** Not from a key
list, which is already incomplete. Where the recording carries an index
(`--stream`), the identity keys are exactly the keys its index entries hold.
Otherwise, for each metric in a group table, a label is identity if its value
differs between that metric's slot columns anywhere in the stream; a label
constant across all of them is fixed. A group with one slot has nothing to
compare, and its labels stay in field metadata. The `--stream` recordings
are the test: the derived split must equal the index's.

### Slot columns

- **Bounded-slot tables** (cgroup, device, mount, interface, GPU, per-CPU;
  see "Per-stream considerations" for why each is wide): one column per
  metric and slot, named
  `{metric_id}x{slot}` as today, with no `#N` suffix. In a 5.21+ recording,
  a slot's `#N` columns hold disjoint rows (the builder pads each with nulls
  outside its occupancy), and the converter merges them into the one slot
  column. It refuses a table where two of them overlap, which would mean the
  premise is wrong. Who occupied the slot when is the index's.
- **Tables with no slots:** unchanged, apart from the dropped provenance keys.
- **The unbounded-slot table (`cpu_usage_task`):** long, keyed by occupant;
  see "The per-task table".

### The identity index

Written to `caller_rows` under the stream's name, as `IndexEntry` blobs
(`crates/rez/src/index.rs:104`), the encoding the indexed reader already
replays.

- A **`--stream` recording** has one; the converter copies it unchanged.
- A **scrape recording** gets one derived from its columns:
  - a `Full` at the stream's first row, listing every slot then live with its
    identity labels;
  - a `Delta` wherever a slot's occupant changes: at a `#N` column's first
    non-null row in a 5.21+ recording, and at a segment start where a slot's
    labels differ from the previous segment's in a 5.18–5.20 one;
  - a slot's removal where it has no further values;
  - a restating `Full` every `RESTATE_EVERY` (300 s,
    `src/recorder/stream.rs:156`) of row time, so a reader's replay starts
    at most one period before the rows it needs, as for a recording made over
    the stream.
- `state` is computed with `SourceIndex` (`crates/rez/src/index.rs:378`), as
  the subscriber computes it.
- `__uid__` goes into a slot's index labels when the recording is `"uid"`.
  A `"labels"` recording has none, and none is minted: a minted uid would
  assert sameness or difference that a 5.18–5.20 file does not record.

### Stream summaries

Not used. iopsystems/dendro#20 proposed a per-stream summary holding every
column's field metadata. For a churning table that summary grows with every
occupant, must be rewritten whole after each seal inside a rolling buffer's
tick, and describes evicted columns after retention; keeping it true would
make it a timestamped change log, which is what `caller_rows` already is.
With identity out of field metadata, a stream's schema is small and changes
rarely, and a reader learns it from one segment.

## Per-stream considerations

Every group with slots that can change hands has a `SlotIdentity`
(`git grep "SlotIdentity::new" src/agent`). Tasks and cgroups are the only
ones that churn; the rest hold a handful of slots that change rarely. Each
kind was considered on its own, because the layout is decided by how wide a
table can get, not by whether its slots can be reassigned: the time-keyed
index already handles reassignment.

| identity | samplers | slot key | how many slots | churn | layout |
|---|---|---|---|---|---|
| task | `cpu_usage` (`cpu_usage_task`) | TID | unbounded up to `PID_MAX_LIMIT`; measured 14,011 per segment at the median, 38,388 max, 396,117 in 2.3 h | high: every thread that lives about a sample interval | **long, keyed by occupant** |
| cgroup | `cpu_usage`, `cpu_bandwidth`, `cpu_migrations`, `cpu_perf`, `cpu_tlb_flush`, `scheduler_runqueue`, `syscall_counts` | CPU-controller `css.id` | the host's live cgroups, capped by rezolus at 4,096; measured 31–57 | low to moderate: pod and job churn on a busy node | **wide by slot, time-keyed index** |
| drive | `drivehealth` (sweep, NVMe) | drive index | 2 measured | rare | wide by slot, index |
| mount | `filesystem` | mount index | 3 measured | rare (remount) | wide by slot, index |
| interface | `network_ethtool` | interface index | a few | rare | wide by slot, index |
| GPU engine, device, memory | `gpu` (Intel) | device index | a few | rare | wide by slot, index |
| CPU | per-CPU groups | CPU id | CPUs (16 measured) | none; the slot is the identity | wide by slot, one `Full` |
| none | plain groups | — | — | — | unchanged |

### Tasks: why long

The TID space is `PID_MAX_LIMIT`, which on 64-bit is `4 * 1024 * 1024`
(`include/linux/threads.h:34`), the same as rezolus's `MAX_PID`
(`src/agent/bpf/task.h:14`), so the task group covers every TID the kernel can
give out and cannot overflow. A wide table's width is the number of distinct
threads a segment saw, which a thread-per-request workload drives up without
limit. The reasoning is in "The per-task table" below.

### Cgroups: considered, and wide is better

A cgroup slot can be reassigned exactly as a task slot can, so the same
occupant-keyed long layout was considered for it. It is not used, because
size, not reassignment, is what rules out wide, and a cgroup table's width is
bounded by the host's live cgroups:

- **The slot is the CPU controller's css id** (`task->sched_task_group->css.id`,
  `src/agent/bpf/cgroup.h:38`). The kernel allocates it with
  `cgroup_idr_alloc(&ss->css_idr, NULL, 2, 0, …)` (`kernel/cgroup/cgroup.c:5936`
  on current master): `idr_alloc` with no upper bound, returning the lowest
  free id. Ids are dense and a freed one is reused at once, so the largest id
  in use tracks the number of live CPU-controller cgroups. Churn reuses slots;
  only a larger live set widens the table.
- **Reassignment is detected and indexed.** A reused id comes with a new
  `css.serial_nr`, which `handle_new_cgroup` compares (`cgroup.h:37-53`), and
  `SlotIdentity::set` then mints a new occupant. The time-keyed index and the
  indexed reader (#1280) attribute the rows; identity is captured with the
  values since #1249.
- **The tables are dense.** Cgroups live long next to threads, and the
  measured tables were 98–100% full, which is where wide is cheapest.
- **At rezolus's cap the cost is bounded.** At 4,096 slots a table's footer
  would be about 4,096 × ~700 bytes, near 3 MB per table per segment, from
  InfluxData's per-column figure with statistics off (Prior art), so an
  estimate, somewhat higher with statistics. A typical Kubernetes node is far
  below the cap: 110 pods by default at a few cgroups each, plus systemd
  units, is about 300–1,000.

What long would buy for cgroups is one reader path and identity recorded once
per occupant. That is a simplification, not a fix for a measured cost, and
the wide path already exists and is tested.

**4,096 is rezolus's cap, not the kernel's.** `MAX_CGROUPS`
(`src/agent/bpf/cgroup.h:10`, from #582) sizes rezolus's BPF maps, and a
cgroup whose id is 4,096 or more is dropped in BPF with no count and no
status (`cgroup.h:42`, and `:126` in `handle_new_cgroup_from_css`; the same bound at `mod.bpf.c:361`, `:421`). That is a
silent gap on a host with more than about 4,094 live CPU-controller cgroups,
counting dying ones that still hold their ids. Tracked in the backlog.

**A detail for any future move to long:** each sampler has its own cgroup
`SlotIdentity` (seven of them), so one cgroup gets a different `__uid__` in
each sampler. An occupant key shared across cgroup tables would have to be
matched by labels, not by uid.

**Reopen** if a recording from a host with thousands of live cgroups and
heavy pod or job churn shows cgroup-table footers or the index join as a
measurable cost. No such recording exists yet; cgroup tables at that scale
are unmeasured.

## The per-task table: long

**Per-thread is a requirement.** The task table exists to show processes with
complex threading, such as a thread pool per kind of work, so it is kept per
thread (per kernel task), not rolled up to the process. Its identity labels,
`pid` (the TID), `tgid` and `comm`, are what let a query take CPU by pool
within one process: `sum by (comm) (rate(task_cpu_usage{tgid="…"}[1m]))`.

**Is the per-task data worth keeping** (checked 2026-09-25). Its cost: the
`task_cpu_usage` BPF array is 32 MiB of preallocated kernel memory
(`MAX_PID` × `u64`); the other per-task maps in `cpu_usage` (`task_utime`,
`task_stime`, `task_start_times`, 96 MiB) are needed anyway, since per-CPU and
per-cgroup usage are computed from their per-task deltas. Downstream it is
most of a recording (434 of 579 MB in the 5.22.0 one, 668 of 1,272 MB in the
older one) and most of the wire churn (#1224). Nothing built into rezolus
reads it: no dashboard section, MCP tool or feature extraction queries it.
It is used through queries: the insights-model evaluations attribute CPU to
threads by `comm` and `tgid` with it, and the `measure-performance` skill
reads server threads' CPU from it. Those evaluations also record it missing
CPU that the cgroup counters saw; that is the backlog item "Per-task
`task_cpu_usage` has missed CPU the cgroup counters saw" (`docs/backlog.md`,
"Agent — per-task CPU usage completeness"). The decision is to build the long
layout regardless, since dendro should handle this cardinality whatever the
sampler does, and to fix the sampler's completeness separately so the data
earns its cost.

**How wide it can get.** The group is sized at `MAX_PID = 4194304` (2^22,
`src/agent/bpf/task.h:14`; `TASK_CPU_USAGE: CounterGroup::new(MAX_PID)`,
`src/agent/samplers/cpu/linux/usage/stats.rs:150`). A task's exported counter
is zeroed first at exit (`mod.bpf.c:427`) and membership follows non-zero
values, so a task has a column only if it was alive with a value at some
sample tick. A segment's width is therefore the number of distinct tasks alive
at a tick during it: the live set plus every task that lived about a sample
interval or longer in those five minutes. That was 14,011 at the median and
38,388 at most on the busy host. A thread-per-request workload raises it with
every request that outlives an interval, and at 100 ms sampling that is most
of them.

**Why long.** In a wide table every distinct task is a column: its chunk
metadata, statistics and page-index entries, a null cell for each tick it was
not alive, and a column builder in the writer. That overhead grows with the
number of distinct tasks, which a spike can take to hundreds of thousands per
segment whatever the field metadata holds. A long table's cost grows with
observations instead: one row per tick at which a task had a value, and a
column set that never changes. The data stored is the same; wide adds a
per-task overhead on top.

**The layout.**

- Columns: `timestamp`, the tick's `:wall_offset`, `:window_begin` and
  `:window_width`, `occupant: UInt64`, and one column per metric in the
  group. No null cells.
- **Rows are keyed by occupant, not by TID.** An occupant is one immutable
  label set: `SlotIdentity::set` (`src/agent/identity.rs:311`) mints a new
  generation and `__uid__` whenever a slot's labels change, including a
  thread renaming itself (`pthread_setname_np`). So a row names its own
  series, the reader groups rows by key into series through a plain map, and
  no row depends on the index putting a TID's handover on the right side of
  a tick. The TID is an ordinary label, `pid`, beside `tgid` and `comm`.
- **The key is a dense number the writer assigns per source**, 0, 1, 2… in
  order of first sight, not a UUID. A time-based UUID (v7) was considered: it
  is 16 bytes per row where an observation is otherwise one 8-byte value, and
  uniqueness across hosts is not needed inside a source, whose `__uid__`
  labels already carry the producer epoch. The agent's own generation
  counter is not available to a scrape recording, which sees only its hash.
  A dense number increases monotonically, repeats in runs once sorted, and
  works the same for `--stream`, scrape and 5.18–5.20 recordings (where it is
  an internal key and claims nothing a label would). `u64`, since the busy
  host minted about 396,000 occupants in 2.3 hours. Pyroscope's `uint32
  SeriesIndex` is the same shape (Prior art).
- **The index for a long table** is the occupant table: one `caller_rows`
  entry per occupant, at the time it was first seen, mapping its number to
  its labels (with the real `__uid__` where the recording has one). It is
  kept under one name per identity, `identity/task`, which `caller_rows`
  allows for a name no stream uses, and it is restated and evicted as the
  time-keyed index is. The writer keeps the map from `__uid__` (or label set)
  to number beside its `SourceIndex`.
- **The write rule:** a group whose slot space is the PID space is written
  long; bounded groups (per-CPU, cgroup, device) stay wide. The agent knows
  which a group is when it declares it, so the writer never has to see the
  data to choose. A converter applies the same rule by group name.

**Row order: arrival order at seal by default, sorted where a re-encode
already happens; whether to sort at seal is measured, not assumed.** A tick's values arrive in ascending slot order (a row's values are
the live slots by rank, `crates/rez/src/index.rs`), so a segment written as it
arrives is ordered by `(timestamp, slot)` at no cost. Sorting it by
`(occupant, timestamp)` would make each thread's samples contiguous, let a
single-thread query skip pages on the page index's `occupant` bounds, and make
timestamp deltas small within a thread. But on the busy host's worst segment that is about
1.2M rows (10.6% of 301 ticks × 37,644 slots) to sort and permute at seal, on
the path that already showed a 73 ms p99.9 stall at 50 ms sampling when the
buffer ran in-process (#1224). So:

- the writer seals in arrival order;
- the converter sorts, since it runs offline;
- dendro's compaction, which already decodes and re-encodes a run of segments
  (`concat_parquet`, iopsystems/dendro `src/rewrite.rs:254`), sorts when given
  a sort key: a `CompactSpec` field naming columns, which keeps dendro from
  interpreting values;
- a sorted segment declares its order in parquet's `sorting_columns`, which is
  per row group, so every row group carries it. A reader uses the page index
  on `occupant` whether or not a segment declares a sort: pruning on page min/max
  is correct on unsorted data, only less selective, and `sorting_columns` lets
  it binary-search the bounds instead of scanning them.

In arrival order a single-thread query decodes every page of the segment's
`occupant` column, since every page spans every thread. Occupant numbers are
assigned in order of first sight, so within a tick they are close together;
how well that compresses is part of the gate.

Deferring the sort has precedent and so does not deferring it (see Prior
art): TimescaleDB, Iceberg and Delta sort when they compress or rewrite, off
the insert path, while InfluxDB 3.0 sorts at persist and ClickHouse sorts
every insert part, both because sorted files compress much better. So the
gate measures what sorting at seal would cost against what it saves, and the
default stays arrival order only if the saving is small.

**The reader cost.** Neither the `.rez` reader nor metriken-query reads a long
table: both expect a column per series. The new reader on dendro's API has to
group rows into series by slot and occupant. It is being written anyway; this
adds to it rather than adding a component.

**Gate (confirms long; does not choose between candidates).** Convert
`cpu_usage_task` from the busy recording (10.6% non-null), the quiet one
(98%), and a synthetic thread-per-request spike at 1 s and 100 ms sampling,
both wide-bare (identity removed, column per slot) and long. Measure:

- bytes on disk;
- bytes read to open the stream;
- time and peak RSS for `sum(rate(task_cpu_usage[1m]))` and for a
  single-thread selector, on long segments in arrival order and sorted;
- seal time for the worst busy-host segment, arrival order against sorted,
  next to the parquet encode it is added to;
- the compression difference between arrival order and sorted. Short-lived
  threads give short runs per slot, and TimescaleDB's guidance is that a
  segment-by group needs on the order of 100 rows to compress well, so the
  saving from sorting may be smaller here than the prior art's figures;
- the derived index's size: one `Delta` per thread arrival, about 396,000 over
  the busy recording's 2.3 hours, which retention has to bound alongside the
  segments.

Long stands if it is no larger on disk, reads less at open, and does not lose
the single-thread query badly enough that the dashboard's common case
regresses. Sorting moves to seal if it saves substantially on disk for a seal
cost well inside the tick budget. Losing narrowly on the quiet host is acceptable, as long as the
rule never produces a pathological case. If arrival order is cheap enough on
the read side, the compaction sort is left out.

## Prior art

Collected 2026-09-25; each claim is from the linked page.

- **Wide sparse schemas are costly in parquet.** InfluxData measured about
  700 bytes and 5 µs of footer decode per extra column with statistics off,
  about 30% more with them on
  (https://www.influxdata.com/blog/how-good-parquet-wide-tables/). OTel-Arrow
  splits data points and attributes into narrow tables linked by id, because
  "a column without value still continues to consume memory space"
  (https://arrow.apache.org/blog/2023/06/26/our-journey-at-f5-with-apache-arrow-part-2/).
  InfluxDB 3.0 stores tags as columns, one row per point
  (https://docs.influxdata.com/influxdb3/cloud-dedicated/reference/internals/storage-engine/).
  FrostDB (Parca) is wide by label *name*, a bounded set, not by entity.
- **Identity in an index, a small integer in the data.** Pyroscope's
  `profiles.parquet` carries a `uint32 SeriesIndex` resolved through
  `index.tsdb`
  (https://grafana.com/docs/pyroscope/latest/reference-pyroscope-architecture/block-format/),
  and the Cortex parquet proposal splits a labels file from a chunks file
  (https://cortexmetrics.io/docs/proposals/parquet-storage/). Neither handles a
  key the OS reuses, as it reuses TIDs. The long task table avoids the
  question by keying rows on an occupant number, never a TID; the wide tables
  resolve `(slot, time)` to an occupant through the index; 5.21+ recordings
  carry `__uid__` as the fingerprint across recordings.
- **Sort at rewrite:** TimescaleDB applies `orderby` when it compresses an aged
  chunk
  (https://github.com/timescale/docs.timescale.com-content/blob/master/using-timescaledb/compression.md);
  Iceberg Z-orders only in `rewrite_data_files`; Delta's `OPTIMIZE ZORDER BY`
  is a manual rewrite (https://docs.delta.io/latest/optimizations-oss.html).
- **Sort at write:** InfluxDB 3.0's ingester sorts at persist and reports
  files "often 10-100x smaller"
  (https://www.influxdata.com/blog/influxdb-3-0-system-architecture/);
  ClickHouse sorts each insert part, at a documented insert-time cost
  (https://clickhouse.com/docs/engines/table-engines/mergetree-family/mergetree);
  OTel-Arrow's compression gain rose from 1.4–1.67x unsorted to 4.94–7.21x
  sorted.
- **Index churn:** VictoriaMetrics notes that a high series churn rate grows
  the inverted index and slows long-range queries
  (https://docs.victoriametrics.com/victoriametrics/faq/).
- **Dropping `ARROW:schema`:** arrow-rs has
  `ArrowWriterOptions::with_skip_arrow_metadata`. Field metadata is where
  `metric` and `id` live, so this layout keeps the entry, made small by holding
  fixed facts only.

No published on-disk layout for per-thread metrics from an eBPF telemetry tool
was found.

## Next

1. Run the gate above, including a synthetic thread spike.
2. Fix `docs/labels.md`'s identity list (`name`).
3. The rezolus reader for dendro archives, on dendro's API, reading this
   layout: identity from `caller_rows`, one schema read per stream.
4. The reshaping converter, with the `--stream` recordings as its oracle: a
   derived index must match a recorded one, and every series must read back
   the same through the new reader as through today's `.rez` reader.
5. The 6.0 writer writes this layout (#1224).

## Related

- #1224, the 6.0 plan; §2 is the identity index this layout completes.
- #1301, the byte-copying converter this replaces.
- [Internal labels](2026-09-22-internal-labels.md), which introduced
  `__uid__` and the index reader.
- [The recorder consumes the replication stream](2026-09-22-recorder-stream-ingest.md),
  the only producer of a recorded index today.
- [Long recordings: memory proportional to the query](2026-09-23-reader-memory.md),
  which measured the same 1.28 GB recording.
- iopsystems/dendro#17 (the two-forms ladder) and #20 (stream summaries).
