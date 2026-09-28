# ext4 telemetry through eBPF — journal first, allocator second

- **Opened:** 2026-09-28
- **Status:** **OPEN — design, pre-build, nothing built.** Two BPF samplers are
  specified: `ext4_journal` (jbd2 commit and checkpoint phases, fsync counts,
  filesystem errors) and `ext4_alloc` (block allocator effort, writeback
  results, inode churn). Host-wide first; per-filesystem attribution is a
  third phase gated on infrastructure that does not exist yet. Three probes
  must run on fleet kernels before the first line of BPF is written — see
  *Go/no-go*.
- **Driver:** Rezolus observes ext4 only from outside. `blockio` sees the
  request the filesystem eventually issues, `syscall_latency` folds `fsync`
  into a class shared with `open`, `stat` and `getdents`
  (`src/agent/samplers/syscall/linux/mod.rs`, class 9), and `filesystem`
  reports occupancy from `statvfs` once a minute. Nothing reports what the
  filesystem itself did between the syscall and the device: how long a
  journal commit spent flushing versus logging, whether writeback fell behind,
  whether the allocator is scanning dozens of block groups per allocation, or
  that the filesystem logged an error before it went read-only.
- **Owner:** Brian Martin

## What the kernel exposes

ext4 and jbd2 export their instrumentation as tracepoints, and the vendored
aarch64 header lists them: `src/agent/bpf/aarch64/vmlinux.h` carries **104**
`trace_event_raw_ext4_*` / `trace_event_raw_jbd2_*` structs. Every hook this
design names is a tracepoint, so principle 4's preference order applies as it
did for `blockio`: `tp_btf` with a `raw_tp` twin, never a kprobe.

The x86_64 header carries **zero** of them, and none of the jbd2 or ext4
private types either (`transaction_run_stats_s`, `transaction_chp_stats_s`,
`ext4_allocation_context`, `extent_status`, `journal_s`, `ext4_sb_info` are
all absent from `src/agent/bpf/x86_64/vmlinux.h` and all present in the
aarch64 one). Both headers were last touched by the same commit (`717bd72e`),
so the difference is not one header being older than the other. The only
reading consistent with it is that the x86_64 snapshot came from a
kernel with `CONFIG_EXT4_FS=m`, where ext4's and jbd2's types live in module
BTF (`/sys/kernel/btf/ext4`, `/sys/kernel/btf/jbd2`) rather than in vmlinux.
That fact drives two decisions below and the first go/no-go probe: such
kernels exist in our own toolchain, so the fleet has to be surveyed rather
than assumed.

The hooks, grouped by the question they answer. Fields are from the aarch64
header's `trace_event_raw_*` structs; a `tp_btf` program receives the
tracepoint's raw arguments, which are named where they differ.

**Journal and durability.** Lowest event rates, highest value.

| tracepoint | fires | carries |
|---|---|---|
| `jbd2_run_stats` | once per commit | `dev`, `tid`, `struct transaction_run_stats_s*`: `rs_wait`, `rs_request_delay`, `rs_running`, `rs_locked`, `rs_flushing`, `rs_logging` (all **jiffies**), `rs_handle_count`, `rs_blocks`, `rs_blocks_logged` |
| `jbd2_checkpoint_stats` | once per checkpoint | `dev`, `tid`, `struct transaction_chp_stats_s*`: `chp_time` (jiffies), `forced_to_close`, `written`, `dropped` |
| `jbd2_lock_buffer_stall` | when a buffer lock stalls | `dev`, `stall_ms` (scalar, no struct) |
| `jbd2_handle_stats` | once per handle stop, i.e. per metadata operation | scalars: `interval`, `sync`, `requested_blocks`, `dirtied_blocks` |
| `ext4_sync_file_enter` / `_exit` | per fsync/fdatasync | enter: `struct file*`, `datasync`; exit: `struct inode*`, `ret` |
| `ext4_fc_commit_stop`, `ext4_fc_stats` | per fast commit | only with `fast_commit` enabled, off by default |

**Errors.** `ext4_error(sb, function, line)` fires on every `ext4_error*`
call, before `errors=remount-ro` takes effect; `ext4_shutdown(sb, flags)` on
a forced shutdown. Both are in the aarch64 header. `ext4_error` is younger
than the 5.8 floor; the first kernel that has it is confirmed in probe 2, and
its absence must not fail the load (*Decisions*, twin selection).

**Writeback and delayed allocation.** `ext4_writepages_result(inode, wbc,
ret, pages_written)` fires once per writeback pass with `pages_skipped` and
`sync_mode`. `ext4_da_write_pages`, `ext4_da_reserve_space` and
`ext4_da_release_space` fire per page or per reservation and are left out
until measured.

**Block allocator.** `ext4_mballoc_alloc(struct ext4_allocation_context*)`
fires once per allocation and carries the original and result extent lengths,
`found`, `groups` scanned, and the criterion `cr` the allocator had to fall to.
One event, no pairing: allocated-shorter-than-requested is fragmentation,
`groups` is effort, and `cr` past the first two criteria means the free-space
bitmaps are being picked over. `ext4_free_blocks(count)`,
`ext4_allocate_inode`, `ext4_free_inode`, `ext4_discard_preallocations` and
the `ext4__trim` class (`ext4_trim_extent`, `ext4_trim_all_free`) are
counters.

**Extent cache.** `ext4_es_lookup_extent_exit(found)` gives a hit ratio and
`ext4_es_shrink_scan_exit(nr_shrunk, cache_cnt)` shows eviction under memory
pressure. Lookups fire on every block mapping, so this is a measured-first
hook.

**VFS entry points** (`ext4_file_read_iter`, `ext4_file_write_iter`) are
functions, not tracepoints; they would need `fentry`/`fexit`. Rejected for
now, reasons below.

## Decisions

**Two samplers, phased, no shared hooks.** `ext4_journal` attaches the jbd2
tracepoints, the two `ext4_sync_file` tracepoints and the two error
tracepoints. `ext4_alloc` attaches `ext4_mballoc_alloc`,
`ext4_writepages_result`, `ext4_free_blocks`, the inode pair and the trim
class. No hook appears in both, so principle 11 does not force one sampler,
and the two have different event-rate profiles: commits happen at most once
per fsync and by default every 5 s, while allocations track write throughput.
Keeping them apart keeps the per-sampler enable knob meaningful. Directory:
`src/agent/samplers/ext4/linux/{journal,alloc}/`, config sections
`[samplers.ext4_journal]` and `[samplers.ext4_alloc]`, following
`blockio/linux/{latency,requests}`.

**Twin selection must consult module BTF, not `kernel_has_btf()`.**
`kernel_has_btf()` (`src/agent/bpf/mod.rs`) answers whether
`/sys/kernel/btf/vmlinux` exists. A `tp_btf` program additionally needs its
own target — the `btf_trace_<name>` typedef — resolvable at load time, and on
a `CONFIG_EXT4_FS=m` kernel that typedef is in `/sys/kernel/btf/ext4` or
`/sys/kernel/btf/jbd2`. Choosing the `_btf` twin on `kernel_has_btf()` alone
would turn a module kernel into a load failure for the whole skeleton, which
`BpfBuilder::build` treats as fatal (`open_skel.load()` failure calls
`set_failed`). `kernel_btf_has_funcs` already exists for the fentry case; the
plan adds a `kernel_btf_has_tracepoints(names)` that checks vmlinux BTF and
then every `/sys/kernel/btf/<module>` for `btf_trace_<name>`, and the sampler
selects twins per hook on that answer. The same helper is what lets a
tracepoint younger than the kernel floor (`ext4_error`) fall to its `raw_tp`
twin, whose attach failure `build` already tolerates as ENOENT.

**Kernels that can run it.** Reading a jbd2 or ext4 struct field with
`BPF_CORE_READ` needs BTF for that struct. On a built-in ext4 it is in
vmlinux BTF; on a module ext4 it is in module BTF, which kernels expose from
5.11 (`CONFIG_DEBUG_INFO_BTF_MODULES`). A 5.8 to 5.10 kernel with
`CONFIG_EXT4_FS=m` has neither, and there the `ext4_journal` sampler can
attach only the hooks whose arguments are scalars (`jbd2_lock_buffer_stall`,
`jbd2_handle_stats`) plus the counting hooks that read nothing
(`ext4_sync_file_enter`, `ext4_error`); `jbd2_run_stats` cannot be read.
Decision: the sampler declares its struct-reading programs as required, so
`rezolus status` shows the sampler degraded on such a kernel rather than
silently reporting zeros, and the module doc says why. Reading the
`tracepoint/` format struct at fixed offsets to dodge CO-RE was considered and
rejected: the offsets are not stable across versions or architectures and the
header comment would be the only thing protecting them.

**Missing types are declared locally, `core_fixes.h` style.** The x86_64
build has no `struct transaction_run_stats_s`. The BPF C declares the fields
it reads in a local struct with `__attribute__((preserve_access_index))`,
exactly as `core_fixes.h` declares `task_struct___o`; CO-RE then relocates
against the running kernel's BTF at load. Phase 1 needs two such structs
(`transaction_run_stats_s`, `transaction_chp_stats_s`), phase 2 one more
(`ext4_allocation_context`: `ac_o_ex.fe_len`, `ac_b_ex.fe_len`, `ac_found`,
`ac_groups_scanned`, `ac_criteria`; note `ac_criteria` became an enum in 6.5,
which a CO-RE read of the integer field survives). The vendored headers are
not regenerated for this (principle 2: updates are deliberate and
version-pinned).

**Jiffies become nanoseconds in BPF, from a measured tick.** `jbd2_run_stats`
and `jbd2_checkpoint_stats` report their phases in jiffies (the tracepoint's
own printer applies `jiffies_to_msecs`), and `jbd2_lock_buffer_stall` in
milliseconds. The histograms must be in nanoseconds like every other latency
metric, so the program multiplies by a tick length it reads from a one-entry
config map loaded through `BpfBuilder::map` (the mechanism `syscall_lut`
uses). Userspace derives the tick as
`clock_getres(CLOCK_MONOTONIC_COARSE)`, which the kernel answers with
`TICK_NSEC`, rather than from `sysconf(_SC_CLK_TCK)` (that is `USER_HZ`, a
constant 100) or a `__kconfig` extern (needs `/proc/config.gz`, and a
`__weak` fallback would be an untested default). The resolution is one jiffy,
1 to 10 ms; the metric descriptions say so, since a distribution of ms-scale
commit phases quantized to 4 ms is honest only if the reader knows it.

**No side maps in phase 1; fsync latency comes from the syscall sampler.**
The obvious ext4 latency metric is `ext4_sync_file_enter` to `_exit`, and it
needs a per-thread start timestamp. `syscall_latency` already keeps exactly
that map (`start`, `MAX_PID` = 4,194,304 entries × `u64` = 32 MB), stamped on
every syscall entry, and its exit program already has the fsync latency in
hand; the only reason fsync is not visible today is that class 9 lumps it
with some forty metadata syscalls. Splitting `fsync`, `fdatasync`, `sync` and
`syncfs` into their own class (and moving `msync` there from class 10) is one
LUT entry and one histogram in an existing sampler, no new probe, no new map
(principle 11). A second
32 MB start array in `ext4_journal` would buy only per-filesystem attribution
on top of that, and phase 1 has no per-filesystem histograms to put it in
(next decision). So `ext4_journal` attaches the sync tracepoints for what
they carry without pairing: `datasync` on entry (fsync versus fdatasync
counts), `ret` on exit (failed syncs). Per-filesystem sync latency is
deferred with two candidate mechanisms recorded: the `MAX_PID` array, or
`BPF_MAP_TYPE_TASK_STORAGE` (5.11+, per-task, no cross-core contention) once
the kernel floor allows it.

**Host-wide first, per-filesystem third.** Every histogram in the codebase is
one mmap'd 496-bucket array per metric (`bpf/histogram.rs`); there is no
histogram with slots, and `docs/backlog.md` ("histogram groups with slots")
records that per-device block IO latency waits on the 6.0 writer for the same
reason. Counters with slots are possible today in the `PackedCounters` shape,
but a first sampler should establish the probes and the measured cost before
adding a slot dimension. Most hosts carry one or two ext4 filesystems, so
host-wide journal telemetry answers the driver on most of the fleet. Phase 3
(per-filesystem counters) is specified below so the phase 1 code leaves room:
every hook already has `dev` in hand, either as a raw argument (`jbd2_*`) or
via `inode->i_sb->s_dev` (`ext4_*`).

**jbd2 metrics are named `ext4_journal_*` and the caveat is recorded.** jbd2
is also the journal for ocfs2. A host running ocfs2 would see its commits
counted here. The dashboard, the driver and the fleet are ext4, so the metrics
carry the name people will search for; `docs/metrics.md` states the caveat.
Phase 3's per-filesystem slots resolve it, since a slot is assigned only to a
mount whose `fstype` is `ext4`.

**Errors: count with the tracepoint, and also read the sysfs gauge.**
`ext4_error` and `ext4_shutdown` are rare events and cost nothing to count, so
`ext4_journal` counts them. But `/sys/fs/ext4/<blockdev>/errors_count` (with
`first_error_time`, `last_error_time`, `last_error_func`) already holds the
count since mount, works on every kernel including the ones the BPF sampler
cannot run on, and the `filesystem` sweep already visits each local mount once
a minute with its `block_device` name in hand
(`src/agent/samplers/filesystem/linux/mod.rs`). Adding `errors_count` and
`lifetime_write_kbytes` to that sweep is the smaller change and the one that
pages someone; it is filed as a follow-up in *Deferred*, not built here.
Principle 15's per-refresh-parsing objection does not apply to a read that
rides a 60 s off-cycle sweep, the same ruling `filesystem` itself received.

**Hooks left out of phases 1 and 2, and why.** Event rate is what decides
overhead, not probe body (the 2026-09-03 blockio entry measured the probe
body as the minor term). Per-page and per-lookup hooks wait for a
measurement: `ext4_da_write_pages`, `ext4_da_reserve_space`,
`ext4__write_begin`/`__write_end`, `ext4__page_op`,
`ext4_es_lookup_extent_enter`/`_exit`, `ext4__map_blocks_*`. `jbd2_handle_stats`
and `ext4_journal_start` fire per metadata operation and are the same
category. `jbd2_commit`/`jbd2_end_commit` are redundant with `jbd2_run_stats`
for counting and would need pairing for latency. Fast commit is off by
default.

**`fentry` on the VFS entry points is rejected for now.** `ext4_file_read_iter`
and `ext4_file_write_iter` would give read and write latency at the
filesystem layer with the bytes returned, and read latency there is bimodal
in a way that separates page-cache hits from misses. Three reasons to wait:
they are function symbols whose split into buffered and direct-IO variants
moved between 5.5 and 5.10; the syscall sampler already reports host-wide
read and write latency; and the value over that is per-filesystem, which
needs phase 3. Reopen condition in *Deferred*.

**Acquisition groups.** Following principle 18's like-entities rule as
`blockio_latency` applies it:

- `ext4_journal_commit_latencies` — six histograms, one family, `phase` label.
- `ext4_journal_checkpoint_latencies` — one histogram.
- `ext4_journal_stall_latencies` — one histogram.
- `ext4_journal_counters` — one `counters` array read in one sweep, as
  `blockio_requests` reads ops and bytes from one map.
- `ext4_alloc_counters` — likewise for the second sampler.
- `ext4_alloc_allocation_sizes` — the one histogram in the second sampler.

**Metrics, phase 1 (`ext4_journal`).** All counters are `Counters` (per-CPU
banks summed in userspace); per-CPU exposure is meaningless for events the
`jbd2/<dev>` kernel thread generates.

| metric | type | labels | source |
|---|---|---|---|
| `ext4_journal_commit_latency` | histogram, ns | `phase` = `wait`, `request_delay`, `running`, `locked`, `flushing`, `logging` | `jbd2_run_stats` `rs_*` × tick |
| `ext4_journal_commits` | counter | | `jbd2_run_stats` |
| `ext4_journal_commit_handles` | counter | | Σ `rs_handle_count` |
| `ext4_journal_commit_blocks` | counter | `kind` = `dirtied`, `logged` | Σ `rs_blocks`, Σ `rs_blocks_logged` |
| `ext4_journal_checkpoint_latency` | histogram, ns | | `jbd2_checkpoint_stats` `chp_time` × tick |
| `ext4_journal_checkpoints` | counter | | `jbd2_checkpoint_stats` |
| `ext4_journal_checkpoint_buffers` | counter | `outcome` = `written`, `dropped` | Σ `written`, Σ `dropped` |
| `ext4_journal_checkpoint_forced_to_close` | counter | | Σ `forced_to_close` |
| `ext4_journal_lock_buffer_stall_latency` | histogram, ns | | `jbd2_lock_buffer_stall` `stall_ms` × 10⁶ |
| `ext4_sync_file` | counter | `op` = `fsync`, `fdatasync` | `ext4_sync_file_enter` `datasync` |
| `ext4_sync_file_errors` | counter | | `ext4_sync_file_exit` `ret < 0` |
| `ext4_errors` | counter | | `ext4_error` |
| `ext4_shutdowns` | counter | | `ext4_shutdown` |

**Metrics, phase 2 (`ext4_alloc`).**

| metric | type | labels | source |
|---|---|---|---|
| `ext4_allocations` | counter | | `ext4_mballoc_alloc` |
| `ext4_allocation_blocks` | counter | `kind` = `requested`, `allocated` | Σ `ac_o_ex.fe_len`, Σ `ac_b_ex.fe_len` |
| `ext4_allocation_groups_scanned` | counter | | Σ `ac_groups_scanned` |
| `ext4_allocations_by_criterion` | counter | `criterion` = `0`..`4` | `ac_criteria`, bounded to 5 slots, higher values into `4` |
| `ext4_allocation_size` | histogram, blocks | | `ac_b_ex.fe_len` |
| `ext4_freed_blocks` | counter | | Σ `count` at `ext4_free_blocks` |
| `ext4_inodes` | counter | `op` = `allocated`, `freed` | `ext4_allocate_inode`, `ext4_free_inode` |
| `ext4_writepages` | counter | | `ext4_writepages_result` |
| `ext4_writepages_pages` | counter | `outcome` = `written`, `skipped` | Σ `pages_written`, Σ `pages_skipped` |
| `ext4_writepages_errors` | counter | | `ret < 0` |
| `ext4_trimmed_blocks` | counter | | Σ `len` at `ext4_trim_extent` |

**Phase 3 (per-filesystem counters), specified so phases 1 and 2 leave room.**
Userspace parses the mount table it already parses for `filesystem`
(`mounts::parse_mountinfo`, `MountEntry::device` is `major:minor`), assigns
each `ext4` mount a slot under a `MAX_FILESYSTEMS` bound, and writes
`dev_t → slot` into a lookup map. The counter arrays widen to
`MAX_FILESYSTEMS × COUNTER_GROUP_WIDTH` per CPU bank (64 × 16 × 1024 × 8 B =
8 MB, of which the refresh sums only possible CPUs), and each slot's labels
(`devnum`, `block_device`, `mount`) are the ones `filesystem` publishes, so
the two samplers join. The lookup map is the one open mechanism question:
`BPF_MAP_TYPE_HASH` keyed by `dev_t` is written only by userspace and read
lock-free by BPF, so principle 5's contention argument does not apply, but
the principle requires the justifying comment; a bounded linear scan over
`MAX_FILESYSTEMS` `dev_t` values in an array is the hashless alternative and
is O(1) at the bound. Measure both on the phase 1 bench before choosing.
Histograms per filesystem wait for histogram groups with slots, as the
backlog already records for `blockio_latency`.

**Validation oracle.** `/proc/fs/jbd2/<blockdev>-8/info` reports, per
journal, the transaction count and the average wait, request delay, running,
locked, flushing and logging times in ms, plus average handles and blocks per
transaction. Those are the same `rs_*` fields the sampler reads, averaged
since mount. A sampler run over a mount's whole life must reproduce the
averages within one jiffy of rounding, and the commit count exactly. This is
also the honest answer to principle 15's "why not procfs": the file gives
lifetime averages of one journal, the sampler gives distributions and rates,
and the file is read once as a check rather than every tick as a source.

## Go/no-go — probes before build

Three probes on fleet-representative hosts, none of which needs a rezolus
build, then the build gates.

**Probe 1 — where do the types live.** On each kernel the fleet runs:

```sh
grep -E 'CONFIG_EXT4_FS=|CONFIG_DEBUG_INFO_BTF_MODULES=' /boot/config-$(uname -r)
ls /sys/kernel/btf/ | grep -E '^(ext4|jbd2)$'
bpftool btf dump file /sys/kernel/btf/vmlinux | grep -c "btf_trace_jbd2_run_stats"
```

GO for `tp_btf` on a host where `btf_trace_jbd2_run_stats` resolves in
vmlinux or a module BTF. A host with `CONFIG_EXT4_FS=m` and no module BTF is
the degraded case; if that is a material share of the fleet, the phase 1
scope shrinks to the scalar hooks on those hosts and the entry says so.

**Probe 2 — argument signatures on the oldest and newest fleet kernel.**

```sh
bpftool btf dump file /sys/kernel/btf/vmlinux format c 2>/dev/null \
  | grep -A4 'btf_trace_\(jbd2_run_stats\|jbd2_checkpoint_stats\|ext4_sync_file_enter\|ext4_sync_file_exit\|ext4_error\|ext4_writepages_result\|ext4_mballoc_alloc\)'
```

(or the module BTF file, per probe 1). Confirms each `TP_PROTO` this entry
assumes and the first kernel that carries `ext4_error`. A signature that
differs across the fleet's kernels gets a CO-RE branch, not a guess.

**Probe 3 — the tick.** `clock_getres(CLOCK_MONOTONIC_COARSE)` on a 250 Hz and
a 1000 Hz kernel returns 4,000,000 and 1,000,000 ns respectively. Two lines
of Rust in a test; if it does not, the jiffies decision above is wrong and
`/proc/config.gz` is the fallback to price.

**Build gates (phase 1), each a measured number in the close-out:**

- Per-refresh cost from the `ext4_journal sampling latency: N us` debug line,
  release build, following the `reviewing-samplers` recipe. The read is nine
  mmap'd arrays (8 histograms of 496 buckets, one counter map with a bank per
  CPU), so
  the expected order is `blockio_latency`'s; the number is what gets reported.
- Probe marginal cost under the worst realistic hook rate: `fio` `--fsync=1`
  random write on an ext4 filesystem over `null_blk` or a fast NVMe, sampler
  off versus on, the method of `2026-09-03-blockio-latency-rq-fields.md`
  (`perf stat -C` on isolated cores, four repetitions, the rest of the agent
  disabled). GO at under 1% throughput change at saturation; the `fsync`
  hooks are the only ones that scale with the workload, and each is one
  `array_incr`.
- Agreement with the `/proc/fs/jbd2/*/info` averages over the run, within one
  tick.
- One qualitative check, recorded with numbers: on the fio run, the
  `flushing` and `logging` phase distributions should account for most of the
  commit time, and their sum should track `blockio_device_latency` for flush
  ops on the same device. If they do not, the phase semantics are
  misunderstood and the descriptions are wrong.

**NO-GO conditions.** Probe 1 shows the fleet is predominantly module-ext4
without module BTF: the sampler as designed cannot read commit phases there,
and the entry closes as NO-GO with the sysfs `errors_count`/`lifetime_write_kbytes`
follow-up as the deliverable. The bench shows the `ext4_sync_file` counters
costing more than 1% at saturation: drop them from phase 1 (the syscall
class split still delivers fsync latency) and keep the commit hooks, whose
rate is bounded by the commit rate.

## Plan

1. **Probes 1 to 3** on fleet kernels; record results here.
2. **`kernel_btf_has_tracepoints`** in `src/agent/bpf/mod.rs`, vmlinux plus
   module BTF, unit-tested against a fixture listing.
3. **Syscall class split**: a `sync` class in
   `src/agent/samplers/syscall/linux/mod.rs` and its histogram in
   `syscall/linux/latency/{stats.rs,mod.bpf.c}`. Independent of the rest;
   lands on its own.
4. **`ext4_journal`**: `src/agent/samplers/ext4/linux/journal/{mod.rs,
   mod.bpf.c,stats.rs}`, `tp_btf`/`raw_tp` twins per hook, the local struct
   declarations, the tick config map, registration in
   `src/agent/samplers/mod.rs`, `config/agent.toml`, `docs/metrics.md`. Bench
   and close-out numbers in this entry.
5. **Dashboard**: an ext4 section in `crates/dashboard/src/dashboard/`
   (commit phase percentiles, checkpoint latency, fsync rate and errors,
   filesystem errors), built once per the `viewer-parity` skill for both
   backends.
6. **`ext4_alloc`** as phase 2, same shape, its own bench on a write-heavy
   fio run since `ext4_mballoc_alloc` tracks write throughput.
7. **Phase 3** per-filesystem counters, after the lookup-map measurement.

## Deferred / reopen

- **Per-filesystem sync latency** — Roadmap. Needs histogram groups with
  slots (backlog, after 6.0) and a per-thread start map: the `MAX_PID` array
  (32 MB, the `syscall_latency` shape) or `BPF_MAP_TYPE_TASK_STORAGE` once the
  kernel floor reaches 5.11. Until then fsync latency is host-wide from the
  syscall sampler's new `sync` class.
- **VFS-layer read/write latency via `fentry`/`fexit`** on
  `ext4_file_read_iter`/`ext4_file_write_iter` — Idea. Value is the
  page-cache hit/miss split per filesystem; reopen with phase 3, and check
  the symbol set on the oldest fleet kernel first (the buffered/direct split
  moved between 5.5 and 5.10).
- **Extent-status cache hit ratio and shrink** — Idea. `ext4_es_lookup_extent_exit`
  fires per block mapping; measure its rate on a read-heavy workload before
  attaching. `ext4_es_shrink_scan_exit` alone is cheap and could join
  `ext4_alloc`.
- **Handle-level stats** (`jbd2_handle_stats`, `ext4_journal_start`) — Idea.
  Per metadata operation; measured-first.
- **Fast commit** (`ext4_fc_*`) — By design, off by default in ext4. Reopen if
  a fleet enables `fast_commit`.
- **sysfs `errors_count` and `lifetime_write_kbytes` in the `filesystem` sweep**
  — Open. Works on every kernel, rides the existing 60 s off-cycle sweep, and
  the `block_device` name is already resolved per mount. Smaller than the BPF
  sampler and independent of it.
- **Per-page and per-reservation writeback hooks** — Idea. `ext4_da_write_pages`,
  `ext4_da_reserve_space`, `ext4__write_begin`; rate-gated.
- **jbd2 counts include ocfs2** — By design until phase 3, where slots are
  assigned by `fstype`.
- **Degraded on module-ext4 kernels below 5.11** — By design. No module BTF
  means no CO-RE against jbd2 structs; the sampler reports degraded and the
  module doc points here.
- **XFS** — Idea. XFS exports several hundred tracepoints of its own with a
  different journaling model (log grant, AIL push, CIL checkpoints); it is a
  separate design, not a label on this one.
