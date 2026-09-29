# XFS telemetry: per-mount stats first, BPF for what stats cannot say

- **Opened:** 2026-09-29
- **Status:** **OPEN — design, probed, nothing built.** Step 7 (C8) of
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
   docs. Verified against `/proc/fs/xfs/stat` in the VM.
2. **`xfs_log`** with grant-sleep and force latency, CIL waits, per-cgroup
   blocked time; benched on `null_blk` XFS before it is offered even as an
   opt-in.
3. Reopen `xfs_alloc` if XFS free-space fragmentation becomes a question.

## Deferred / reopen

- **Page-cache hit ratio (C7)** — Idea, the other half of step 7. Misses
  from `mm_filemap_add_to_page_cache`; hits need `fentry` at read rate.
  Deferred behind the XFS steps above; reopen when a read-path finding
  needs it.
- **Log space gauge** — By design, not built. The grant-head decode differs
  before and after 6.11; the sleep counts and the sleep latency say when
  the log is the limit without a gauge.
- **`xfs_alloc` BPF** — Idea, see decision 3.
