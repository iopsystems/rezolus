# Page-cache hit ratio: probed, designed, not built

- **Opened:** 2026-09-29
- **Status:** **NO-GO for now — probed and designed, nothing built.** The
  second half of step 7 (C7) of `2026-09-28-filesystem-telemetry-gaps.md`.
  One probe on the hv01 Debian 13 guest (`6.12.63+deb13-amd64`) measured how
  often every candidate hook fires per read under cached, cold, sequential
  and small-file workloads; the numbers are below, and they change the
  design the gaps entry sketched. The sampler is specified here and costs
  two probe crossings per read call, at the read rate, for a question no
  finding in the record has asked. Reopen conditions at the end.
- **Driver:** the gaps entry's C7: a file-per-object cache serving reads has
  no continuous view of whether its reads are served from memory or from
  the device, per filesystem and per cgroup. `memory_vmstat` carries the
  host-wide half (`pgpgin`, `pgmajfault`, the workingset refault counters)
  and `memory_meminfo` the cache size; nothing attributes a fill to a mount
  or a service, and nothing counts the reads that did not fill.
- **Owner:** Brian Martin

## What the kernel exposes

**Tracepoints.** The `filemap` subsystem on the probe kernel has seven
(systemslab `01a0ecce-2c93-717a-37ae-ab35e272b7a1`), five of them
`mm_filemap_*`, all with `s_dev` and `i_ino` in the record:

| tracepoint | fires | fields | since |
|---|---|---|---|
| `mm_filemap_add_to_page_cache` | once per folio added to a file's page cache: a read miss, a readahead page, or a buffered write of a new page | `pfn`, `i_ino`, `index`, `s_dev`, `order` | old (in both vendored `vmlinux.h`) |
| `mm_filemap_delete_from_page_cache` | once per folio removed: reclaim, truncate, invalidation | same | old |
| `mm_filemap_get_pages` | once per `filemap_get_pages` call, with the requested range | `i_ino`, `s_dev`, `index`, `last_index` | 6.12 |
| `mm_filemap_map_pages` | once per fault-around mapping | same | 6.12 |
| `mm_filemap_fault` | once per page-cache fault (mmap read) | `i_ino`, `s_dev`, `index` | 6.12 |

The vendored `src/agent/bpf/{x86_64,aarch64}/vmlinux.h` carry BTF for the
first two only, so a program using the 6.12 three needs `tp_btf`/`raw_tp`
twins selected at load, as `ext4_journal` does per hook.

**Functions.** In `/proc/kallsyms` on this build: `filemap_read` (the
buffered read path's entry, global), `filemap_get_pages` and
`filemap_get_read_batch` (static in `mm/filemap.c`, not inlined here, so
they have symbols on this build and may not on another), `__filemap_get_folio`,
`folio_mark_accessed`, `filemap_add_folio`. `filemap_readahead` and
`filemap_update_page` were inlined away.

## Measured: how often each hook fires

Kprobes on the functions and the tracepoints, counted with `perf stat -a`
over each phase; one 1 GiB file of random bytes and 20,000 files of 4 KiB
on the ext4 root (virtio disk), 56 vCPU, 220 GiB RAM. fio's default
`invalidate=1` drops a file's pages when a job starts, so the two "warm"
fio phases below started cold and are mixed; the `dd` and small-file warm
phases are genuinely warm (0 adds).

| phase | read calls | `mm_filemap_get_pages` | `folio_mark_accessed` | pages added | pages deleted | `__filemap_get_folio` | `pgpgin` (KiB) |
|---|---|---|---|---|---|---|---|
| fio randread 4 KiB, 4 jobs, 10 s (131k IOPS; invalidated at start) | 1,311,551 | 1,311,550 | 1,320,548 | 441,987 | 525,058 | 432 | 1,768,040 |
| same, after `drop_caches` (137k IOPS) | 1,367,255 | 1,367,254 | 1,376,399 | 468,958 | 263,093 | 1,548 | 1,896,844 |
| `dd` 1 GiB, 1 MiB reads, cold | 1,035 | 9,765 | 263,118 | 262,234 | 0 | 308 | 1,070,536 |
| `dd` 1 GiB, warm | 1,035 | 9,225 | 263,009 | 0 | 0 | 5 | 0 |
| 20,000 small files read once, 4 threads, cold | 20,005 | 20,005 | 103,380 | 21,378 | 0 | 28,657 | 106,976 |
| 20,000 small files, 10 passes, warm | 200,005 | 200,005 | 200,834 | 0 | 0 | 6 | 0 |
| fio randread 4 KiB, 8 jobs, 10 s (444k IOPS; invalidated) | 4,440,251 | 4,440,250 | 4,451,018 | 700,662 | 530,362 | 1,266 | 2,802,616 |

What the table says:

1. **Hits are countable per read call, not per page.** `mm_filemap_get_pages`
   fires exactly once per `filemap_read` call for 4 KiB reads (1,311,550
   against 1,311,551; 200,005 against 200,005) and about nine times per 1
   MiB read (the folio batch is 15 folios), carrying the requested index
   range. `filemap_read` itself is one global function, `fentry`/`fexit`
   bracket a read call on every kernel in range. Neither is a per-page hook.
2. **`__filemap_get_folio` is not the read path.** The gaps entry proposed
   `fentry` on `filemap_get_folio` "at every read"; the read path looks pages
   up through the static `filemap_get_read_batch`, and `__filemap_get_folio`
   fired 432 times for 1.3 million reads (it is the write, fault and
   metadata path: 28,657 on the cold small-file pass, the directory and
   inode reads). A sampler built on it would have seen no reads.
3. **`folio_mark_accessed` is the per-page hook**, one per folio touched
   (263,118 for the 262,144 pages of a 1 GiB sequential read; one per 4 KiB
   read). It is what `cachestat`-style tools count as accesses; at 444k
   reads/s it fires 445k times a second.
4. **Adds conflate three things.** `mm_filemap_add_to_page_cache` fires for a
   read miss, for every readahead page the miss pulled in, and for a
   buffered write of a page not yet cached. The cold sequential read added
   262,234 pages for 1,035 read calls: readahead did the work, and a
   miss-per-call figure from adds alone would say 253 misses per read. The
   cold small-file pass added 21,378 pages for 20,000 reads: 20,000 data
   pages and 1,378 pages of the directory and the block device's inode
   tables, which are page cache too (the `bdev` inode) and carry the block
   device's `s_dev`, not the filesystem's.
5. **The host already has the fill rate.** `pgpgin` tracked the adds within
   a few percent in every cold phase (1,070,536 KiB for 262,234 pages of
   the sequential read: 4 KiB each plus metadata), `pgmajfault` is the mmap
   half, and the workingset refault counters say when a fill is a re-fill.
   `memory_vmstat` publishes all of them host-wide. What is missing is the
   attribution (which mount, which cgroup) and the denominator (reads that
   did not fill).

## Design, if built

A `pagecache` sampler, opt-in like `ext4_ops` and `xfs_log`, floor 5.12
(task local storage for tracing programs):

- **Read bracket**: `fentry`/`fexit` on `filemap_read(kiocb, iov_iter,
  already_read) -> ssize_t`. Entry stamps the task (start time, `s_dev`
  from `iocb->ki_filp->f_inode->i_sb`, bytes requested from `iter->count`,
  an adds-during-this-call counter zeroed); exit publishes one read call
  per filesystem as `pagecache_reads{outcome="hit"|"miss"}` (miss if any
  page was added during the call), `pagecache_read_bytes`, and per cgroup
  the same two, plus a host-wide read latency histogram split by outcome,
  which is the one number that says what a miss costs this host.
- **Fills**: `tp_btf/mm_filemap_add_to_page_cache`, `1 << order` pages per
  event, per filesystem by `s_dev`, split by whether the task is inside a
  read bracket: `pagecache_pages_added{reason="read"}` is misses plus the
  readahead they triggered (sync and async readahead both run in the
  reader's context), `{reason="other"}` is buffered writes and everything
  else. Per cgroup for the read half.
- **Evictions**: `tp_btf/mm_filemap_delete_from_page_cache`, pages per
  filesystem. Reclaim runs in kswapd's context or the allocating task's, so
  no cgroup attribution is attempted.
- **Faults**: `mm_filemap_fault` where the kernel has it (6.12), `fentry`
  on `filemap_fault` before, one per mmap fault per filesystem.
- **Not** `folio_mark_accessed` or `filemap_get_read_batch`: per page, at
  page rate, for an access count the read bracket already implies.

Dashboard: a Page Cache group with hit ratio per mount
(`hit / (hit + miss)`), fills per second by reason, evictions, read
latency by outcome; a cgroups row for the miss share and the pages added.

**Cost, estimated rather than measured, because nothing was built.** The
bracket is two probe crossings per read call with task storage, a
`dev_t → slot` hash lookup, counters and cgroup accounting, the same shape
`xfs_log`'s force pair has, which measured **1.3 µs per pair** under
`kernel.bpf_stats_enabled` (that entry's Results — step 2). At the 444k
reads/s of the last phase that is 0.58 s of CPU per second; at 50k reads/s
it is 6.5% of a core, at 20k 2.6%. Every read pays it, hit or miss, which
is why it is opt-in and why it is not built ahead of a question.

## Decision

**NO-GO for now.** Three reasons, in order:

1. The cost lands on the read path at the read rate, on every read, and the
   gaps entry's own finding was that the read path was not where the
   characterization lost its SLO. The record has no read-path finding that
   this sampler would have answered.
2. The host-wide half exists: `memory_vmstat`'s `pgpgin`, `pgmajfault` and
   workingset refaults, and `memory_meminfo`'s cache size, are on every
   recording already. The missing pieces are attribution and the hit
   denominator, and both cost the bracket.
3. A misses-only always-on variant (adds and deletes per filesystem, no
   bracket) was considered and declined too: it cannot separate a miss from
   a write or a readahead page, so it would publish a fill rate the host
   already has with a `mount` label added, at 47k events a second on the
   cold random-read phase (about 3% of a core at `xfs_log`'s per-crossing
   cost).

**Reopen** when a read-path finding needs it: a service whose latency tail
tracks device reads, a working set that may or may not fit, or a readahead
question (pages added per page requested, per mount, is the readahead
efficiency and this design measures it for free). The design above is
buildable as written; the first thing to measure is the bracket's real cost
with `kernel.bpf_stats_enabled` on a read-heavy `null_blk` workload, and
the GO gate should be that cost against the fleet's read rate.

## Deferred / reopen

- **`pagecache` sampler** — NO-GO for now, design above. Reopen on a
  read-path finding; GO gate is the measured bracket cost against the
  fleet's read rate.
- **Readahead efficiency** — Idea, falls out of the design (pages added
  during reads over pages requested). Not separately pursued.
- **Block-device page cache** — Observation. Inode tables and directory
  blocks are page cache on the `bdev` inode and appear under the block
  device's `s_dev`; a per-mount view has to map the bdev back to the mount
  it backs, or leave those adds under `other`. `memory_slabinfo`'s
  `buffer_head` and the ext4 samplers' inode-table reads cover the same
  ground from the other side.
