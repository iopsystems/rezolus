# ext4 telemetry through eBPF — journal first, allocator second

- **Opened:** 2026-09-28
- **Status:** **Phases 1 and 2 SHIPPED and measured. Phase 1
  (`ext4_journal`, #1321): GO on probe cost, +225 instructions per fsync at
  450 K fsync/s on `null_blk`. Phase 2 (`ext4_alloc`): every counter exact
  against tracefs, refresh 151–301 µs; see *Results — phase 2*. Phase 3
  (per-filesystem) OPEN**, re-prioritized by
  `2026-09-28-filesystem-telemetry-gaps.md`. Two BPF
  samplers are specified: `ext4_journal` (jbd2 commit and checkpoint phases,
  fsync counts, filesystem errors) and `ext4_alloc` (block allocator effort,
  writeback results, inode churn). Host-wide first; per-filesystem
  attribution is a third phase gated on infrastructure that does not exist
  yet. Probes 1 to 3 passed on aarch64 Debian 13 (built-in ext4) and x86_64
  Debian 13 (module ext4). Phase 1 refreshes in **190–295 µs** on a 56-vCPU
  guest and its counts reconcile with fio and the jbd2 procfs oracle; see
  *Results — phase 1*. Four defects were found by running it, all recorded
  there.
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

**Probe results, 2026-09-28, pi10** (Raspberry Pi 4B, aarch64, Debian 13,
kernel `6.12.75+rpt-rpi-v8`, `CONFIG_HZ=250`; systemslab job
`01a0e919-f3f8-7161-6274-d7ec8d4fd402`):

- Probe 1: `CONFIG_EXT4_FS=y`, `CONFIG_JBD2=y`, `CONFIG_DEBUG_INFO_BTF=y`,
  `CONFIG_DEBUG_INFO_BTF_MODULES=y`; `/sys/kernel/btf/vmlinux` present and no
  `ext4`/`jbd2` module BTF, as expected for a built-in ext4. tracefs lists 115
  ext4 events and 23 jbd2 events; every hook this entry names is present,
  including `ext4_error` and `ext4_shutdown`. `bpftool` was not installed on
  the host, so the `btf_trace_*` typedef check moved to the x86_64 build job.
- Probe 2: the tracefs `format` files agree with the aarch64 header field for
  field for `jbd2_run_stats` (`dev`, `tid`, six `unsigned long` phases,
  `handle_count`, `blocks`, `blocks_logged`), `jbd2_checkpoint_stats`
  (`chp_time`, `forced_to_close`, `written`, `dropped`),
  `jbd2_lock_buffer_stall` (`stall_ms`), both `ext4_sync_file_*`,
  `ext4_error` (`function`, `line`), `ext4_shutdown` (`flags`),
  `ext4_mballoc_alloc` and `ext4_writepages_result`.
- Probe 3: `clock_getres(CLOCK_MONOTONIC_COARSE)` = 4,000,000 ns on the 250 Hz
  kernel, and `CLOCK_MONOTONIC` = 1 ns. `getconf CLK_TCK` = 100 on the same
  host, which is the `USER_HZ` trap the tick decision avoids.
- Oracle: `/proc/fs/jbd2/mmcblk0p2-8/info` reports 63,650 transactions with
  average phases in ms (running 2944, logging 4, commit time 4408 µs), 8
  handles and 5 logged blocks per transaction. `/sys/fs/ext4/mmcblk0p2/`
  carries `errors_count`, `lifetime_write_kbytes`, `session_write_kbytes`,
  `first_error_time` and `last_error_time`, confirming the sysfs follow-up.

**Probe results, 2026-09-28, hv01 guest** (x86_64, Debian 13 `debian-13-ci`
image, kernel `6.12.63+deb13-amd64`, `CONFIG_HZ=250`; systemslab job
`01a0e923-d21c-7120-9690-ed12235fbffc`):

- Probe 1: **`CONFIG_EXT4_FS=m`, `CONFIG_JBD2=m`**, `CONFIG_DEBUG_INFO_BTF=y`,
  `CONFIG_DEBUG_INFO_BTF_MODULES=y`; `/sys/kernel/btf/` holds `vmlinux`,
  `ext4` and `jbd2`. The stock Debian amd64 kernel is the module case, while
  the Raspberry Pi kernel above builds ext4 in. So the same distribution
  release lands on both sides of the twin-selection decision depending on
  architecture, and `kernel_btf_has_tracepoints` consulting module BTF is
  required on the most ordinary x86_64 Debian host, not an edge case.
  `vmlinux` BTF carries none of the nine `btf_trace_*` typedefs checked.
- Probe 3: `clock_getres(CLOCK_MONOTONIC_COARSE)` = 4,000,000 ns, matching
  `CONFIG_HZ=250`.

**NO-GO conditions.** Probe 1 shows the fleet is predominantly module-ext4
without module BTF: the sampler as designed cannot read commit phases there,
and the entry closes as NO-GO with the sysfs `errors_count`/`lifetime_write_kbytes`
follow-up as the deliverable. The bench shows the `ext4_sync_file` counters
costing more than 1% at saturation: drop them from phase 1 (the syscall
class split still delivers fsync latency) and keep the commit hooks, whose
rate is bounded by the commit rate.

## Plan

1. **Probes 1 to 3** on fleet kernels; record results here. *Done for
   aarch64 Debian 13 (above); x86_64 with the build job.*
2. **`kernel_btf_has_tracepoints`** in `src/agent/bpf/mod.rs`, vmlinux plus
   every `/sys/kernel/btf/<module>`. *Done.* The module scan is factored as
   `module_btfs(dir)` so the two failure modes that matter — no module BTF
   directory, files that are not BTF — are unit-tested without a kernel.
3. **Syscall class split**: a `sync` class in
   `src/agent/samplers/syscall/linux/mod.rs` and its histogram in
   `syscall/linux/latency/{stats.rs,mod.bpf.c}`. Independent of the rest;
   lands on its own. Note for whoever does it: sixteen classes fill the
   16-wide per-CPU counter bank exactly, so a seventeenth widens
   `COUNTER_GROUP_WIDTH` to 24 in both syscall BPF programs (the bank is a
   whole number of cachelines, `bpf/counters.rs`) and adds a
   `cgroup_syscall_sync` map to `syscall_counts`.
4. **`ext4_journal`**: `src/agent/samplers/ext4/linux/journal/{mod.rs,
   mod.bpf.c,stats.rs}`, `tp_btf`/`raw_tp` twins per hook, the local struct
   declarations, the tick config map, registration in
   `src/agent/samplers/mod.rs`, `build.rs`, `config/agent.toml`,
   `docs/metrics.md`, the analysis crate's sampler universe
   (`src/analysis/extract/{context,golden}.rs`). *Implemented; the jbd2
   structs are declared as CO-RE flavors `transaction_run_stats_s___rz` and
   `transaction_chp_stats_s___rz`, a fourteenth counter
   (`ext4_journal_lock_buffer_stalls`) was added beside the stall histogram,
   and the tick is 0 when `clock_getres` fails, which keeps the counters and
   empties the jiffy histograms rather than publishing wrong latencies.*
   Bench and close-out numbers below when the Linux job reports.
5. **Dashboard**: an ext4 section in `crates/dashboard/src/dashboard/ext4.rs`
   (commit rate and phase percentiles, checkpoint latency and forced closes,
   fsync rate by op and errors, filesystem errors), gated on
   `ext4_journal_commits` being in the recording. *Done*, in the shared
   `dashboard` crate so both viewer backends get it.
6. **`ext4_alloc`** as phase 2, same shape, its own bench on a write-heavy
   fio run since `ext4_mballoc_alloc` tracks write throughput. *Done; ten
   hooks rather than the six specified, since the gaps entry's C3 metadata
   reads (`ext4_load_inode`, the two bitmap loads) joined it, and
   `ext4_discard_preallocations` came along for eviction churn.*
7. **Phase 3** per-filesystem counters, after the lookup-map measurement.

## Results — phase 1

All on the hv01 guest described under the x86_64 probe (Debian 13,
`6.12.63+deb13-amd64`, `CONFIG_EXT4_FS=m` with module BTF, 56 vCPU, root on
ext4 over virtio `/dev/vda1`), release build, the agent run with only
`ext4_journal` enabled and `[log] level = "debug"`. Final run: systemslab
`01a0e94e-26b9-718e-0865-57164e71bb6a`; branch `ext4-journal-sampler` at
`d678ce84`.

**Build, lint, tests.** `cargo build --release --locked` 388 s cold;
`cargo clippy --all-targets --all-features -- -D warnings` clean;
`cargo test --workspace --exclude viewer --tests --bins --locked`: 1,331
tests pass. One unrelated test failed in this run only —
`agent::exposition::http::stream_tests::a_dropped_stream_is_reconnected_and_both_ends_are_reported`,
a 20 s event deadline that timed out under the full-workspace test load; it
passed in the two previous runs on the same guest and is not touched by this
change.

**Twin selection on a module kernel.** `kernel_btf_has_tracepoints` found all
seven `btf_trace_*` typedefs in `/sys/kernel/btf/ext4` and `/sys/kernel/btf/jbd2`,
so every hook loaded its `tp_btf` twin; libbpf resolved the attach targets and
CO-RE-relocated `transaction_run_stats_s___rz` and `transaction_chp_stats_s___rz`
against the module BTF ("found target candidate ... in [jbd2]"). `rezolus status`:
`ext4_journal active healthy`, seven programs attached, verdict `ok` each.

**Refresh cost** (`ext4_journal sampling latency`, the number principle 16
asks for): **295 µs on the first refresh, then 190–211 µs** per refresh at a
300 ms scrape interval. The read is eight 496-bucket histograms and one
counter map of `MAX_CPUS` × 16 banks (128 KiB) summed in userspace; the
counter sweep walks all 1,024 possible banks regardless of the guest's 56
CPUs, which is the same `Counters` shape `blockio_requests` uses and is the
obvious place to look if this number matters. Not measured: a host with
several ext4 filesystems (host-wide maps, so no scaling expected) and the
worst-case CPU count (the sweep is fixed at `MAX_CPUS`).

**Counts reconcile.** Under `fio --rw=randwrite --bs=4k --numjobs=4 --fsync=1
--ioengine=psync` for 30 s at 616 IOPS, the snapshot after the run read:

| metric | value | check |
|---|---|---|
| `ext4_sync_file{op="fsync"}` | 18,498 | 616 IOPS × 30 s = 18,480; one fsync per write |
| `ext4_sync_file{op="fdatasync"}`, `ext4_sync_file_errors` | 0, 0 | fio calls fsync; none failed |
| `ext4_journal_commits` | 6,339 | 2.9 fsyncs per commit: the four jobs' fsyncs batch into shared commits |
| `ext4_journal_commit_handles` | 15,237 | 2.4 per commit |
| `ext4_journal_commit_blocks{kind="dirtied"}` / `{kind="logged"}` | 6,353 / 19,031 | 1.0 and 3.0 per commit; procfs lifetime averages read 2 and 4 |
| `ext4_journal_checkpoints`; buffers `written` / `dropped` | 6,544; 278 / 6,749 | in-place overwrites leave little to write at checkpoint |
| `ext4_journal_checkpoint_forced_to_close`, `ext4_journal_lock_buffer_stalls`, `ext4_errors`, `ext4_shutdowns` | 0 | expected on a healthy guest |
| every `ext4_journal_commit_latency{phase}` histogram total | 6,339 | one sample per commit in every phase |

`/proc/fs/jbd2/vda1-8/info` went from 600 to 23,652 transactions across the
100 s of fio (prefill, A, B, C); the sampler's 6,339 over its 30 s window is
the right share for the slower B run. The oracle's per-phase averages are
integer jiffies rounded to ms over the journal's lifetime, so they cannot be
compared to a 30 s distribution more finely than "logging is the non-zero
phase", which both agree on.

**Phase distributions, decoded from the h2 buckets** (grouping power 3:
index 159 is one 4 ms tick, 167 two, 171 three, 175 four):

| phase | 0 ticks | 4 ms | 8 ms | 12 ms | 16 ms+ |
|---|---|---|---|---|---|
| logging | 1,721 | 2,912 | 1,552 | 121 | 33 |
| running | 2,202 | 2,661 | 1,373 | 77 | 26 |
| request_delay | 2,457 | 2,591 | 1,204 | 75 | 12 |
| wait | 6,311 | 0 | 4 | 10 | 14 (tail to ~100 ms) |
| flushing, locked | 6,339 | | | | |
| checkpoint | 6,538 | 6 | | | |

Logging carries the commit time on this workload, mean 1.03 ticks (about
4 ms), which matches the oracle's 5.9 ms average commit time within one
tick. Flushing is zero because fio overwrites blocks it already allocated,
so ordered mode has no unwritten data to flush; the design's "flushing and
logging account for most of the commit" holds with flushing's share being
zero here. The jiffy resolution the design accepted is visible: a 4 ms tick
puts most of a 6 ms commit into two buckets. Whether that is enough for the
questions people ask of it is a matter for use; if not, `jbd2_start_commit`
to `jbd2_end_commit` pairing with our own clock is the alternative, at the
price of a per-journal side map.

**Probe cost, first attempt: not measurable on a virtio disk.** The A/B/A fio
sequence on the guest's virtio disk read OFF 851, ON 616, OFF 510 IOPS
(p99 163, 5,866, 161 µs): the two OFF runs differ by 40%, so nothing under
that noise floor is attributable to the sampler.

**Probe cost, measured** (systemslab `01a0e986-e415-717b-ae86-747c23cc7e0a`,
same guest image, `d01988fd` on main). The disk noise is removed by putting
ext4 on a memory-backed `null_blk` device, so fsync completes in microseconds
and the workload is CPU-bound at about 450,000 fsyncs/s: fio randwrite 4 KiB,
8 jobs each pinned to one of vCPUs 8–15, `fsync=1`, 20 s runs, under
`perf stat -C 8-15`; the agent pinned to vCPUs 0–7 and scraped at 1 Hz. Three
arms interleaved four times: no agent, agent with every sampler disabled
(`idle`), agent with `ext4_journal` only (`ext4`).

| arm | fsync/s (mean ± sd) | task-clock ns/fsync | cycles/fsync | instructions/fsync |
|---|---|---|---|---|
| none | 445,847 ± 10,130 | 18,307 ± 449 | 68,381 ± 639 | 33,157 ± 134 |
| idle | 454,658 ± 912 | 17,892 ± 41 | 67,550 ± 216 | 33,107 ± 126 |
| ext4 | 446,289 ± 2,438 | 18,250 ± 132 | 68,948 ± 419 | 33,332 ± 133 |

- **Instructions per fsync is the clean number: +225 ± 130 over `idle`**
  (+0.68%), and +175 over `none`. That is the whole cost of two `tp_btf`
  trampolines, two 14-instruction programs and two per-CPU counter
  increments, per fsync. At 450,000 fsync/s it is about 100 M instructions/s
  across the eight cores, under 1% of one core.
- **Cycles: +1,398 per fsync over `idle` (+2.1%), +567 over `none` (+0.8%).**
  The two baselines disagree by 830 cycles, more than the sampler's own
  span, and `idle` is faster than `none` on every rep by a margin its
  standard deviation does not explain (+2.0% fsync/s). The likely cause is
  the arm order: `none` runs first in each rep, immediately after the
  previous rep's agent teardown, and the first `none` run is the one
  outlier (428 K). Whatever it is, it bounds what this bench can resolve at
  about ±2% on cycles and throughput.
- **Throughput: −1.8% against `idle`, +0.1% against `none`.** The gate was
  "under 1% at saturation"; against one baseline it passes and against the
  other it does not, and the instruction count says the true cost is well
  under 1% of the fsync path on a device where fsync is 18 µs of CPU. On a
  real device fsync is a millisecond-scale wait at a rate three orders of
  magnitude lower, so the fraction there is unmeasurably small. **Ruled GO**,
  with the ±2% baseline disagreement recorded rather than averaged away.
- Reconciliation held on every ext4 arm: `ext4_sync_file{op="fsync"}` was
  99.6–99.7% of fio's write count with the last scrape landing up to a second
  before fio ended; commits 6,150–6,252 per arm, so at this rate jbd2 batches
  about 1,450 fsyncs into one commit. Refresh latency p50 181–211 µs, max
  261 µs, with the agent on its own cores.

The `rezolus_bpf_run_time`/`run_count` pair would give the in-program time
per event directly, excluding dispatch; the JSON snapshot did not carry them
under the sampler's name in this run, so that number is not reported.

**Defects found by running it**, none by reading it:

1. `config` as a map name collides with `typedef struct config_s config` in
   the x86_64 `vmlinux.h`; renamed `jiffy_ns`.
2. `BpfBuilder::map` loads userspace values through mmap, so the map needs
   `BPF_F_MMAPABLE`; without it the sampler thread panicked with EINVAL.
3. `libbpf_rs::btf::Btf::from_path` parses standalone BTF only; a module's
   BTF is split BTF and failed to parse, so the scan found nothing and every
   hook fell back to `raw_tp`. Fixed with `btf__parse_split` through
   libbpf-sys (`RawBtf` in `src/agent/bpf/mod.rs`).
4. The verifier refused `jbd2_run_stats_btf`: a `tp_btf` pointer argument is
   `trusted_ptr_or_null`, and `BPF_CORE_READ`'s offset arithmetic on it is
   prohibited until it is null-checked. `blockio`'s `struct request*` never
   hit this because `block_rq_complete`'s argument is not nullable in BTF.

## Results — phase 2

Same guest as phase 1 (Debian 13, `6.12.63+deb13-amd64`, `CONFIG_EXT4_FS=m`,
56 vCPU, root on ext4 over virtio), release build; systemslab
`01a0eb8c-b8c3-71d4-1dc9-49adb555b59a`. Ten hooks, all `tp_btf` from module
BTF; `struct ext4_allocation_context` CO-RE-relocated against the `ext4`
module ("found target candidate ... in [ext4]").

**Two facts the header did not settle.** `ac_criteria` was a `__u8` (as in
the aarch64 header) until 6.5 made it an `enum criteria`, four bytes, and
inserted a criterion at 2, so the numbering shifted. The field is read with
`BPF_CORE_READ_BITFIELD_PROBED`, which takes the width from the kernel's BTF
rather than the declaration; the trace confirmed it, every allocation on this
6.12 kernel reading 1 where the format prints `CR_GOAL_LEN_FAST`. The
descriptions and `docs/metrics.md` give both numberings. And
`ext4_read_block_bitmap_load` gained a `prefetch` argument in 5.9; the
program takes only the two arguments every kernel passes.

**Verification against tracefs over the same window**, three phases: 20,000
files of 56 KiB written and fsynced (fio, 36 s), caches dropped and every file
`stat`ed, everything deleted:

| metric | sampler | tracefs |
|---|---|---|
| `ext4_allocations`; blocks requested / allocated | 20,104; 279,636 / 279,636 | identical |
| `ext4_allocations_by_criterion` | all 20,104 at `1` | every event `CR_GOAL_LEN_FAST` |
| `ext4_allocation_groups_scanned` | 20,104 (one per allocation) | `grps 1` on every event |
| `ext4_allocation_size` | 19,964 at 14 blocks (56 KiB), 140 at 1 block | |
| `ext4_inodes` allocated / freed | 20,000 / 20,000 | identical |
| `ext4_freed_blocks` | 280,000 | 280,000 |
| `ext4_writepages`; pages written / skipped | 29,508; 280,118 / 0 | identical |
| `ext4_preallocation_discards` | 144,435 | 144,436 |
| `ext4_bitmap_loads` block / inode | 31 / 3 | 31 / 3 |
| `ext4_inode_loads` | 36 | 43 |

The seven-read difference on `ext4_inode_loads` is scrape timing: the last
scrape landed before the deletes' final inode-table reads. The number itself
taught something the design had wrong: 20,000 cold `stat`s cost 36 reads,
not 1,250, because `__ext4_get_inode_loc` reads inode tables in
`inode_readahead_blks` windows (32 blocks by default) and the tracepoint
fires once per read. `ext4_inode_loads` is therefore the rate of synchronous
inode-table *reads*, each serving up to 32 blocks of neighbouring inodes;
the characterization's cold-atime cost is that rate on a table too large and
too randomly accessed for readahead to help, which is exactly when it
matters. `ext4_trimmed_blocks` read 417,996 without a tracefs check; the
guest mounts with online discard and the deletes issued it.

**Refresh cost**: 151–301 µs, median 221 µs (one counter map of `MAX_CPUS`
× 24 banks, one histogram). **Probe cost**: not benched; the allocator hook is
one CO-RE struct read, a few counter increments and one histogram increment
per extent allocation, and allocations run at write-batch rate, three
orders of magnitude below the fsync rate the phase 1 bench measured at
+225 instructions per event.

## Deferred / reopen

- **Probe-cost bench** — Done (GO), in a guest rather than on bare metal:
  `null_blk` removed the disk noise and pinned vCPUs plus `perf stat -C`
  gave the per-event accounting. Reopen only if a bench needs a baseline
  agreement tighter than the ±2% this one had; the fix is randomized arm
  order and a warm-up run per arm.
- **Counter sweep at `MAX_CPUS`** — Idea. The refresh walks 1,024 banks on a
  56-CPU guest; bounding the `Counters` sweep to possible CPUs is a
  `bpf/counters.rs` change that every `Counters` sampler would share.

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
