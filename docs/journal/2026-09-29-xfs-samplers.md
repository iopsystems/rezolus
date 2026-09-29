# XFS telemetry: per-mount stats first, BPF for what stats cannot say

- **Opened:** 2026-09-29
- **Status:** **Both steps shipped. Step 1: `xfs_stats` (#1351), exact
  against sysfs and `/proc/fs/xfs/stat` in the VM, see *Results — step 1*.
  Step 2: `xfs_log` (#1352, opt-in), force counts exact against the stats
  file, 1.3 µs per force measured with `kernel.bpf_stats_enabled`, see
  *Results — step 2*; its log-space path could not be provoked on the test
  rig and is verified only at zero.** Step 7 (C8) of
  `2026-09-28-filesystem-telemetry-gaps.md`. Two probes on the hv01 Debian 13
  guest (`6.12.63+deb13-amd64`, `CONFIG_XFS_FS=m` with module BTF) supplied the
  tracepoint inventory, the `tp_btf` prototypes, the module struct fields and
  the event rates under an fsync workload; they are recorded below. The
  design departs from the ext4 entry's shape in one way: XFS keeps
  per-mount counters in sysfs for nearly everything its tracepoints count, so
  the first sampler reads those and BPF is reserved for latencies and
  per-cgroup attribution.
- **Driver:** the characterization behind the gaps entry compared an ext4 cache
  against a tuned-XFS alternative and could measure neither from inside the
  filesystem. ext4 now has `ext4_journal`, `ext4_alloc` and `ext4_ops`; an
  XFS host has `blockio`, `syscall_latency`'s `sync` class and `filesystem`
  occupancy, and nothing between the syscall and the device: whether
  transactions are waiting for log space, how often the log is forced, whether
  the AIL pusher is keeping up, and whether the inode cache is being missed.
- **Owner:** Brian Martin

## What the kernel exposes

**Tracepoints.** `xfs.ko` on the probe kernel registers **623** tracepoints
(`/sys/kernel/tracing/events/xfs/`), and its module BTF
(`/sys/kernel/btf/xfs`, 382 KB) carries **606** `btf_trace_xfs_*` typedefs,
so `tp_btf` programs attach against module BTF exactly as `ext4_journal`'s do
(the vendored `src/agent/bpf/{x86_64,aarch64}/vmlinux.h` carry none of them;
XFS is a module on both source kernels). By family: 42 `log_*`, 11 grant, 8
`ail_*`, 5 CIL, 43 `alloc_*`, 50 `buf_*`, 31 `inode_*`, 5 `iget*`, 6
`iomap_*`, 15 `file_*`. Systemslab `01a0ec3f-a211-7100-b6c9-deee55083a37`.

**Prototypes and the structs they hand over** (systemslab
`01a0ec4d-a5b2-71c3-7417-1c82f1e76053`, module BTF dumped with `bpftool`):

| tracepoint | `TP_PROTO` | device path |
|---|---|---|
| `xfs_log_reserve`, `_reserve_exit`, `_regrant`, `xfs_log_grant_sleep`, `_grant_wake`, `xfs_log_cil_wait` | `(struct xlog *log, struct xlog_ticket *tic)` | `log->l_mp->m_super->s_dev` |
| `xfs_log_force` | `(struct xfs_mount *mp, xfs_lsn_t lsn, unsigned long caller_ip)` | `mp->m_super->s_dev` |
| `xfs_ail_push`, `_pinned`, `_flushing`, `_locked` | `(struct xfs_log_item *lip)` | `lip->li_log->l_mp->m_super->s_dev` |
| `xfs_file_fsync`, `xfs_iget_hit`, `xfs_iget_miss` | `(struct xfs_inode *ip)` | `ip->i_mount->m_super->s_dev` |
| `xfs_alloc_exact_done`, `_near_first`, `_size_done`, `_vextent_finish`, `_vextent_allfailed`, `_vextent_loopfailed` | `(struct xfs_alloc_arg *args)` | `args->mp->m_super->s_dev`; `args->len`, `minlen`, `maxlen`, `wasdel`, `wasfromfl` |
| `xfs_free_extent` | `(struct xfs_mount *mp, xfs_agnumber_t agno, xfs_agblock_t agbno, xfs_extlen_t len, enum xfs_ag_resv_type, int haveleft, int haveright)` — 7 arguments on 6.12 | `mp->m_super->s_dev` |
| `xfs_inodegc_queue`, `_throttle` / `xfs_inodegc_worker` | `(struct xfs_mount *mp, void *caller_ip)` / `(mp, unsigned int shrinker_hits)` | `mp` |
| `xfs_file_buffered_write`, `_direct_write` | `(struct kiocb *iocb, struct iov_iter *iter)` | `iocb->ki_filp->f_inode->i_sb->s_dev`; bytes `iter->count` |
| `xfs_buf_read` | `(struct xfs_buf *bp, unsigned int flags, unsigned long caller_ip)` | `bp->b_mount->m_super->s_dev`; `b_length` in 512-byte blocks |
| `xfs_iomap_alloc`, `_found` | `(struct xfs_inode *ip, xfs_off_t, ssize_t, int whichfork, struct xfs_bmbt_irec *)` | `ip`; `br_blockcount` |

Struct fields confirmed in the module BTF: `xfs_mount.m_super`,
`xlog.l_mp`/`l_logsize`/`l_tail_space`/`l_reserve_head`/`l_write_head`
(each grant head is a single packed `atomic64_t grant`), `xlog_ticket.t_curr_res`/
`t_unit_res`/`t_ocnt`/`t_cnt`/`t_flags`, `xfs_log_item.li_log`/`li_type`/
`li_flags`/`li_lsn`, `xfs_inode.i_mount`/`i_ino`/`i_vnode`,
`xfs_alloc_arg.mp`/`pag`/`agno`/`minlen`/`maxlen`/`len`/`wasdel`/`wasfromfl`/
`resv`, `xfs_buf.b_length`/`b_flags`/`b_mount`/`b_target`.

**Version hazards, from the kernel history rather than the probe** (the
probe kernel is 6.12; a 5.8 floor spans several XFS reworks):

- `xfs_log_item.li_log` replaced `li_mountp` in 5.19. A CO-RE flavor
  declaring both, read through `bpf_core_field_exists`, covers both shapes;
  on the old shape the device is `li_mountp->m_super->s_dev`.
- `xfs_buf.b_mount` exists on every kernel in range (added in 5.3); before it
  the path was `b_target->bt_mount`, which is below the floor.
- `xfs_free_extent`'s first argument became `struct xfs_perag *` in 6.3
  and reverted to `(mp, agno, ...)` later; the probe kernel passes the
  seven-argument form. `kernel_btf_tracepoint_arg_count` selects, as
  `memory_writeback` does for `balance_dirty_pages`; the length is the
  fourth argument in the 7-form and the third in a 6-form.
- The grant heads changed from separate cycle/space words to one packed
  `atomic64_t grant` in 6.11, and `l_tail_space` arrived with them. Nothing
  here reads grant bytes: a "log space in use" gauge would be a different
  decode per kernel, and the sysfs stats below already carry the log
  sleep counts that the gauge would be used to explain.

**Event rates**, 20 s of 4 KiB `randwrite` with `fsync=1` from 8 fio jobs on a
loop-backed XFS (416 fsync/s, the loop device the ceiling), then 2,000
56 KiB files written and fsynced, caches dropped, every file `stat`ed:

| tracepoint | events | per fsync (10,331) |
|---|---|---|
| `xfs_log_reserve` | 26,693 | 2.6 |
| `xfs_log_force` | 20,669 | 2.0 |
| `xfs_file_fsync` | 10,331 | 1 |
| `xfs_file_buffered_write` | 10,331 | 1 |
| `xfs_iget_miss` | 4,016 | (2 per cold `stat`) |
| `xfs_alloc_near_first` | 2,128 | (1 per file) |
| `xfs_ail_push` | 322 | |
| `xfs_alloc_size_done` | 132 | |
| `xfs_ail_flushing` | 61 | |
| `xfs_log_grant_sleep`, `xfs_log_cil_wait`, `xfs_alloc_exact_done` | 0 | (log never filled) |

So an fsync on XFS crosses **about 5.6 of these hooks** where ext4's crosses
two (`ext4_sync_file_enter`/`_exit`, which cost +225 instructions together on
the phase 1 bench). Counting every one of them in BPF at request rate would
cost roughly three times `ext4_journal`'s probe budget for numbers the kernel
already keeps, which is the decision below.

**Counters XFS already keeps.** `/sys/fs/xfs/<block_device>/stats/stats` is
per mount (and `/proc/fs/xfs/stat` the host sum, identical line for line on a
one-mount host), one line per family with space-separated counters,
maintained at event rate by the same code paths the tracepoints instrument.
Read on the probe kernel (systemslab `01a0ec51-fb15-7100-e653-58d464d293bf`)
after 1,349 write+fsync pairs, 500 small files and a cold `stat` of each; the
field names are the kernel's `xfsstats` order (`fs/xfs/xfs_stats.h`):

| line | fields | read |
|---|---|---|
| `extent_alloc` | extents allocated, blocks allocated, extents freed, blocks freed | `531 39844 0 0` |
| `ig` | attempts, found, recycled, missed, duplicates, reclaims, attribute changes | `1011 0 0 1011 0 504 505` |
| `log` | writes, blocks written, `noiclogs`, forces, `force_sleep` | `1326 31174 0 1850 3186` |
| `push_ail` | `try_logspace`, `sleep_logspace`, pushes, success, pushbuf, pinned, locked, flushing, restarts, flush | `7579 0 174 66 0 0 0 14 0 0` |
| `trans` | sync, async, empty | `0 7579 0` |
| `xstrat` / `rw` | extent conversions, quick and split / write calls, read calls | `0 0` / `1849 0` |
| `dir` | lookups, creates, removes, getdents | `1008 504 0 2` |
| `buf` | get, create, get_locked, get_locked_waited, busy_locked, miss_locked, page_retries, page_found, get_read | `22128 107 22024 18336 360 104 0 90 49` |
| `xpc` | `xstrat` bytes, bytes written, bytes read | `0 34197504 0` |
| also | `abt`, `blk_map` (7), `bmbt`, `attr`, `icluster` (3), `vnodes` (8), the nine 15-field btree lines, `qm`, `defer_relog`, `debug` | |

So the log-space sleeps the design cares most about are already counted:
`push_ail`'s first two fields are the transactions that asked for log space
and the ones that slept for it (7,579 asked, none slept on a 2 GiB loop image
with the default log). `log` carries forces (1,850, 1.4 per fsync) and
`force_sleep` (3,186 callers that found a force already in progress and
waited). What no field carries is *how long* anyone waited: the sleep and
force latencies, and who (which cgroup) waited. That, and nothing else, is
what BPF is for on XFS.

One read of the per-mount file costs **159 µs** on the probe guest (1,000
reads averaged), which is a full refresh budget on its own, so the read
belongs off the scrape path on a throttled sweep, as `filesystem`'s and
`memory_slabinfo`'s do, at a cadence fit for counters (1 s) rather than
occupancy (60 s).

## Decisions

**Two samplers, counters from sysfs and latencies from BPF, in that order.**

1. **`xfs_stats`** reads `/sys/fs/xfs/<block_device>/stats/stats` for every
   XFS mount, off the scrape cycle on the `filesystem` sampler's cadence
   pattern (a principle 15 exception with the same justification as
   `filesystem_written_bytes`: the kernel maintains the counters, the read
   is one small sysfs file per mount, and it works on every kernel that has
   XFS). It publishes the families above as per-mount counters with the
   same labels the `filesystem` sampler and the ext4 samplers use, through
   the shared slot registry (`bpf/filesystems.rs`, its `FSTYPES` gaining
   `xfs`), so the XFS series and the ext4 series of one host read alike. No
   probes, no request-path cost, exact by construction.
2. **`xfs_log`**, BPF, for what the stats file cannot say: the distribution
   of time a transaction waited for log space (`xfs_log_grant_sleep` →
   `_grant_wake`, paired per task in task local storage as `ext4_ops` pairs
   its operations, so the floor is 5.12), the distribution of time an
   `xfs_log_force` took (the fsync's device wait: `fentry`/`fexit` on the
   module's `xfs_log_force`, whose symbol set is confirmed at build against
   module BTF as `ext4_ops` confirms `ext4_rename2`), CIL waits, and
   per-cgroup time blocked in each.
   Counts stay in `xfs_stats`; this sampler carries only what needs a
   timestamp pair. Off by default until benched, as `ext4_ops` is: its
   hooks fire 2–3 times per fsync.
3. **No `xfs_alloc` BPF sampler.** `extent_alloc`, `ig` and `buf` in the
   stats file cover allocation counts, inode-cache misses and metadata reads.
   What BPF would add is the allocation *length* distribution and the
   requested-versus-returned split (`xfs_alloc_arg.maxlen` against `len`,
   the analogue of `ext4_allocation_blocks{kind}`); that is one hook at
   file-creation rate and can join `xfs_log` later if the fragmentation
   question comes up on XFS as it did on ext4.

**Per-filesystem from the start.** The registry and reader built in the ext4
entry's phase 3 exist; `xfs_stats` uses the registry for slots and labels
and sets its `CounterGroup`s directly (no BPF map), and `xfs_log` uses
`FilesystemCounters` for its per-mount counts of sleeps and forces. The
`mount="other"` slot does not apply to `xfs_stats`, which reads by mount;
for `xfs_log` it applies as for ext4.

**Host-wide histograms**, as everywhere else, until histogram groups have
slots.

## Go/no-go — probes before build

1. **Tracepoint and BTF inventory** — *done above*: 623 tracepoints, 606 in
   module BTF, prototypes and fields as listed.
2. **Event rates** — *done above*: 5.6 hooks per fsync for the log family;
   the decision to leave counts to sysfs follows from it.
3. **`xfs_stats` sweep cost** — the read is measured at 159 µs per mount;
   the parse and publish are measured with the sampler and recorded in
   *Results*. GO as long as the sweep runs off the scrape path; a 1 s sweep
   over a handful of mounts is under a millisecond a second of blocking-pool
   time.
4. **`xfs_log` probe cost** — the phase 1 `null_blk` bench with `mkfs.xfs`
   instead of `mkfs.ext4`: instructions per fsync with the sampler attached
   versus the idle agent. GO for opt-in if the sleep/wake pair and the force
   pair together cost under what `ext4_ops` pays per fsync (about 2,000
   instructions for two probe pairs); NO-GO for on-by-default regardless,
   as for `ext4_ops`.
5. **Validation oracle** — `/proc/fs/xfs/stat` for `xfs_stats` (the host sum
   of the per-mount files, so the sum over mounts must match it exactly);
   the `push_ail` sleep field and the `log` force field for `xfs_log`'s
   counts of the same events, and tracefs for the pairing.

## Plan

1. **`xfs_stats`** sampler with the log, push_ail, ig, extent_alloc, buf,
   rw, xstrat and xpc families as per-mount counters; dashboard "XFS" group;
   docs. Verified against `/proc/fs/xfs/stat` in the VM. *Done in #1351,
   without the `xstrat` line; see Results — step 1.*
2. **`xfs_log`** with grant-sleep and force latency, CIL waits, per-cgroup
   blocked time; benched on `null_blk` XFS before it is offered even as an
   opt-in. *Done in #1352; CIL waits are a count, not a latency. See
   Results — step 2.*
3. Reopen `xfs_alloc` if XFS free-space fragmentation becomes a question.

## Results — step 1: `xfs_stats`

Same guest as the probes (`6.12.63+deb13-amd64`, `CONFIG_XFS_FS=m`, 56
vCPU, root on ext4), two loop-backed XFS filesystems: 4 GiB on `/mnt/xfs`
(`loop0`) and 2 GiB on `/mnt/xfs2` (`loop1`). systemslab
`01a0ec70-45d6-714e-e7da-d8b16b9a134e` on the PR's final code
(`d9c9b54b`); an earlier run `01a0ec65-d0f0-71b8-21af-9369b13007dc` on the
first commit is cited where it differs. Shipped in #1351:
`src/agent/samplers/xfs/linux/stats/`, `crates/dashboard/src/dashboard/xfs.rs`,
the `xfs` entry in `bpf/filesystems.rs`'s `FSTYPES`.

**Built as specified, minus one line.** 41 per-mount counters from `log`,
`push_ail`, `trans`, `ig`, `extent_alloc`, `dir`, `rw`, `xpc` and `buf`. The
plan also named `xstrat` (delayed-allocation conversions, quick and split);
it is not published, nor are `abt`, `bmbt`, `attr`, `vnodes`, `icluster`,
`qm` or `defer_relog`. None of them answers a question the gaps entry asked;
adding a line is a `FIELDS` row and a metric declaration. Every field but
the byte counts is `uint32_t` in the kernel (`fs/xfs/xfs_stats.h`), so a busy
mount wraps one eventually and the docs say so (`xfs_log_blocks_written`
after 2 TiB of log writes); a wrap reads as a counter reset.

**Exact against the file and against `/proc/fs/xfs/stat`.** Workload: 15 s
of 4 KiB random writes with an fsync each, 8 fio jobs, on `/mnt/xfs` (6,734
writes and fsyncs, 449 IOPS on the loop device); 1,000 files of 56 KiB with
an fsync at the end on `/mnt/xfs2`, then `drop_caches` and a `stat` of every
file. The compare waited until two reads of each mount's sysfs file 2.5 s
apart were identical (three tries: the log worker keeps the counters moving
for seconds after the workload ends) and then held the agent's snapshot
against the file:

| | result |
|---|---|
| series compared against the mount's own `stats/stats` | 82 over 2 mounts, 0 mismatches |
| sum over mounts against `/proc/fs/xfs/stat` | 0 of 41 fields differ |
| fields the sampler names that the 6.12 file carries | 41 of 41 |
| labels | `mount`, `fstype="xfs"`, `block_device` (`loop0`, `loop1`), `devnum` (`7:0`, `7:1`) |

Per mount, the values are the workload's:

| series | `/mnt/xfs` (fsync) | `/mnt/xfs2` (small files) |
|---|---|---|
| `xfs_file_calls{op="write"}` | 6,734 | 1,000 |
| `xfs_log_forces` | 6,736 | 1,002 |
| `xfs_log_writes` | 3,543 | 1,012 |
| `xfs_directory_ops{op="create"}` | 8 | 1,000 |
| `xfs_directory_ops{op="lookup"}` | 8 | 2,000 |
| `xfs_inode_cache_lookups{outcome="missed"}` | 11 | 2,003 |
| `xfs_extents{op="allocated"}` | 72 | 1,026 |
| `xfs_log_space_sleeps` | 0 | 0 |

fio's 6,734 writes are the 6,734 write calls; the forces are one per fsync
plus two. The 2,003 inode-cache misses on the small-file mount are the
`stat` of every file after `drop_caches`, each inode read back from disk,
which is the cold-metadata cost the dashboard's Inode Cache group is for.
The first run (same workload shape) also showed what `xs_log_force_sleep`
counts: 1,001 sleeps for 1,002 forces with fio's single small-file job, and
10,546 for 6,756 forces with the 8 fsync jobs. A synchronous force sleeps
once for its own log write, and `xlog_force_lsn` sleeps a second time when
the previous in-core log buffer is still being written, so sleeps per force
above one is fsyncs waiting on each other's commits; the dashboard and
`docs/metrics.md` describe the counter that way, replacing an earlier
"queued behind a force in progress" that was wrong for the single-job case.

**Population.** Adversarial review found the sweep declaring the shared
registry's bound as the group's member bound: on a host with an ext4 root
and no XFS, that is `bound() == 2` and the sampler would have emitted an
`xfs_stats` table of 82 all-null columns on every tick, forever. The first
run confirmed the shape (82 unlabeled null series beside the two mounts'
82 real ones). The bound is now one past the highest slot the sweep gave
values, 0 when there is none, so an XFS-less host has no XFS table (unit
test). On this mixed host slot 0 and the root's ext4 slot sit below the XFS
slots and still read as 82 null columns, the trade the ext4 samplers'
vacant slots make (`2026-09-28-ext4-sampler.md`, Results — phase 3). Review
also had a stats line with an unparseable token shift every later field
onto the wrong counter; such a line now reads absent.

**Unmount.** `/mnt/xfs2` was unmounted with the agent running. The sysfs
directory went first, so the sweep logged a failed read once a second for
6 s until the registry's 10 s rescan (generation 2) freed the slot, cleared
its identity and unset its 41 counters; the next snapshot had `/mnt/xfs`
alone. The snapshot builder skipped the group for the one tick on which the
schema shrank mid-walk (its `SkeletonCache` arity check, a debug line), the
same one-tick gap any population change costs.

**Cost.** The sweep is `spawn_blocking`, at most once per `interval` (1 s
default), and the scrape path pays a time check:

| | n | min | p50 | max |
|---|---|---|---|---|
| sweep, 2 mounts (`reads` p50 371 µs) | 34 | 321 µs | 394 µs | 624 µs |
| sweep, 1 mount (`reads` p50 222 µs) | 14 | 173 µs | 237 µs | 294 µs |
| `sampling latency`, the scrape path | 48 | 17 µs | 19 µs | 83 µs |

About 190–220 µs per file here against the probe's 159 µs: the probe timed
the read alone, the sampler's figure includes open, close and the string.
Parse and publish are the difference between the sweep and its reads, about
20 µs for two mounts. GO gate 3 holds: a 1 s sweep over a handful of mounts
is under a millisecond a second of blocking-pool time and nothing on the
scrape path.

**One wide window.** One of the compare snapshots carried a 1.01 s
acquisition window on the group, against 218 µs on the run's final
snapshot. The sweep that scrape's own `refresh()` dispatched finished
between the snapshot builder's window pre-pass and the group's emission,
and the builder widens to the union of the two windows
(`resolve_walk_window`, `src/agent/exposition/http/snapshot.rs`), since
some of the values it read may belong to the newer sweep. The band is
honest but a whole interval wide, and any sweep dispatched from `refresh()`
at the scrape's cadence (`memory_slabinfo`, `filesystem`) can hit it; see
Deferred.

## Results — step 2: `xfs_log`

Same guest. Three systemslab runs on the PR's code (`e469853f`):
`01a0ec94-bbd1-7186-8ed1-cdb6fcf6d6e8` (two loop-backed mounts, fio
workloads), `01a0eca5-a64e-712d-fb57-4c7c7bfb8bfa` (a C driver of
create, 4 KiB write, fsync, unlink, and the first bench),
`01a0ecb3-c900-7160-df07-c50ebbbb098b` (a dm-delay device and the bench
with `kernel.bpf_stats_enabled=1`). Shipped in #1352:
`src/agent/samplers/xfs/linux/log/`, the Blocked Time group in
`crates/dashboard/src/dashboard/xfs.rs`, an XFS row in the cgroups view.

**Built as specified, with one narrowing.** The log-space wait is the
`xfs_log_grant_sleep`/`_wake` pair on one thread; the force is
`fentry`/`fexit` on `xfs_log_force` and on `xfs_log_force_seq`
(`xfs_log_force_lsn` before 5.13, chosen from BTF at init); starts are in
task local storage, one slot per pair, as `ext4_ops` keeps its. All seven
programs attached on the 6.12 module-XFS guest. The CIL wait is a count
only: `xfs_log_cil_wait` fires before the sleep on `xc_push_wait` and
nothing traces the wake, and the function around it is static, so a
latency would need a kprobe on a function the compiler may inline. The
counts duplicate two `xfs_stats` fields on purpose: `xlog_grant_head_wait`
increments `xs_sleep_logspace` in the same loop trip that fires the sleep
tracepoint, and both force functions increment `xs_log_force` once at
entry, so `xfs_log_waits{wait="space"}` and `{wait="force"}` must equal
`xfs_log_space_sleeps` and `xfs_log_forces` per mount. That equality is
the cross-check below.

**Forces: exact, per mount and per cgroup.** Per mount, against the mount's
own `stats/stats` read after the counters stopped moving:

| run | mount, device | `xfs_log_waits{wait="force"}` | `log/force` | mean force |
|---|---|---|---|---|
| 1 | A, loop, 64 MiB log; fio 8 jobs 4 KiB write+fsync 20 s | 12,391 | 12,391 | 9.4 ms |
| 1 | B, loop; fio 4 jobs write+fsync 10 s | 3,250 | 3,250 | 9.8 ms |
| 2 | A, loop; 300 fsyncs then an unsynced flood | 393 | 393 | 6.6 ms |
| 2 | B, loop; fio | 3,262 | 3,262 | 9.8 ms |
| 3 | C, dm-delay (writes +20 ms); 994 fsyncs | 1,102 | 1,102 | 51.4 ms |

The histogram total equaled the summed counts every time (15,641; 3,655;
1,102). A Python driver run under `systemd-run --scope` wrote and fsynced
300 files: its cgroup shows exactly 300 forces in both runs, 1.80 s and
1.83 s blocked in total, 6.0 ms per fsync on the loop device. The `other`
slot stayed at 0. Unmounting B freed its slot at the next rescan and its
series left the snapshot. The force histogram on the dm-delay device sits
where two 20 ms writes put it (51 ms mean): the fsync's log write and the
data flush before it.

tracefs is not the reference for forces: `xfs_log_force` fired 31,298 and
7,322 times over the two runs against 15,641 and 3,655 forces completed
and counted by the stats file, two per force plus the few before the agent
attached. The tracepoint fires more than once on a force's path; the stats
field increments once, and the sampler agrees with the field.

**Log space: not exercised.** Three attempts to fill a 64 MiB log did not
produce a single `xs_sleep_logspace` on this rig: 8 fio jobs creating and
fsyncing files (run 1); 8 driver threads creating, writing and unlinking
without fsync at 10,636 ops/s on the loop device (run 2, 748,350
reservations); the same flood at 6,551 ops/s on a dm-delay device with 20
ms writes (run 3, 932,130 reservations). The CIL cancels most of what a
create-then-unlink logs and the AIL kept the tail moving, so the grant
heads never reached the log's end. The sampler's space count equaled the
stats file's at 0 on every mount, and the grant pair runs through the same
`wait_begin`/`wait_end` code the force pair verified, but the space
latency has not been seen with a real value. See Deferred.

**Cost.** Two measures. `perf stat` over the driver on `null_blk` XFS
(8 threads on CPUs 8–15, agent on 0–7, three arms, three 20 s reps) could
not resolve the probes: one op costs about 1.4 ms of task clock and 360k
instructions, and the per-op spread across reps (±8k–17k instructions)
was an order of magnitude above the difference between arms, which
changed sign between run 2 (+7,573 instructions per op, `xfs_log` minus
idle) and run 3 (−6,224). The kernel's own program statistics, with
`kernel.bpf_stats_enabled=1`, are precise:

| rep | forces | BPF runs | runs per force | ns per run | ns per force |
|---|---|---|---|---|---|
| 1 | 111,029 | 222,058 | 2.00 | 654 | 1,307 |
| 2 | 111,289 | 222,578 | 2.00 | 639 | 1,278 |
| 3 | 108,726 | 217,452 | 2.00 | 640 | 1,281 |

So **1.3 µs per force**, for the `fentry` and `fexit` together: task
storage get, the `dev_t → slot` hash lookup, a histogram increment, two
per-filesystem counters and the cgroup accounting. At 20,000 forces a
second that is 26 ms of CPU a second, 2.6% of one core; the loop devices
here forced about 600 times a second. Per crossing this is close to
`ext4_ops`'s +2.26 µs per write-plus-fsync pair (four crossings). The
sampler stays opt-in as designed; the number says a default-on decision
would be about the fsync rate a fleet runs at, not about the probe.

Refresh cost (the `sampling latency` line, one `counters` bank read plus
two histograms and the cgroup maps): p50 220–249 µs across the runs,
p99 509 µs, max 571 µs in run 3 and one 3.9 ms outlier in run 2 (n=47).
Instruction counts 677 (`xfs_log_grant_wake`) and 678
(`xfs_log_force_seq_fexit`).

**Review.** Adversarial review found no defect. Its one note is now in the
descriptions: a force called without `XFS_LOG_SYNC` (inode unpinning,
buffer locking) returns once the write is issued, so the force histogram's
low tail is those, and the count must include them to equal
`xfs_log_forces`. A `sync` split would be one flag test in each `fentry`
if it is ever wanted.

## Deferred / reopen

- **`xfs_log` log-space latency unverified** — Open. `xfs_log_waits{
  wait="space"}` and its time are 0 everywhere the sampler has run; the
  pairing code is the force pair's, but no real grant sleep has been timed.
  Reopen on the first host whose `xfs_log_space_sleeps` is nonzero:
  compare the counts per mount (they must be equal) and read the latency.
  A rig that fills the log needs a metadata workload the CIL cannot cancel
  (creates without unlinks, or a directory that keeps growing) on a device
  slower than the loop and dm-delay devices tried here.
- **`xfs_log` default-on** — By design opt-in, with the number to revisit:
  1.3 µs per force. Reopen if a fleet's XFS hosts force the log rarely
  enough that 2.6% of a core at 20k forces/s is not the operating point.
- **CIL wait latency** — By design a count. The wait is inside a static
  function; reopen if the count is ever nonzero in production and the
  duration matters.
- **Refresh-dispatched sweeps race the snapshot walk** — Open. A
  `spawn_blocking` sweep dispatched by `refresh()` runs concurrently with
  the builder's walk of the same tick, and when it finishes mid-walk the
  group's window becomes the union of two sweeps, an interval wide (seen
  once in the `xfs_stats` run above). Applies to `memory_slabinfo` and
  `filesystem` as well. Options: drive sweeps from a timer so they land
  between scrapes rather than during one, or let the builder emit the
  pre-pass values with the pre-pass window for sampler-stamped groups.
  Reopen when the band width matters to a consumer.
- **`xstrat` and the other stats lines** — By design, not published; a
  `FIELDS` row each. Reopen with a question that needs one.
- **Page-cache hit ratio (C7)** — Moved to its own entry,
  `2026-09-29-pagecache-hit-ratio.md`: probed, designed, NO-GO for now.
- **Log space gauge** — By design, not built. The grant-head decode differs
  before and after 6.11; the sleep counts and the sleep latency say when
  the log is the limit without a gauge.
- **`xfs_alloc` BPF** — Idea, see decision 3.
