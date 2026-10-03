# The per-cgroup path's cost is its helper calls

**Status: every sampler with a per-event cgroup path converted and measured.
`syscall_counts` first, the other eight after (see "The other samplers").
`cpu_bandwidth` reads a css only on throttle events and keeps its
`bpf_probe_read_kernel()` path.**

## Goal

The per-cgroup path of `syscall_counts` measured 176–188 ns per syscall on
bare metal against 39–42 ns without it (#1391, `docs/metrics.md`, "Per-cgroup
and per-task series"). That run had five samplers enabled; the run below has
`syscall_counts` alone, and main measures 140–151 ns there. `perf bench syscall basic` ran 20% slower with it on.
The question was whether that path could be made cheap enough to keep on by
default, rather than whether to turn it off.

## What the path did

Per syscall, the per-cgroup path in `sys_enter` made seven
`bpf_probe_read_kernel()` calls. Each `BPF_CORE_READ` step is one call:

| read | calls |
|---|---|
| `current->sched_task_group`, for the NULL check | 1 |
| `current->sched_task_group->css.id` | 2 |
| `handle_new_cgroup()`: the id again | 2 |
| `handle_new_cgroup()`: `css.serial_nr` | 2 |

After the reads the path does one array lookup for the stored serial number
and one atomic add. The verifier inlines array lookups. The program
attached to the classic tracepoint `raw_syscalls/sys_enter`, for which the
kernel builds a trace record of the syscall number and six arguments before
the program runs.

## Change

- **`current_task_group()`** in `cgroup.h` returns the task group, its css id
  and serial number, read once. In a BTF-typed program on a kernel with
  `bpf_get_current_task_btf()` (5.11; checked at load with
  `bpf_core_enum_value_exists`), the three reads are direct loads from a BTF
  pointer. The verifier emits those as inline guarded loads, not helper
  calls. Elsewhere it is three `bpf_probe_read_kernel()` calls instead of
  seven.
- **`handle_new_cgroup_read()`** takes the id and serial number from the
  caller. For a known cgroup it does one lookup and a compare. The name reads
  for a new cgroup are unchanged.
- **`sys_enter`** is a `tp_btf/sys_enter` program with a `raw_tp/sys_enter`
  twin for kernels without BTF, selected by `kernel_has_btf()` as
  `scheduler_runqueue` does.

The counter layout and the per-cgroup maps are unchanged. Merging the 17
per-cgroup maps into one array, which was the third proposed step, was not
needed for this result and was not done.

## Measured

Bare metal: AMD EPYC 4564P (Zen 4, 32 CPUs), Debian 13, 6.12.90. Only
`syscall_counts` was enabled. `kernel.bpf_stats_enabled=1`. Main and the
branch were built in the same job and run in alternation. systemslab
`01a0fa91-b7df-7156-64fc-49a20c86a459`.

| build, `cgroup_attribution` | ns per run, pipe phase | ns per run, syscall phase | xlated |
|---|---|---|---|
| main, on | 150, 151 | 143, 140 | 5,824 B |
| branch, on | 38, 39 | 33, 33 | 5,224 B |
| branch, off | 27 | 24 | 424 B |
| main, off | 33 | 31 | 448 B |

The cgroup path went from 109–118 ns per syscall (on minus off, main) to
9–12 ns (branch). The off-path program went from 31–33 ns to 24–27 ns with
nearly the same instructions (448 B and 424 B); I don't know what accounts for
the 6–7 ns. `bpf_stats` times the program only, so the trace record the
classic tracepoint built before the program ran is not in these numbers, and
its cost was not measured.

Correctness: in every on arm, the sum of `cgroup_syscall` equals the sum of
`syscall` to within the events between the two reads (ratio 1.0000).

Throughput was not a usable measure in this run. The host's own Rezolus 5.x
agent ran throughout with two `sys_enter` programs of its own at about 170 and
95 ns per syscall. `perf bench syscall basic` ranged from 1.43 M to 1.73 M
ops/s across arms with no consistent order between the branch and main.

## The other samplers

`task_group_of(task, btf, ...)` in `cgroup.h` is `current_task_group()` for a
given task: direct loads when the pointer is BTF-typed, three
`bpf_probe_read_kernel()` calls otherwise. `current_task_group()` now wraps
it. `handle_new_cgroup(task)`, which read the id and serial number a second
time, is gone; `handle_new_cgroup_from_css()` reads them from the css and
delegates to `handle_new_cgroup_read()`.

| sampler | hook | task pointer |
|---|---|---|
| `scheduler_runqueue` | `sched_switch` | `prev` and `next`, typed in `tp_btf` |
| `cpu_perf` | `sched_switch` | `prev`, typed in `tp_btf` |
| `cpu_migrations` | `sched_switch` | `next`, typed in `tp_btf` |
| `cpu_usage` | `cpuacct_account_field`, `sched_process_exit` | the `fentry` and `tp_btf` argument |
| `cpu_tlb_flush` | `tlb_flush` | current task; a new `tp_btf` program beside the `raw_tp` one |
| `ext4_ops`, `xfs_log`, `memory_pagecache` | their hooks | `bpf_get_current_task_btf()`, which they already used |

Each `raw_tp` or `kprobe` twin passes `btf = false` and makes three helper
calls per task where it made six or seven. The exception is
`sched_process_exit`'s raw twin in `cpu_usage`, which read only the task group
and its id and now also reads the serial number: three where it made two, once
per process exit. Compiled, each BTF program that takes its task as an
argument has three fewer `bpf_probe_read_kernel()` calls than its twin (six
for `scheduler_runqueue`), and the twins contain no direct loads.
`cpu_tlb_flush`'s two programs have the same count, because the BTF one
carries `current_task_group()`'s fallback for kernels before 5.11; the
verifier removes it at load where `bpf_get_current_task_btf()` exists.

Measured on delta with all of these samplers enabled, main and the branch
built in one job and run in alternation, two passes each, under
`perf bench sched pipe`, `perf bench syscall basic`, a 16-thread
mmap/munmap loop, O_DSYNC writes to a loop-mounted ext4 and cold and warm
reads of a 512 MiB file. ns per run is each sampler's `rezolus_bpf_run_time`
over `rezolus_bpf_run_count`, all of its programs together. systemslab
`01a0fcf6-7b9e-715a-6a63-3c0d6d56c249`.

| sampler | main | branch |
|---|---|---|
| `scheduler_runqueue` | 328, 280 | 137, 139 |
| `cpu_tlb_flush` | 146, 162 | 45, 37 |
| `ext4_ops` | 217, 208 | 156, 148 |
| `memory_pagecache` | 271, 207 | 155, 187 |
| `cpu_migrations` | 72, 54 | 55, 54 |
| `cpu_usage` | 100, 108 | 103, 98 |
| `syscall_counts` | 34, 33 | 34, 34 |

`cpu_migrations` takes its cgroup path only on a migration; neither it nor
`cpu_usage` moved outside noise. `cpu_usage`'s figure averages all its
programs, including the softirq ones, which have no cgroup path; I did not
break it down per program. `syscall_counts` was converted in #1392 and is
the control. Nine of these samplers were enabled on delta, whose status
lists eleven with four unsupported. `xfs_log` and `cpu_perf` were among the
unsupported: delta has no XFS, and the host's own agent presumably holds the
PMU. `cpu_bandwidth` was not enabled there. In a KVM guest with the image's agent
stopped and a loop-mounted XFS, all ten enabled samplers loaded healthy,
including `cpu_perf`, `xfs_log` and `cpu_bandwidth`. The nine with
`cgroup_attribution` produced per-cgroup series with values; `cpu_bandwidth`
had none, with no CPU quota set (systemslab
`01a0fd03-ebf4-718d-088c-ee4af495caf1`).

Not run: a kernel from 5.8 to 5.10, where `current_task_group()` falls back
to `bpf_get_current_task()` and three probe reads but `task_group_of()` still
makes direct loads from typed arguments, and a kernel without BTF, where the
`raw_tp` and `kprobe` twins load.

## The task reads outside the cgroup path

The same helper calls read other task fields on every event.
`BTF_READ(btf, ptr, field)` in `src/agent/bpf/btf_read.h` is a plain load
where `btf` is true and `BPF_CORE_READ` otherwise, and
`get_task_state_btf()` in `core_fixes.h` is the same for the task state:

- `scheduler_runqueue`: `pid` in `sched_wakeup` and `sched_wakeup_new`
  (which also read `tgid` and never used it), `pid` of `prev` and `next` and
  `prev`'s state in `sched_switch`.
- `cpu_migrations`: `next`'s `pid`.
- `cpu_usage`: `pid`, `start_time`, `utime` and `stime` on every tick.
  `handle_new_task` takes `pid` and `start_time` from the caller instead of
  reading them again. The exit program reads `pid` directly, and
  `softirq_exit` takes the pid from `bpf_get_current_pid_tgid()`.
- `syscall_latency`: moved from the `raw_syscalls/sys_enter` and `sys_exit`
  classic tracepoints to `tp_btf`/`raw_tp` twins, as `syscall_counts` was in
  #1392. The exit program reads the syscall number from `regs` as x86's and
  arm64's `syscall_get_nr()` do: `orig_ax` truncated to an int, and
  `syscallno`.

Measured on delta with these five samplers enabled, main and the branch
alternated, two passes each, under `perf bench sched pipe`,
`perf bench syscall basic` and 16 CPUs of busy loop; ns per run over each
sampler's programs (systemslab `01a0fde2-f883-71f2-04fa-03929d34218c`):

| sampler | main | branch |
|---|---|---|
| `scheduler_runqueue` | 141.5, 136.1 | 94.5, 91.4 |
| `cpu_migrations` | 58.8, 56.0 | 39.0, 38.0 |
| `syscall_latency` | 72.7, 69.0 | 69.1, 66.9 |
| `cpu_usage` | 100.5, 139.2 | 107.3, 115.7 |
| `syscall_counts` (control) | 34.5, 34.2 | 36.0, 35.2 |

`cpu_usage`'s run counts varied fourfold between passes (0.51 M to 2.06 M),
so its average moves with the mix of its programs and does not separate the
builds. `syscall_latency` recorded 99.99% of the syscalls `syscall_counts`
counted in every pass, on both builds.

### The tracepoint move depends on what else is attached

`syscall_latency`'s programs cost about the same per run either way; the
change is in what the kernel does around them. Throughput with
`syscall_latency` alone enabled, `perf bench syscall basic`, six
alternations each:

| host | none | main (classic) | branch (raw) |
|---|---|---|---|
| delta, the host's 5.20 agent attached | 1.87–2.27 M | 1.78–1.89 M | 1.49–1.71 M |
| KVM guest, no other syscall tracer | 6.32–6.35 M | 1.97–1.99 M | 2.60–2.62 M |

systemslab `01a0fdef-f946-7164-7cd1-07ed97d89014` and
`01a0fdff-07de-7132-0e7b-aec2acad7321`.

With nothing else on the syscall tracepoints, the raw tracepoints cut the
overhead per syscall from about 346 ns to about 226 ns. On delta the branch
was slower in all six pairs, by 30–58 ns per syscall in five and 140 ns in
one; delta's own baseline moved between about 440 and 535 ns per syscall
from pass to pass.

The explanation offered for delta was that its host agent keeps classic
programs on `sys_enter` and `sys_exit`. Classic programs on one trace event
share a single trace-record build and program-array run, so ours would add
little there, while a raw tracepoint registers a callback of its own.

Tested in the KVM guest with the image's agent stopped. `bpftrace` stood in
for another tool, with empty classic programs on both tracepoints. The old
build is b98bfdee, from before #1392, with both syscall samplers on classic
tracepoints. The new build is main after this change, with both on raw
tracepoints. Both had `syscall_counts` and `syscall_latency` enabled, with
`cgroup_attribution = false` on `syscall_counts` so the cgroup path is out of both builds, and
each condition ran five alternations of `perf bench syscall basic`
(systemslab `01a10010-52d0-71c4-2f0f-66a2d2693e5d`). Time per syscall, from
the mean throughput:

| condition | ns per syscall | added by our agent |
|---|---|---|
| nothing attached | 157 | |
| old, classic tracepoints | 542 | 385 |
| new, raw tracepoints | 460 | 304 |
| `bpftrace` only | 374 | |
| `bpftrace` and old | 576 | 202 |
| `bpftrace` and new | 697 | 323 |

The explanation holds. The raw tracepoints cost our agent about the same
with or without the other tracer: 304 and 323 ns. The classic tracepoints
cost 385 ns alone and 202 ns beside `bpftrace`, whose 217 ns of its own
includes the record build and dispatch they then share. With Rezolus as the
only syscall tracer, the move saves about 82 ns per syscall. With another
tool's classic programs on the same tracepoints, it costs about 121 ns more.
The spread across the five passes was under 2.5% in every condition.

Decided 2026-10-02: keep the raw tracepoints, on the assumption that
Rezolus is the only syscall tracer on the host.

## Contention on the shared per-cgroup counters

Each per-cgroup counter is one u64 that every CPU adds to atomically, and up
to eight adjacent css ids share a cache line. `perf bench syscall basic` is
single-threaded, so the numbers above say nothing about many CPUs adding to
one counter. The fix would be the layout of `FilesystemCounters`
(`src/agent/bpf/counters.rs`), with the cgroup in place of the filesystem
slot: one mmapable array of per-CPU banks, each padded to
whole cache lines, indexed `(cpu * MAX_CGROUPS + cgroup) * width + counter`
and summed over CPUs by the reader. For `syscall_counts` the bank is 17
counters padded to 24 (192 B), so each CPU needs 4096 × 192 B = 768 KiB,
against 544 KiB for the whole of today's 17 shared arrays. Sized by
`MAX_CPUS` (1024), as the filesystem banks are, that is 768 MiB allocated
eagerly. The filesystem banks stay at 8–12 MiB because they have 64 slots,
not 4096. Sized to the possible CPUs at load, it is 24 MiB at 32 and 144 MiB
at 192. No map in the repo is sized that way today: it needs
`set_max_entries` before load and a reader that maps fewer than `MAX_CPUS`
banks. The possible-CPU count is the highest possible CPU id plus one, so a
VM that advertises hotplug capacity pays for the CPUs it could have. That
memory was judged too expensive for the default (2026-10-02). This section
measures whether it is needed.

Method: N processes, each pinned to its own CPU from CPU 8 up, each calling
`getppid()` in a loop for 5 s. In `same` mode all N are in one cgroup with
the CPU controller (`/rzb/same`, css id 82). In `distinct` mode each has its
own (`/rzb/c0` upward, css ids 84 upward). Those ids still share cache lines,
up to eight to a line, so `distinct` is not a control with no sharing; the
comparison that carries the result is 1 process against 24. Only
`syscall_counts` was enabled, built from main at `f8338afe`, two passes per
arm. Host: delta (EPYC 4564P, 16 cores, SMT on; CPU n and n+16 are
siblings), 6.12.90. systemslab `01a0fb2e-4ed1-719e-7bda-5235ea3bd625` and
`01a0fb3b-29a2-7106-09af-288d22a9d330`.

The agent published a `/rzb/same` series with css id 82. It does that only
after a task in that cgroup has run the per-cgroup path, which then adds at
that id, so the path resolved the shared cgroup. The value of that counter
was not checked against the benchmark's syscall count.

`sys_enter_btf`, ns per run, two passes, from the second run. The last column
is the range over every on and off pairing:

| procs, mode | on | off | on minus off |
|---|---|---|---|
| 1 | 34.0, 33.4 | 25.4, 25.9 | 7.5–8.6 |
| 8, same | 33.1, 33.2 | 25.7, 25.3 | 7.4–7.9 |
| 16, same | 33.8, 34.0 | 27.3, 25.6 | 6.5–8.4 |
| 24, same | 39.2, 37.8 | 29.2, 28.8 | 8.6–10.4 |
| 24, distinct | 39.4, 37.8 | 29.1, 28.9 | 8.7–10.5 |

The first run gives 7.5–11.0 ns at the same points. Taking the mean of the
two passes, the attribution cost changed from 1 process to 24 by +1.5 ns in
the second run and by −0.7 ns in the first, and within each run it changed by
the same amount in `same` and `distinct` mode. That is within noise. The
program itself got 3–6 ns slower at 24 processes in both arms. At 24, eight
cores (CPUs 8–15 and their siblings 24–31) each run two of the processes,
which likely accounts for that.

The host's own Rezolus 5.20 agent ran throughout with two `sys_enter`
programs. Its `syscall_counts` program (5,776 B, the old per-cgroup path)
read 142–169 ns per run in every arm and mode, so it showed no contention
either. Its other program (112 B), which by its size and the 5.20 source is
the `syscall_latency` entry program, went from about 70 ns to
144–186 ns at 16 and 24 processes in `same` mode, and to 96–105 ns at 24 in
`distinct` mode. That program stores a start timestamp in an array indexed
by thread id, eight ids to a cache line, and has no per-cgroup state. A
likely cause is false sharing between processes with adjacent ids; I did not
check how the ids fell in each mode.

Total throughput varied by up to 23% between passes of the same setup, as
much as most of the gaps between `same` and `distinct`, so it does not
separate the arms. The `syscall_latency` program's extra 70–115 ns may
contribute to `same` mode's lower throughput in most arms.

Not covered: a host with more cores, two sockets, or a different CPU vendor.
On this host per-CPU per-cgroup counters are not needed for
`syscall_counts`.
