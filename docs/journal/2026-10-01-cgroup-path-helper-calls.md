# The per-cgroup path's cost is its helper calls

**Status: every sampler with a per-event cgroup path converted and measured.
`syscall_counts` first, the other seven after (see "The other samplers").
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
calls per task instead of seven; compiled, each BTF program has three fewer
`bpf_probe_read_kernel()` calls than its twin (six for `scheduler_runqueue`),
and the twins contain no direct loads.

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

`cpu_migrations` takes its cgroup path only on a migration, and `cpu_usage`'s
figure is mostly its softirq programs, which have no cgroup path; neither
moved outside noise. `syscall_counts` was converted in #1392 and is the
control. `cpu_perf` and `xfs_log` did not load on delta (the host's agent
holds the PMU, and delta has no XFS). In a KVM guest with the image's agent
stopped and a loop-mounted XFS, all ten samplers with `cgroup_attribution`
loaded healthy, including `cpu_perf`, `xfs_log` and `cpu_bandwidth`, and each
produced per-cgroup series with values (systemslab
`01a0fd03-ebf4-718d-088c-ee4af495caf1`).

Not run: a kernel from 5.8 to 5.10, where the BTF programs take the
`bpf_get_current_task_btf()` fallback in `current_task_group()` but still use
direct loads from typed arguments in `task_group_of()`, and a kernel without
BTF, where the `raw_tp` and `kprobe` twins load.

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
