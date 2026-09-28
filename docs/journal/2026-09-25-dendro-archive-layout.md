# The layout of a rezolus dendro archive

- **Opened:** 2026-09-25
- **Status:** **OPEN — design, nothing built.** Intent-first: this is what the
  reshaping `recording upgrade --to dendro` will write, and what the 6.0 writer
  (#1224) must then write identically. Revised 2026-09-27 after the gate
  (below): every group table with slots is written long, keyed by occupant.
  The first version made only the per-task table long and kept cgroup,
  per-CPU and device tables wide; measurement showed long smaller for all of
  them.
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
CPU that the cgroup counters saw. Measured on delta (2026-09-25/26, against the
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
`occupant` bounds. The gate measured what that buys (below): a third off the
task table, and a single-thread read about 50 times smaller on the busy host
and 110 times smaller on the quiet one.

- The converter sorts, since it runs offline.
- dendro's compaction, which already decodes and re-encodes a run of
  segments (`concat_parquet`, iopsystems/dendro `src/rewrite.rs:254`), sorts
  when given a sort key: a `CompactSpec` field naming columns, which keeps
  dendro from interpreting values.
- The writer seals in arrival order for now. The sort cost is at most 16–19
  ms per task segment (below), on the seal path that showed a 73 ms p99.9
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

## Gate results (2026-09-27)

A scratch harness (not committed) read each table's segments from two
recordings in `~/Downloads` and re-encoded every segment three ways, with the
`.rez` segment writer's settings (LZ4_RAW, dictionary off,
`crates/rez/src/rez.rs:373`):

- **wide-bare:** the `.rez` layout with identity labels removed from field
  metadata;
- **long, arrival:** one row per tick and occupant, in the order the rows
  arrive;
- **long, sorted:** the same rows sorted by `(occupant, timestamp)`.

"All" is a full decode of every segment. "One" reads the timestamp and the
single occupant with the most observations, using the page index on long
tables. Times are wall-clock totals over all segments of the table.

The busy host is the 5.22.0 recording (2.3 h, 10.6% of task cells non-null);
the quiet one is the 5.18–5.20 recording (9.6 h, 98%).

| table | host | segments | wide-bare MB | long arrival MB | long sorted MB | one: wide → arrival → sorted, MB read |
|---|---|---|---|---|---|---|
| `cpu_usage_task` | busy | 32 | 273.3 | 39.2 | **26.3** | 219.4 → 37.6 → 0.76 |
| `cpu_usage_task` | quiet | 159 | 524.0 | 505.4 | **348.7** | 181.2 → 496.0 → 4.35 |
| `syscall_counts_cgroup` | busy | 28 | 12.5 | 1.7 | **1.5** | 9.75 → 0.22 → 0.26 |
| `syscall_counts_cgroup` | quiet | 116 | 54.5 | 7.6 | **6.8** | 42.2 → 0.92 → 1.07 |
| `cpu_usage_cpu` | busy | 28 | 2.8 | 2.2 | **2.1** | 0.82 → 0.86 → 0.84 |
| `cpu_usage_cpu` | quiet | 116 | 11.9 | 9.3 | **8.9** | 3.44 → 4.46 → 4.39 |
| `drivehealth_sweep` | quiet | 116 | 1.0 | 0.4 | **0.4** | 0.88 → 0.29 → 0.35 |

Footers, the main cost being removed: `cpu_usage_task` busy 219.3 MB wide
against 0.05 MB long; `syscall_counts_cgroup` busy 9.69 MB against 0.11 MB.

Time, in ms:

| table | host | encode: wide / long sorted | all: wide / arrival / sorted | one: wide / arrival / sorted | sort |
|---|---|---|---|---|---|
| `cpu_usage_task` | busy | 1,620 / 299 | 1,224 / 189 / 145 | 598 / 85 / 5.4 | 502 |
| `cpu_usage_task` | quiet | 1,918 / 2,051 | 1,133 / 1,162 / 914 | 491 / 501 / 28 | 2,987 |
| `syscall_counts_cgroup` | busy | 75 / 25 | 79 / 13 / 25 | 28 / 2.5 / 3.6 | 27 |
| `syscall_counts_cgroup` | quiet | 304 / 113 | 321 / 53 / 104 | 116 / 11 / 15 | 111 |
| `cpu_usage_cpu` | busy | 9.8 / 7.5 | 5.0 / 2.7 / 2.8 | 2.2 / 1.1 / 1.3 | 4.4 |
| `cpu_usage_cpu` | quiet | 35.0 / 29.4 | 19.5 / 11.5 / 11.3 | 9.1 / 4.7 / 5.4 | 17.8 |

What the results show:

- **Long is smaller than wide for every table measured**, including the
  dense ones. Per-CPU (16 slots, always full) is the closest, 25% smaller.
  The mechanism is the per-column footer cost ("Cgroups" above).
- **Sorting matters for the task table and little elsewhere.** Sorted is a
  third smaller than arrival order on both hosts, and turns a single-thread
  read from 37.6 MB into 0.76 MB on the busy host and from 496 MB into 4.35
  MB on the quiet one. On the quiet host, arrival order read more than wide
  for one thread, because every page spans every thread.
- **The sort costs 16–19 ms per task segment** (502 ms over 32 segments,
  2,987 ms over 159), about 1 ms per cgroup segment. The harness sorts by
  rebuilding the arrays, so this is an upper bound.
- A full decode of the cgroup table was slower sorted than in arrival order
  (13 against 25 ms busy, 53 against 104 ms quiet) for the same bytes. I
  don't know why; it is small in absolute terms.

Not measured:

- a synthetic thread-per-request spike, and 100 ms sampling;
- peak RSS per query, and reads through a query engine: the harness reads
  parquet directly and prunes on the page index by hand, since no reader for
  long tables exists yet;
- the occupant index's size (about 396,000 entries over the busy
  recording's 2.3 hours);
- the drive table on the busy host, and tables whose metrics are histograms
  (`blockio_latency_device_latencies`): the harness handles only integer
  columns;
- mount, interface and GPU tables, which neither recording has with slots.

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

1. ~~Run the gate~~ — done 2026-09-27; every group with slots is long. A
   synthetic thread spike and 100 ms sampling are still unmeasured.
2. Fix `docs/labels.md`'s identity list (`name`).
3. Settle the occupant index's encoding (`u64` occupant numbers).
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
