# Filesystem telemetry gaps, from a file-per-object cache characterization

- **Opened:** 2026-09-28
- **Status:** **OPEN — scoping, nothing built.** Lists the filesystem-level
  measurements a storage characterization had to take by hand, states what an
  always-on sampler would have said instead, and orders the work. Builds on
  `2026-09-28-ext4-sampler.md`, whose phase 1 (`ext4_journal`) shipped in
  #1321 and whose phases 2 and 3 this entry re-prioritizes.
- **Driver:** a characterization of a cache that stores one file per object
  (about 56 KiB each, tens of millions of files per host, several TB on a
  local NVMe), evicts by file atime, and writes each object as a temp file,
  `fsync`, then rename. Every finding below was reached with a one-off tool
  — off-CPU profiles, `e2freefrag` after an 8 h soak, `/proc/fs/jbd2/*/info`,
  `perf`, kworker CPU accounting from `top` — run by someone who already
  suspected the answer. Rezolus was on the hosts and could name none of the
  mechanisms. The findings are the requirements; the workload is described
  only as far as the requirements need.
- **Owner:** Brian Martin

## What the characterization found, and what measured it

| finding | how it was measured | what rezolus should have said, continuously |
|---|---|---|
| Overwriting hot keys shredded ext4 free space. Collapse at 55–60 minutes: p50 1.6 ms → 218 ms, throughput halved. After 8 h: 894 GB free in 10.3 M extents averaging 91 KB, 73% too small for one object; files averaged 2.1 extents. A kworker went from 0.006 to 0.743 of a core in `mballoc`; device latency flat; block-layer queue latency up 91x. | `e2freefrag` offline; `top` on the kworker; blockio queue latency (the one thing rezolus had) | allocator effort per allocation rising (block groups scanned, allocation criterion falling to the slow paths), allocated extent length falling below the request, and a rate of allocations per file above 1 — hours before the collapse, on the host that will collapse |
| The same collapse on kernel 6.12 and 7.0; a suspected kernel regression was withdrawn. | repeating the soak on a second kernel | the same allocator signals on both hosts, so the comparison is two time series rather than two soaks |
| atime is required and costs a cold inode-table read on the request path: `__ext4_get_inode_loc` off-CPU 1.46 s → 12.03 s, and requests queued behind a per-request permit for 87.85 s of lock wait. `vm.vfs_cache_pressure=1` removed it: p99 2.01 → 0.856 ms, device writes 26,711 → 2 MiB per 300 s, p99.99 6.7x worse for an unknown reason. | off-CPU and lock profiles; two sysctl arms | inode-table block reads per second, which is the synchronous read on the request path; the rate of deferred (`lazytime`) timestamp writes; and inode-cache residency, so "does the cache fit" is a gauge rather than a hope |
| The write path loses the SLO: at 45 M objects the p99 crosses 1 ms at about 3.5% writes, and production is 12.5%. Removing the `fsync` cut device writes 19% and made p99.99 56x worse (2.01 → 113 ms) because dirty pages then flushed in bursts that throttled writers. | a write-ratio sweep; an `LD_PRELOAD` shim | fsync latency at the filesystem layer (`ext4_journal` now gives the commit phases); time writers spent throttled by writeback, which is the mechanism behind the 56x |
| A ~300 MB write burst every ~5 s on read-only runs is the page-cache flusher (`dirty_writeback_centisecs`), not the journal commit interval: a 30x change in `commit=` did nothing, the flusher interval moved the period. | four arms of two sysctls, read off device-write charts | writeback runs by reason (periodic, background, sync) with pages written per run; dirty and writeback page gauges |
| Eviction is episodic and large: one pass deleted 7.76 M files (461 GB) at about 4,400 unlinks/s on top of 2,300 inserts/s, after walking all 54.5 M. Two client-timeout bursts lined up with pass boundaries; causation not established. | server logs; client timeouts | unlink and inode-eviction rates, unlink latency, and the inode-table read storm the walk causes, on the same time axis as the request tail |
| Write amplification measured 5.1x in one campaign and 1.48x in another for the same nominal setup, unexplained. | device bytes over client bytes, per campaign | bytes at the VFS layer, pages written by writeback, blocks logged to the journal, and bytes at the device, so the ratio decomposes into data, metadata and journal terms |
| Two hosts with the same kernel-level work (syscall rates, context switches, CPU, TLB within 10%) differed 2x in block write p99, and socket-ready-to-read p99 was 4x worse with the runqueue idle. Hypothesis: slow file writes hold request threads. | comparing rezolus recordings; the hypothesis is untested | time request threads spend blocked in fsync and write, per cgroup, which is the hypothesis as a metric |
| Most tunables were nulls: commit interval, `data=writeback`, I/O scheduler, writeback throttling latency target, inode readahead, ZFS. | a run per lever | not a telemetry gap; recorded here because the allocator and writeback signals above are what would have said *why* they were nulls |

The filesystem the characterization settled on is ext4 with `bigalloc`
(64 KiB clusters, 14% capacity cost), with tuned XFS as the alternative that
was behind on p99 in every matched pair. Both are in scope below.

## Capabilities

Each capability names the hook, the metric, the finding it serves, the
event-rate shape that decides its cost, and what it depends on. The kernel
facts are checked against the vendored aarch64 `vmlinux.h`, which carries the
`trace_event_raw_*` struct for every tracepoint named here except XFS's
(XFS is a module on that kernel too; the module-BTF path #1321 built covers
it).

**C1. Writeback throttling and flusher activity** — a new `writeback` sampler
on the `mm` writeback tracepoints, filesystem-agnostic.

- `balance_dirty_pages`: fires when a task that has dirtied pages is
  considered for throttling; carries `pause` (ms the task will sleep),
  `paused` (cumulative), `dirty`, `bdi_dirty`, `dirty_ratelimit`,
  `task_ratelimit` and `cgroup_ino`. Metrics: a histogram of non-zero
  `pause`, a count of throttle events, and a count of calls. This is the
  mechanism behind the 56x p99.99 without fsync, and it is what "dirty pages
  accumulate and flush in bursts that throttle writers" looks like as a
  number. Per-cgroup throttled time is available from `cgroup_ino` once the
  cgroup-slot infrastructure is taught inode-keyed lookup; host-wide first.
- `writeback_start` / `writeback_written` (`writeback_work_class`): one event
  per writeback work item with `nr_pages`, `reason` (background, periodic,
  sync, vmscan, foreign-flush, ...) and `sb_dev`. Metrics: runs and pages by
  reason. The 5 s burst becomes a `reason=periodic` series with its page
  count, and the null result for `commit=` is visible as that series not
  moving.
- Rate shape: `balance_dirty_pages` is called once per `ratelimit_pages`
  dirtied (tens of pages), so under a 12% write mix at 20 K requests/s of
  56 KiB objects it is on the order of a few thousand calls per second;
  writeback work items are tens per second. Probe 1 for this sampler is the
  measured call rate on a write-heavy fio run.
- Not this: `writeback_dirty_page`, `wbc_writepage`, `writeback_dirty_inode`
  are per page or per inode and stay out until measured.

**C2. Allocator effort and fragmentation** — `ext4_alloc`, phase 2 of the ext4
entry, with two additions.

- As specified there: `ext4_mballoc_alloc` (requested vs allocated length,
  groups scanned, criterion), `ext4_free_blocks`, inode allocate/free,
  `ext4_writepages_result`, trim.
- Added: a histogram of allocated extent length in blocks
  (`ext4_alloc_allocation_sizes` in the ext4 entry's group list already
  reserves it), and `ext4_mballoc_discard` / `ext4_discard_preallocations`
  counts, because preallocation discard churn is what a delete-heavy
  eviction pass does to the allocator.
- Why: the collapse is an allocator that scans more groups per allocation
  and returns shorter extents for longer; `2.1 extents per 56 KiB file` is
  `allocations / files created > 1`, which is two counters this sampler has.
  On `bigalloc` the same signals say whether the 64 KiB cluster is doing its
  job, and they are the only way to evaluate the untested `-C 8192` cluster
  size without another 8 h soak.
- Rate shape: one `ext4_mballoc_alloc` per extent allocation, so per insert
  on this workload and per writeback batch on a streaming one; tens of
  thousands per second at most. Counters and one histogram.

**C3. Metadata reads on the request path** — into `ext4_alloc` as a
"metadata reads" group, or its own small sampler if the cadence differs.

- `ext4_load_inode(sb, ino)`: fires in `__ext4_get_inode_loc` only when the
  inode-table block is not in the buffer cache and must be read, so its rate
  is exactly "synchronous 4 KiB metadata reads on the request path per
  second". This is the atime finding as one counter, and the `vfs_cache_pressure`
  fix as that counter going to zero.
- `ext4_read_block_bitmap_load` / `ext4_load_inode_bitmap` (`ext4__bitmap_load`
  class): bitmap block reads, the allocator's own cold-metadata cost.
- `ext4_other_inode_update_time` and `ext4_mark_inode_dirty`: the deferred
  timestamp writes `lazytime` batches, which under `strictatime,lazytime` is
  where the atime *write* cost lands. `ext4_mark_inode_dirty` fires per
  metadata change and is the hot one; measure first.
- Rate shape: `ext4_load_inode` fires at up to the request rate when the
  inode table is cold (20 K/s in the characterization) and near zero when
  warm; that swing is the signal, and the per-event cost is one counter
  increment.

**C4. Per-filesystem attribution** — phase 3 of the ext4 entry, promoted.
The cache device is not the root filesystem, and every capability above is a
per-device question. The `dev_t → slot` lookup and `PackedCounters`-shaped
banks are specified in that entry; the `filesystem` sampler's mount table
supplies the labels. Histograms per filesystem still wait on histogram
slots, so latency stays host-wide until then.

**C5. VFS operation latency and blocked time, per filesystem and per cgroup**
— a new `ext4_ops` sampler (name open).

- `ext4_sync_file_enter` → `_exit` paired by thread: fsync latency per
  filesystem, and its error count already in `ext4_journal`.
- `ext4_unlink_enter` → `_exit` paired by thread: unlink latency, the
  eviction pass as a latency distribution rather than a log line.
- `fexit` on `ext4_file_write_iter` and `ext4_rename2` (BTF-gated, module
  BTF on the kernels seen so far): write and rename latency and bytes
  written at the VFS layer. `ext4_rename2` matters because the write path's
  durability ends at a rename that is not itself fsynced.
- Per cgroup: the sum of time spent blocked in each of these, as
  `PackedCounters` by cgroup slot, which the syscall sampler already does
  for counts. This is the "slow file writes hold request threads"
  hypothesis as a metric, and it needs no per-thread state beyond the start
  timestamp.
- Depends on: a per-thread start array (the `MAX_PID` shape, 32 MB per
  paired hook, or one shared array with the hook id in the value) or
  `BPF_MAP_TYPE_TASK_STORAGE` once the kernel floor is 5.11. The ext4 entry
  recorded both options and deferred the choice; C5 forces it.
- Rate shape: fsync and unlink at request rate; write_iter at request rate;
  one map write and one read per operation. This is the most expensive
  sampler in the list and gets the bare-metal probe-cost bench before it is
  on by default.

**C6. Write amplification decomposition** — no new hooks; a dashboard and a
documented ratio. Bytes returned by `ext4_file_write_iter` (C5), pages from
`ext4_writepages_result` (C2), `ext4_journal_commit_blocks{kind="logged"}`
(shipped), and `blockio_bytes{op="write"}` (shipped) put data, metadata plus
journal, and device bytes on one axis. The 5.1x versus 1.48x discrepancy
becomes a question of which term moved, answerable from the recording.

**C7. Page-cache hit ratio** — `mm_filemap_add_to_page_cache` and
`mm_filemap_delete_from_page_cache` count misses that add pages and
evictions; hits have no tracepoint and would need `fentry` on
`filemap_get_folio` at every read. Rate is the read rate. Deferred until C1
through C5 exist; the read path was not where this characterization lost its
SLO.

**C8. XFS parity** — an `xfs_log` sampler on XFS's log-grant, AIL-push and
CIL tracepoints, and an `xfs_alloc` on its allocator, so the tuned-XFS
alternative is measurable on the same axes as `bigalloc`. XFS's tracepoints
are in the `xfs` module; the module-BTF twin selection from #1321 applies
unchanged. Separate design, after C1–C5.

**C9. Adjuncts that are not eBPF.**

- `memory_meminfo` does not expose `Dirty`, `Writeback`, `Dirty` thresholds
  or `Buffers`-adjacent writeback state; the file is already read every
  refresh, so these are new fields on an existing parse.
- Slab gauges for `ext4_inode_cache`, `dentry` and `buffer_head` from
  `/proc/slabinfo` at the `filesystem` sampler's 60 s off-cycle cadence.
  "Does the ~46 GB inode cache stay resident" is this gauge; nothing in the
  agent answers it today. Principle 15 exception, measured like
  `filesystem`'s sweep.
- `/sys/fs/ext4/<dev>/errors_count` and `lifetime_write_kbytes`, already in
  the backlog from the ext4 entry.

## Plan

Ordered by which finding each closes, event-rate risk, and what it depends
on. Every sampler carries the ext4 entry's gates: measured refresh
microseconds, a rate probe on a representative workload before the hot hooks
are attached, and the bare-metal probe-cost bench for anything at request
rate.

1. **`memory_meminfo` dirty/writeback fields** (C9). Parse-only; lands alone.
   *Done in #1325*, with 28 more of the file's lines while there.
2. **`writeback` sampler** (C1): `balance_dirty_pages` pause histogram and
   counts, writeback runs and pages by reason. Rate probe first on a
   write-heavy fio run. Filesystem-agnostic, so it serves the XFS arm too.
   *Done as `memory_writeback`; measured below.*
3. **`ext4_alloc` with metadata reads** (C2, C3). The allocator signals and
   `ext4_load_inode` together are what the two largest findings needed.
   *Done; measured in the ext4 entry's "Results — phase 2": every counter
   exact against tracefs, refresh 151–301 µs. One correction to C3:
   `ext4_load_inode` fires once per inode-table read, and a read is an
   `inode_readahead_blks` window of 32 blocks, so it counts reads, not
   inodes. The rate is still the synchronous metadata cost; it is just
   smaller than one-per-stat on any access pattern readahead can help.*
4. **Slab gauges** (C9) beside `filesystem`'s sweep, so C3's inode-read rate
   has its cause on the same dashboard. *Done as `memory_slabinfo`, its own
   sampler rather than a field on `filesystem`'s sweep: the file is
   host-wide, not per mount. Measured below.*
5. **Per-filesystem counters** (C4), the lookup-map decision measured on the
   `ext4_alloc` bench. *Done; the ext4 entry's "Results — phase 3" has the
   design as built and the measured lookup cost.*
6. **`ext4_ops`** (C5) with the per-thread start map decision, and the
   amplification dashboard (C6) once its four terms exist. *Done; see
   Results — C5 and C6. The start map is task local storage, decided by
   the kernel floor the sampler already has.*
7. **XFS** (C8), then page cache (C7). *Designed and probed in its own
   entry, `2026-09-29-xfs-samplers.md`: per-mount sysfs stats first, BPF for
   the latencies the stats file cannot carry; the page cache is deferred
   there. `xfs_stats` shipped in #1351 and `xfs_log` (opt-in) in #1352;
   the page cache (C7) stays open.*

## Results — C1, the `memory_writeback` sampler

Built on the hv01 `debian-13-ci` guest (Debian 13, `6.12.63+deb13-amd64`,
`CONFIG_HZ=250`, 56 vCPU, root on ext4 over virtio); final run systemslab
`01a0ea5e-86d2-718c-92e3-1adb6bf25a7f`. Three hooks, all `tp_btf`:
`balance_dirty_pages`, `writeback_start`, `writeback_pages_written`.

**Three things the kernel source corrected before the numbers meant
anything.** (1) The `pause` a `tp_btf`/`raw_tp` program receives is the raw
argument, in **jiffies**; the millisecond value exists only in the
tracepoint's formatted record (`__entry->pause = pause * 1000 / HZ`). The
first build multiplied by a millisecond. The jiffy-length helper the ext4
entry built for jbd2 moved to `bpf/mod.rs` as `jiffy_ns()` and both samplers
use it. (2) `balance_dirty_pages()` returns before its tracepoint while dirty
pages are under the free-run ceiling (midway between the background and hard
limits), so the "checks" counter is evaluations of tasks already in the
throttle zone, not one per ratelimit of pages dirtied; it reads zero on a
host whose dirty pages never approach the limit, which is the correct
reading. (3) `writeback_start` fires once per pass of `wb_writeback`'s loop,
not per work item, so `writeback_runs` counts passes.

**The tracepoint's argument list is a hazard, and the sampler guards it.**
`balance_dirty_pages` has 12 arguments on every kernel seen so far (both
vendored headers, the 6.12 guest); a later rework passes the
`dirty_throttle_control` instead of its fields and has 8. A positional read
on the wrong arity is silently wrong, so the sampler carries one program per
arity and selects on `kernel_btf_tracepoint_arg_count`, a new helper that
walks `btf_trace_<name>` → pointer → function prototype in vmlinux or module
BTF; neither arity, or no BTF, disables the hook and reports degraded.

**Verification, sampler against tracefs over the same window** (the same
three tracepoints read through `trace_pipe`, plus `/proc/vmstat`), with
`dirty_bytes` = 1 GiB and `dirty_background_bytes` = 256 MiB so 16 GiB of
buffered 1 MiB writes had to throttle:

| metric | sampler | tracefs / vmstat |
|---|---|---|
| `writeback_throttle_checks` | 4,710 | 4,710 events |
| `writeback_throttle_events` | 1,847 | 1,847 with `pause > 0` |
| `writeback_throttled_time` | 207.352 s | the pause histogram sums to the same |
| `writeback_throttle_latency` | buckets 167/171/175/177/… = 115/29/105/126/… | pause 8/12/16/20 ms = 115/29/105/126/… |
| `writeback_runs` background / periodic / sync / foreign_flush | 235 / 8 / 3 / 6 | identical |
| `writeback_pages_written` | 2,625,503 | tracefs 2,625,503; `nr_written` +2,625,509 |

An earlier run (`01a0ea27-dc82-71e2-f2bb-81d068b8fd9e`, cancelled for a
payload fault, results in its console) agreed the same way: 9,033 / 2,753 /
191.520 s / 2,659 background passes, all exact. fio under the throttle went
from 567 µs mean and 717 µs p99 per 1 MiB write to 21.8 ms and 55.8 ms,
which is the throttle doing what the histogram says: 884 of 1,847 sleeps
were the 200 ms maximum.

**`writeback_pages_written` is the flusher's own accounting, not the total.**
In the first run, with the default 20% dirty ratio on a 220 GiB guest, 16
GiB of writes never reached the throttle and sat dirty until the final
`sync`; `nr_written` grew by 4.20 M pages and the tracepoint reported
238,774. Pages written by an integrity sync did not appear in it, while
flusher-driven writeback matched `nr_written` to six pages in the runs
above. So `memory_vmstat` now also exports `memory_pages_written` and
`memory_pages_dirtied` (`nr_written`, `nr_dirtied`), the complete counts,
and the dashboard shows both.

**Refresh cost**: 126–261 µs per refresh, median 167 µs, on the 56-vCPU
guest (one counter map of `MAX_CPUS` banks plus one histogram).
**Probe cost**: not benched separately. `balance_dirty_pages` fired 4,710
times in 40 s under a throttle chosen to be as busy as a 1 GiB limit
allows, and `writeback_start` 252 times; at those rates the per-event cost
does not register, and the hooks are two counter increments and a
histogram increment. The rate to watch is `balance_dirty_pages` on a host
that lives near its dirty limit: the kernel evaluates once per
`ratelimit_pages` dirtied per task, so it is bounded by write throughput
divided by tens of pages.

**Test flake, recorded.** `stream_tests::a_dropped_stream_is_reconnected_and_both_ends_are_reported`
timed out on its 20 s deadline in three of four full-workspace test runs on
this 56-vCPU guest today and passes in CI; not touched by this work.

## Results — C9, the `memory_slabinfo` sampler

Same guest as C1 (`6.12.63+deb13-amd64`, 56 vCPU, root on ext4); systemslab
`01a0ebb3-3a29-71a0-1775-9d5b6fe01608`. Eight caches followed: `dentry`,
`inode_cache`, `ext4_inode_cache`, `ext4_extent_status`, `jbd2_journal_head`,
`buffer_head`, `xfs_inode`, `radix_tree_node`. Two gauges per cache,
`memory_slab_cache_objects{cache,state=active|total}` and
`memory_slab_cache_bytes{cache}` (slabs × pages per slab × page size).

**Values match the file.** After a `cargo build` had warmed the caches, the
sampler's gauges equalled the `/proc/slabinfo` line read beside the scrape:
`dentry` 66,562 active of 66,780, 1,590 slabs × 2 pages = 13,025,280 bytes;
`ext4_inode_cache` 27,076 of 27,076; `buffer_head` 163,801 of 163,995;
`radix_tree_node` 56,597 of 56,924.

**Six of the eight caches were present.** `xfs_inode` is absent because XFS is
not loaded, which the design expected. `ext4_extent_status` is absent for a
reason the design did not: SLUB merges a cache that has no constructor into a
same-sized pool, and the file lists the pool under one name, so the
extent-status cache (40 bytes, `SLAB_RECLAIM_ACCOUNT`, no constructor) is
invisible on this kernel. The module doc's claim that the followed caches
"are not merged in practice" was wrong and is corrected. The inode and
dentry caches have constructors and cannot merge; `jbd2_journal_head` has
none and happened not to merge here. `slab_nomerge` on the kernel command
line is the operator's fix; the sampler reports absent, not 0.

**Sweep cost**, 196-line file: read 366–409 µs, parse 51–63 µs, four sweeps
at a 3 s test interval. **Refresh cost** on the scrape path: 0 µs at the
median, 86 µs maximum across 12 refreshes, the dispatch of a sweep. The
default interval is 60 s, so the ~450 µs sweep is 7.5 µs per second of
blocking-pool time.

## Results — C5 and C6, the `ext4_ops` sampler

Same guest (`6.12.63+deb13-amd64`, `CONFIG_EXT4_FS=m` with module BTF,
56 vCPU); systemslab `01a0ebeb-6ca9-71cd-5439-b7eef2574e83`. Eight programs,
all attached: the fsync and unlink tracepoint pairs as `tp_btf`, write and
rename as `fentry`/`fexit` on the module's functions, the rename pair after
`kernel_btf_func_arg_count("ext4_rename2")` confirmed the six arguments the
program reads (the run carried a five-argument twin for pre-5.12 kernels;
review pointed out no such kernel can load the skeleton, task storage being
5.12 for tracing programs, and the twin was removed).

**Counts are exact.** A driver in its own cgroup wrote 200 files of 8 KiB,
fsynced each, renamed 100 and unlinked all 200. Its cgroup's series read
fsync 200, write 200, rename 100, unlink 200; the host-wide fsync histogram
held 600 events and tracefs counted 600 `ext4_sync_file_enter` over the same
window (the other 400 were the guest's own services), unlink 200 both ways.
The per-filesystem rows of this run were hidden by the member-set defect the
per-filesystem review found (fixed before either landed); the same reader
serves both samplers and is verified in the phase 3 results of the ext4
entry.

**Probe cost, the number this sampler was gated on.** The phase 1 bench
(`null_blk`, 4 KiB `randwrite` with `fsync=1`, 8 fio jobs pinned to CPUs 8–15,
agent on 0–7, `perf stat` on the fio CPUs, 3 reps × 20 s), so one fio
operation is one write plus one fsync and crosses four probes:

| arm | IOPS | instructions / op | cycles / op | task-clock ns / op |
|---|---|---|---|---|
| no agent | 444,684 ± 10,256 | 33,096 ± 162 | 68,628 ± 998 | 18,345 ± 454 |
| agent, no samplers | 452,201 ± 3,050 | 33,008 ± 146 | 68,038 ± 369 | 18,035 ± 98 |
| agent + `ext4_ops` | 402,609 ± 3,357 | 37,076 ± 178 | 76,361 ± 263 | 20,292 ± 166 |

`ext4_ops` costs **+4,070 instructions, +8,320 cycles, +2.26 µs per
write+fsync pair** against the idle agent: 12% at this saturating 450 K
ops/s, roughly 1,000 instructions per probe. For comparison phase 1's two
`ext4_sync_file` probes cost +225 instructions together. The difference is
what each probe does: two task-local-storage lookups per operation
(`bpf_task_storage_get` at begin and end), a hash lookup for the filesystem
slot, a histogram increment, four per-filesystem counter adds and the
per-cgroup path (`handle_new_cgroup`'s serial-number check plus two atomic
adds into cache lines every CPU shares). Which of these dominates is not
measured; `bpftool prog profile` on each program is the next step, and the
per-cgroup atomics are the first suspect for the cycle count exceeding the
instruction count's share.

**Verdict.** NO-GO for on-by-default, GO as an opt-in: at the
characterization's 20 K fsync/s a 2.3 µs pair cost is 4.5% of one core spread
over the request threads, which is affordable for the finding it serves
(request threads held inside the filesystem, per cgroup) but not a fleetwide
always-on cost. It is in `OPT_IN_SAMPLERS` (`src/agent/config/mod.rs`), so
`[defaults] enabled = true` never turns it on; `config/agent.toml` ships it
`enabled = false`. Two things review caught after the run: an asynchronous
direct write returns `-EIOCBQUEUED`, which the program now excludes from the
error count (its bytes are unknowable at submission and are documented as
missing from `ext4_write_bytes`), and an operation whose function BTF lacks
reads 0 rather than absent, which the docs now say. Refresh cost
315–488 µs on the 56-vCPU guest (one 16-wide slotted counter map, four
histograms, eight cgroup maps).

**The amplification dashboard (C6)** needs no measurement: it is four
existing rates on one axis (`ext4_write_bytes`, `ext4_writepages_pages`
written × 4 KiB, `ext4_journal_commit_blocks{kind="logged"}` × 4 KiB,
`blockio_bytes{op="write"}`), each drawn when the recording has it. The
4 KiB conversion is the default page and block size and is stated on the
plot.

## Deferred / reopen

- **`ext4_ops` probe cost** — Open. +4,070 instructions per write+fsync pair
  is 18x phase 1's two probes; profile per program before any attempt to cut
  it, the per-cgroup atomics and the two task-storage lookups first. Reopen
  the on-by-default question when a probe costs under ~300 instructions.

- **Merged slab caches** — By design. A cache SLUB merges is absent, not
  approximated from the pool it joined; `ext4_extent_status` is merged on
  Debian 13's 6.12. Reopen if that cache's residency becomes the question:
  the alternative is `/sys/kernel/slab/<cache>` which resolves aliases, at
  the cost of one directory walk per cache per sweep.

- **Per-cgroup writeback throttling** — Roadmap. `balance_dirty_pages`
  carries `cgroup_ino`, not the css id the cgroup slot machinery keys on; an
  inode-keyed lookup is a `bpf/cgroup.h` change. Host-wide first.
- **Per-thread start state for paired hooks** — Decided by C5: task local
  storage (`BPF_MAP_TYPE_TASK_STORAGE`), one slot per operation in one value.
  Tracing programs can use it from 5.12, one release above the module-BTF
  `fentry` (5.11) `ext4_ops` needs anyway, so the floor is 5.12; a write to
  an O_SYNC file nests fsync inside it on one thread, which a single shared
  slot would lose and separate 32 MB arrays would pay 128 MB for.
- **Page-cache hits** — Idea. Needs `fentry` at read rate; C7.
- **Free-space fragmentation as a gauge** — By design, not eBPF. The state
  `e2freefrag` reports is the on-disk bitmap; the allocator signals in C2
  are its rate-of-change and the honest always-on proxy. Reopen if a cheap
  periodic read of the group descriptors' free-block counts proves useful
  as a 60 s gauge.
