# The layout of a rezolus dendro archive

- **Opened:** 2026-09-25
- **Status:** **OPEN — design, nothing built.** Intent-first: this is what the
  reshaping `recording upgrade --to dendro` will write, and what the 6.0 writer
  (#1224) must then write identically. Revised 2026-09-27 after the gate
  (below): every group table with slots is written long, keyed by occupant.
  The first version made only the per-task table long and kept cgroup,
  per-CPU and device tables wide; measurement showed long smaller for all of
  them. A synthetic thread and cgroup spike at 1 s and 100 ms (2026-09-28)
  confirmed it and found two things the design must handle: the occupant
  table needs a compressed encoding, and sorting helps reads but does not
  always shrink a table.
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
  `grouping_power`, `max_value_power`; `docs/labels.md`), `sampler`, and
  labels that are fixed per metric column (`op` on `syscall_counts_cgroup`,
  `kind` on softirq).
- **Moved to the occupant index:** the slot `id`, and a slot's identity
  labels, the ones `SlotIdentity::set` writes (`src/agent/identity.rs:311`):
  `comm`, `pid`, `tgid`, `cgroup`, `name`, `device`, `mount`, the
  hardware-sensor keys, and `__uid__`. A long table has no column per slot,
  so `id` has no column to sit on; the `.rez` indexed reader's use of it
  (`crates/rez/src/indexed.rs:237`) does not carry over. `docs/labels.md`'s
  identity list omits `name`, which cgroup slots set
  (`src/agent/bpf/mod.rs:339`); that document needs the fix.
- **Dropped:** `source` and `endpoint`, the per-column provenance older
  recorders wrote. Provenance is the source's (#1258, #1259).

**How the converter tells identity labels from fixed ones.** Not from a key
list, which is already incomplete. Where the recording carries an index
(`--stream`), the identity keys are exactly the keys its index entries hold.
Otherwise, for each metric in a group table, a label is identity if its value
differs between that metric's slot columns anywhere in the stream; a label
constant across all of them is fixed. A group with one slot has no other
column to compare with; its labels are identity if they change between
segments, and fixed otherwise. The `--stream` recordings
are the test: the derived split must equal the index's.

### Group tables with slots: long

Every group table with slots is written long, keyed by occupant: task,
cgroup, per-CPU, drive, mount, interface and GPU groups alike. The gate
(below) measured long smaller than wide for per-task, cgroup, per-CPU and
drive tables, on a busy and a quiet host.

- **Columns:** `timestamp`, the tick's `:wall_offset`, `:window_begin` and
  `:window_width`, `occupant: UInt64`, and one column per metric in the
  group. One row per tick and occupant that has a value, so a table has no
  null cells except where an occupant lacks one of the group's metrics at
  that tick.
- **An occupant is one immutable label set:** `SlotIdentity::set`
  (`src/agent/identity.rs:311`) mints a new generation and `__uid__`
  whenever a slot's labels change. A row names its own series, the reader
  groups rows into series through a plain map, and no row depends on the
  index putting a handover on the right side of a tick. The slot, as `id`,
  is an ordinary label of the occupant.
- **Occupants are numbered per stream,** 0, 1, 2… in order of first sight.
  Per stream rather than per source because each sampler has its own cgroup
  `SlotIdentity` (seven of them), so one cgroup has a different `__uid__` in
  each sampler's table; a numbering shared across tables would have to match
  occupants by labels. The reasons for a dense number rather than a UUID are
  under "The per-task table".
- **5.21+ `#N` columns** (#1232) are already one occupant each and become
  that occupant's rows. The converter refuses a table in which two
  occupants of one slot both have a value at the same tick, which would mean
  the premise that `#N` columns are disjoint is wrong.
- **The write rule:** a group with slots is written long, and a group with
  none is one row per tick. The agent knows which a group is when it
  declares it, so the writer never has to see the data to choose. The
  converter applies the same rule by whether a table has `{metric}x{slot}`
  columns.
- **Tables with no slots:** one row per tick, unchanged apart from the
  dropped provenance keys.

### The occupant index

With no slot columns in any stream, no stream needs the `.rez` time-keyed
index that resolves `(slot, time)` to an occupant. What a long stream needs
is its occupant table: which labels each occupant number stands for.

- One entry per occupant, at the time it was first seen, mapping its number
  to its labels, including `id` and, when the recording is `"uid"`, its
  `__uid__`. Written to `caller_rows` under the stream's name.
- An occupant is added once and never relabeled, since a label change is a
  new occupant. An occupant that has no further rows is removed, so the live
  set stays bounded by what the stream is currently observing.
- A restating entry every `RESTATE_EVERY` (300 s, `src/recorder/stream.rs:156`)
  of row time lists the live occupants, so a reader starts its replay at most
  one period before the rows it needs, and retention can evict entries older
  than the oldest surviving restatement.
- **Encoding: open.** `IndexEntry` (`crates/rez/src/index.rs:104`) has the
  right shape, a `Full` restatement and `Delta` additions and removals, but
  `SlotEntry::slot` is `u32`. The occupant number is `u64` because a
  rolling buffer runs indefinitely: at 100,000 new threads a second, `u32`
  wraps in about 12 hours. The encoding is settled with the writer.
- **It must be compressed.** Stored as plain msgpack entries, the busy
  host's occupant table was 62.3 MB, more than twice its 26.3 MB of long
  data. As one zstd blob per seal it was 8.1 MB, and as parquet 7.6–8.6 MB
  (Gate results). Either is acceptable. Plain per-entry blobs are not.

How the converter finds occupants:

- **A `--stream` recording** carries a time-keyed index. The converter
  replays it with `SourceIndex` (`crates/rez/src/index.rs:378`) to assign
  each row's slot to its occupant at that tick, then writes the occupant
  table. The recorded index is not copied.
- **A scrape recording:** each slot column's label set, with its `__uid__`
  where present, is an occupant. In a 5.18–5.20 recording, a slot whose
  labels differ from the previous segment's is a new occupant from that
  segment's first row.
- `__uid__` goes into an occupant's labels when the recording is `"uid"`. A
  `"labels"` recording has none, and none is minted: a minted uid would
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
kind was considered on its own. The first version of this entry decided
layout by how wide a table can get, made the unbounded task table long and
kept the bounded ones wide. The gate showed that rule wrong for bounded
tables too ("Cgroups" below), and every kind with slots is now long.

| identity | samplers | slot key | how many slots | churn | layout |
|---|---|---|---|---|---|
| task | `cpu_usage` (`cpu_usage_task`) | TID | unbounded up to `PID_MAX_LIMIT`; measured 14,011 per segment at the median, 38,388 max, 396,117 in 2.3 h | high: every thread that lives about a sample interval | **long, keyed by occupant** |
| cgroup | `cpu_usage`, `cpu_bandwidth`, `cpu_migrations`, `cpu_perf`, `cpu_tlb_flush`, `scheduler_runqueue`, `syscall_counts` | CPU-controller `css.id` | the host's live cgroups, capped by rezolus at 4,096; measured 31–57 | low to moderate: pod and job churn on a busy node | **long** (measured: 8x smaller than wide) |
| drive | `drivehealth` (sweep, NVMe) | drive index | 2 measured | rare | long (measured: 2.5x smaller) |
| mount | `filesystem` | mount index | 3 measured | rare (remount) | long (not measured) |
| interface | `network_ethtool` | interface index | a few | rare | long (not measured) |
| GPU engine, device, memory | `gpu` (Intel) | device index | a few | rare | long (not measured) |
| CPU | per-CPU groups | CPU id | CPUs (16 measured) | none; the slot is the identity | long, one occupant per CPU (measured: 25% smaller) |
| none | plain groups | — | — | — | one row per tick, unchanged |

### Tasks: why long

The TID space is `PID_MAX_LIMIT`, which on 64-bit is `4 * 1024 * 1024`
(`include/linux/threads.h:34`), the same as rezolus's `MAX_PID`
(`src/agent/bpf/task.h:14`), so the task group covers every TID the kernel can
give out and cannot overflow. A wide table's width is the number of distinct
threads a segment saw, which a thread-per-request workload drives up without
limit. The reasoning is in "The per-task table" below.

### Cgroups: long, reversed by the gate

The first version of this entry kept cgroup tables wide. Its argument: size,
not reassignment, rules out wide, and a cgroup table's width is bounded.
Those facts still hold, and they now bound the number of occupants rather
than columns:

- **The slot is the CPU controller's css id** (`task->sched_task_group->css.id`,
  `src/agent/bpf/cgroup.h:38`). The kernel allocates it with
  `cgroup_idr_alloc(&ss->css_idr, NULL, 2, 0, …)` (`kernel/cgroup/cgroup.c:5936`
  on current master): `idr_alloc` with no upper bound, returning the lowest
  free id. Ids are dense and a freed one is reused at once, so the largest id
  in use tracks the number of live CPU-controller cgroups.
- **Reassignment is detected.** A reused id comes with a new
  `css.serial_nr`, which `handle_new_cgroup` compares (`cgroup.h:37-53`), and
  `SlotIdentity::set` then mints a new occupant. Identity is captured with
  the values since #1249.
- **The tables are dense:** 98–100% full in both recordings.

The argument assumed that a dense column costs about what its values cost.
At the segment sizes the seal policy produces, about 290–300 rows, it does
not: each column costs about 520 bytes of footer per segment (measured:
9.69 MB of footer for 18,544 column-segments on the busy host, 41.93 MB for
80,272 on the quiet one), and 300 values of a slowly changing counter
compress to less than that. Long pays that cost once per metric instead of
once per metric and slot. Measured on `syscall_counts_cgroup`: 12.5 MB wide
against 1.5 MB long on the busy host, 54.5 MB against 6.8 MB on the quiet
one. Long also gives every group with slots one reader path.

Compaction into longer segments would spread wide's per-column cost over
more rows. It does not change the decision: under cgroup churn a merged
segment's columns are the union of its inputs', and long is smaller at the
segment size the writer produces, which is what a rolling buffer holds.

**4,096 is rezolus's cap, not the kernel's.** `MAX_CGROUPS`
(`src/agent/bpf/cgroup.h:10`, from #582) sizes rezolus's BPF maps, and a
cgroup whose id is 4,096 or more is dropped in BPF with no count and no
status (`cgroup.h:42`, and `:126` in `handle_new_cgroup_from_css`; the same
bound at `mod.bpf.c:361`, `:421`). That is a silent gap on a host with more
than about 4,094 live CPU-controller cgroups, counting dying ones that still
hold their ids. Tracked in the backlog. The layout does not depend on the
cap.

### Groups without slots: one row per tick

A group whose members are fixed metrics, told apart by a fixed label such
as `op`, has no occupants: nothing is reassigned and the column count never
changes. Long would replace a column per metric with an `occupant` column
that names the metric. It would save the per-column footer and nothing
else, and a read of one metric would need page pruning to match what
reading its own column gives today. Measured (2026-09-28) on the three histogram tables of
the two recordings (busy / quiet), the footer is a small share:

| table | columns | size | footer |
|---|---|---|---|
| `blockio_latency_device_latencies` | 8 | 4.25 / 17.75 MB | 2.8% / 3.1% |
| `syscall_latency_latencies` | 20 | 31.2 / 114.3 MB | 2.6% / 3.5% |
| `scheduler_runqueue_runqlat` | 5 | 4.5 / 17.7 MB | 1.2% / 1.4% |

Several of those columns are idle. `op=flush` is zero on every row of both
recordings, and `op=discard` changed on 33 of 8,180 rows (busy) and 30 of
34,678 (quiet). Each is about 3% of the table, because LZ4 compresses a
repeated 496-bucket list well. Long would not remove them: the cells are
not null, since a cumulative histogram has a value every tick.

**Considered and declined: writing a value only when it changes.** The
reader would hold the last value forward. It would remove idle columns'
rows and idle threads' rows in the per-thread table, but it costs a
comparison against the previous value of every cell at write time, and it
makes a missing row ambiguous between "absent" and "unchanged", which the
staleness bound and occupant liveness rely on.

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
CPU that the cgroup counters saw. Measured on a 32-CPU x86_64 host on kernel 6.12 (2026-09-25/26, against the
kernel's `cpu.stat`): long-lived threads are within 1%, but under heavy thread
churn the task event ring buffer overflows, and because per-task accounting is
tied to metadata delivery the loss reaches the host and cgroup totals (system
time 69% short); short-lived tasks lose CPU to how `rate()` reads a series that
starts above zero. The mechanisms, measurements and ordered fixes are in
`docs/backlog.md`, "Agent — per-task CPU usage completeness". The decision is
to keep per-task telemetry, build the long layout regardless (dendro should
handle this cardinality whatever the sampler does), and fix the sampler, the
first fix being to stop per-task metadata delivery from affecting totals.

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

**The layout** is the one every group with slots uses ("Group tables with
slots: long"). For the task table, `pid` (the TID), `tgid` and `comm` are
occupant labels, and a thread renaming itself (`pthread_setname_np`) is a new
occupant.

**Why a dense number, not a UUID.** A time-based UUID (v7) was considered
for the occupant key: it is 16 bytes per row where an observation is
otherwise one 8-byte value, and uniqueness across hosts is not needed inside
a stream, whose `__uid__` labels already carry the producer epoch. The
agent's own generation counter is not available to a scrape recording, which
sees only its hash. A dense number increases monotonically, repeats in runs
once sorted, and works the same for `--stream`, scrape and 5.18–5.20
recordings (where it is an internal key and claims nothing a label would).
Pyroscope's `uint32 SeriesIndex` is the same shape (Prior art); rezolus uses
`u64` because a rolling buffer can outlive `u32` (see "The occupant index").

**Row order: arrival order at seal, sorted by `(occupant, timestamp)` where
a re-encode already happens.** A tick's values arrive in ascending slot
order (a row's values are the live slots by rank, `crates/rez/src/index.rs`),
so a segment written as it arrives is ordered by `(timestamp, slot)` at no
cost. Sorting by `(occupant, timestamp)` makes each thread's samples
contiguous and lets a single-thread query skip pages on the page index's
`occupant` bounds. The gate measured what that buys (below): a single-thread
read about 50 times smaller on the busy host and 110 times smaller on the
quiet one. The disk size moves either way: sorting took a third off the
task table on both existing recordings, but made the spike's churning
tables up to 50% larger, because a thread that lives a few ticks has no run
for sorting to compress.

- The converter sorts, since it runs offline.
- dendro's compaction, which already decodes and re-encodes a run of
  segments (`concat_parquet`, iopsystems/dendro `src/rewrite.rs:254`), sorts
  when given a sort key: a `CompactSpec` field naming columns, which keeps
  dendro from interpreting values.
- The writer seals in arrival order for now. The sort cost is at most 32 ms
  for the largest segment measured (below), on the seal path that showed a 73 ms p99.9
  stall at 50 ms sampling when the buffer ran in-process (#1224). Whether to
  sort at seal is decided with the 6.0 writer, measured on its own seal path.
- A sorted segment declares its order in parquet's `sorting_columns`, which
  is per row group, so every row group carries it. A reader uses the page
  index on `occupant` whether or not a segment declares a sort: pruning on
  page min/max is correct on unsorted data, only less selective.

Deferring the sort has precedent and so does not deferring it (see Prior
art): TimescaleDB, Iceberg and Delta sort when they compress or rewrite, off
the insert path, while InfluxDB 3.0 sorts at persist and ClickHouse sorts
every insert part.

**The reader cost.** Neither the `.rez` reader nor metriken-query reads a long
table: both expect a column per series. The new reader on dendro's API has to
group rows into series by occupant. It is being written anyway, and with
every group with slots long it has one path for all of them.

## Gate results (2026-09-27, spike added 2026-09-28)

A scratch harness (not committed) reads a table's segments from a v3 `.rez`
and re-encodes every segment three ways, with the `.rez` segment writer's
settings (LZ4_RAW, dictionary off, `crates/rez/src/rez.rs:373`):

- **wide-bare:** the `.rez` layout with identity labels removed from field
  metadata;
- **long, arrival:** one row per tick and occupant, in the order the rows
  arrive;
- **long, sorted:** the same rows sorted by `(occupant, timestamp)`.

Long metric columns keep the fixed field metadata a metric column carries.
Each long segment holds the rows of one source segment, so it inherits the
wide writer's segmentation. A long writer would seal on its own byte cap
and write fewer, longer segments, so the long figures here are conservative.

"All" is a full decode of every segment. "One" reads the timestamp and the
single occupant with the most observations, pruning pages on the page
index's `occupant` bounds. Times are wall-clock totals over all segments of
the table. The occupant table is sized as one msgpack entry per occupant
(its number and labels) plus a restatement of the live set every 300 s.

### Two existing recordings

The busy host is the 5.22.0 recording (2.3 h, 10.6% of task cells non-null);
the quiet one is the 5.18–5.20 recording (9.6 h, 98%). These figures are
from the second harness version (2026-09-28). The first left field metadata
off long columns; they changed by 0.2 MB or less, except the drive table,
which went from 0.4 to 0.6 MB.

| table | host | segments | wide-bare MB | long arrival MB | long sorted MB | one: wide → arrival → sorted, MB read |
|---|---|---|---|---|---|---|
| `cpu_usage_task` | busy | 32 | 273.3 | 39.2 | **26.3** | 219.4 → 37.8 → 0.77 |
| `cpu_usage_task` | quiet | 159 | 524.0 | 505.4 | **348.7** | 181.3 → 496.8 → 4.39 |
| `syscall_counts_cgroup` | busy | 28 | 12.5 | 1.8 | **1.7** | 9.75 → 0.39 → 0.39 |
| `syscall_counts_cgroup` | quiet | 116 | 54.5 | 8.1 | **7.4** | 42.2 → 1.61 → 1.64 |
| `cpu_usage_cpu` | busy | 28 | 2.8 | 2.2 | **2.1** | 0.82 → 0.90 → 0.86 |
| `cpu_usage_cpu` | quiet | 116 | 11.9 | 9.4 | **9.0** | 3.44 → 4.60 → 4.50 |
| `drivehealth_sweep` | quiet | 116 | 1.0 | 0.6 | **0.6** | 0.88 → 0.56 → 0.56 |

On the busy host the drive table has 2 occupants and 137 rows, and all three
layouts are 0.1 MB.

Footers, the main cost being removed: `cpu_usage_task` busy 219.3 MB wide
against 0.06 MB long; `syscall_counts_cgroup` busy 9.69 MB against 0.25 MB.

Time, in ms:

| table | host | encode: wide / long sorted | all: wide / arrival / sorted | one: wide / arrival / sorted | sort |
|---|---|---|---|---|---|
| `cpu_usage_task` | busy | 1,720 / 353 | 1,340 / 205 / 159 | 637 / 93 / 6.2 | 537 |
| `cpu_usage_task` | quiet | 2,020 / 2,241 | 1,173 / 1,210 / 959 | 497 / 517 / 29 | 2,688 |
| `syscall_counts_cgroup` | busy | 78 / 35 | 83 / 14 / 26 | 28 / 3.2 / 3.9 | 29 |
| `syscall_counts_cgroup` | quiet | 320 / 144 | 325 / 54 / 106 | 116 / 13 / 16 | 113 |
| `cpu_usage_cpu` | busy | 10.0 / 8.5 | 5.3 / 3.0 / 3.0 | 2.4 / 1.3 / 1.5 | 4.1 |
| `cpu_usage_cpu` | quiet | 34.1 / 30.5 | 19.2 / 11.6 / 11.4 | 9.0 / 5.1 / 5.6 | 15.8 |

### A synthetic spike

Recorded on a 32-CPU x86_64 host running kernel 6.12 at 250 Hz. 16 of its
CPUs are isolated by `isolcpus` and `nohz_full`, so the load ran on the
other 16. A `main` build (5.22.2-alpha.11) was both agent and recorder. Each recording is
700 s: 30 s quiet, then 600 s of:

- **thread-per-request:** 300 threads a second (180,000 in all), each named
  into one of four pools, living 0.5–5 s, burning a fixed amount of CPU at
  start and sleeping out the rest;
- **cgroup churn:** 5 jobs a second (2,990 in all), each in its own cgroup
  with the CPU controller on, burning 50 ms and living 10–60 s.

Four recordings: the thread CPU was 0.5 ms ("light") or 10 ms ("heavy"), each
at 1 s and 100 ms.

| table | run | segments | occupants | wide-bare MB | long arrival MB | long sorted MB | one: wide → arrival → sorted, MB read |
|---|---|---|---|---|---|---|---|
| `cpu_usage_task` | light, 1 s | 3 | 24,976 | 16.4 | **1.6** | 1.9 | 13.2 → 1.53 → 0.18 |
| `cpu_usage_task` | light, 100 ms | 17 | 26,917 | 29.0 | **9.6** | 11.2 | 22.9 → 8.65 → 0.23 |
| `cpu_usage_task` | heavy, 1 s | 3 | 177,854 | 99.4 | **3.4** | 5.1 | not measured |
| `cpu_usage_task` | heavy, 100 ms | 23 | 184,332 | 133.5 | 24.1 | **19.1** | 110.2 → 22.8 → 0.43 |
| `syscall_counts_cgroup` | light, 1 s | 6 | 3,079 | 43.6 | **0.9** | **0.9** | 36.9 → 0.14 → 0.12 |
| `syscall_counts_cgroup` | light, 100 ms | 53 | 3,292 | 167.3 | **4.7** | 6.1 | 141.2 → 1.02 → 0.87 |
| `syscall_counts_cgroup` | heavy, 100 ms | 53 | 3,291 | 168.8 | **4.4** | 5.0 | 142.7 → 1.01 → 0.87 |
| `cpu_tlb_flush_cgroup` | light, 1 s | 3 | 3,074 | 9.5 | 0.6 | **0.5** | 7.79 → 0.30 → 0.09 |
| `cpu_tlb_flush_cgroup` | light, 100 ms | 17 | 3,289 | 20.1 | **2.3** | 3.5 | 16.1 → 1.15 → 0.33 |
| `cpu_usage_cpu` | light, 1 s | 3 | 32 | 0.5 | 0.3 | 0.3 | 0.16 → 0.12 → 0.12 |
| `cpu_usage_cpu` | heavy, 100 ms | 8 | 32 | 2.4 | 2.1 | 2.1 | 0.48 → 0.75 → 0.61 |

The heavy 1 s cgroup and per-CPU tables match the light 1 s ones to within
0.1 MB. The heavy 1 s task table is the one arrow-rs cannot open (below), so
its row is from a pyarrow version of the harness. On the light 1 s task
table, which both can read, pyarrow gave 15.4 / 1.4 / 1.8 MB against the Rust
harness's 16.4 / 1.6 / 1.9, so its figures run about 10% lower.

What the spike adds:

- **Long is smaller for every table and run.** Cgroup churn is where wide
  does worst: `syscall_counts_cgroup` is 48 times smaller long at 1 s and 36
  times at 100 ms, because every short-lived cgroup is a column in every
  segment it touched.
- **Sorting does not always shrink a table.** With short-lived occupants
  a sorted segment is often larger than arrival order: the light task table
  at 100 ms is 11.2 MB sorted against 9.6 MB, the cgroup table 6.1 against
  4.7. A long-lived occupant gives a long run of close timestamps and slowly
  changing values when sorted; a thread that lives a few ticks does not, and
  arrival order keeps each tick's timestamps together instead. Sorted was
  smaller where occupants have many rows each: the heavy task table at 100 ms
  (threads of 0.5–5 s give 5–50 rows; 19.1 against 24.1 MB) and both existing
  recordings. The same threads at 1 s give 1–5 rows, and sorted was larger
  (5.1 against 3.4 MB). A
  single-occupant read is always far smaller sorted (0.23 against 8.65 MB),
  which is what sorting is for; the disk size is a side effect, in either
  direction.
- **Sort cost at seal:** the worst segment of any table measured took 32 ms,
  about 526,000 rows, on both the busy host's task table and the heavy 100 ms
  spike's. The harness sorts by rebuilding the arrays, so this is an upper
  bound.
- **The occupant table is large unless compressed.** As plain msgpack it
  is 62.3 MB for the busy host's 396,117 occupants, more than twice the
  26.3 MB of long data, and 24.4 MB for the heavy 100 ms spike's 184,332
  against 19.1 MB. The cgroup path is about half of each entry (61 of
  roughly 131 label bytes), and one busy segment's 2,600 occupants share 22
  cgroups and 227 `tgid`s. Measured on the busy host's occupants:
  compressing each seal's new entries as one zstd blob gives 8.1 MB; the
  same table as parquet (zstd, dictionary on) gives 7.6 MB as one file and
  8.6 MB as 32. The floor is the `__uid__` values, which are random: 396,117
  × 8 bytes is 3.2 MB. Restatements add little on a busy host (6.3 MB) and
  most of the size on a quiet one: 26.0 MB there, against 0.7 MB of first
  sightings, from 116 restatements of about 2,500 live threads.
- **Threads under one tick of CPU mostly get no series.** The light runs
  started 180,000 threads and the task table saw 24,976, about 14%. The
  load's CPUs use tick accounting at 250 Hz, and `cpuacct_account_field` is
  charged per tick, so a thread that runs 0.5 ms is charged only if a tick
  lands while it runs: 0.5 / 4 ms is 12.5%. Totals are right on average, since
  the thread that is charged gets a full tick. The heavy runs (10 ms, over
  two ticks) saw nearly every thread: 177,854 occupants at 1 s. Recorded in the backlog under per-task
  completeness.
- **A wide task segment can become unreadable.** At 1 s the heavy spike's
  task segments reached 90,227 columns, sealed by the 5-minute age bound (at
  100 ms the 8 MiB byte cap seals them near 11,000). arrow-rs could not open
  the segment of 52,109 columns (a 36.4 MB `ARROW:schema`): it fails with
  `TooManyTables`, because it verifies that flatbuffer with
  `VerifierOptions::default()` (arrow-ipc 58, `src/convert.rs:990`), whose
  `max_tables` is 1,000,000. rezolus's reader
  then reports the table as evicted (`crates/rez/src/reader.rs:1289`), which
  is not what happened. The busy host's largest segment, 38,388 columns,
  opens; the exact threshold between the two was not found. This is a defect of the `.rez` layout today, independent of the
  dendro work; the long layout removes it. Tracked in the backlog.

### Not measured

- reads through a query engine: the harness reads parquet directly and
  prunes on the page index by hand, since no reader for long tables exists;
- peak memory per query;
- mount, interface and GPU tables, which neither recording has with slots.
  No sampler produces a slotted histogram group, so there is no histogram
  case.

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
  key the OS reuses, as it reuses TIDs. The long tables avoid the question by
  keying rows on an occupant number, never a slot; 5.21+ recordings carry
  `__uid__` as the fingerprint across recordings.
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
  `metric` and the other storage keys live, so this layout keeps the entry, made small by holding
  fixed facts only.

No published on-disk layout for per-thread metrics from an eBPF telemetry tool
was found.

## Next

1. ~~Run the gate~~ — done 2026-09-27, with a synthetic spike at 1 s and
   100 ms on 2026-09-28; every group with slots is long.
2. Fix `docs/labels.md`'s identity list (`name`).
3. Settle the occupant index's encoding: `u64` occupant numbers, compressed.
4. The rezolus reader for dendro archives, on dendro's API, reading this
   layout: occupant labels from `caller_rows`, rows grouped into series by
   occupant, one schema read per stream.
5. The reshaping converter, with the `--stream` recordings as its oracle:
   occupants derived from a recording's columns must match those its
   recorded index gives, and every series must read back the same through
   the new reader as through today's `.rez` reader.
6. A sort key in dendro's `CompactSpec`.
7. The 6.0 writer writes this layout (#1224), and measures sorting at seal.

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
