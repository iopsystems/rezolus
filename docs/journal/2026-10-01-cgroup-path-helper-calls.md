# The per-cgroup path's cost is its helper calls

**Status: `syscall_counts` converted and measured; the other nine samplers
that include `src/agent/bpf/cgroup.h` remain.**

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

## Remaining

- Convert the other samplers that include `cgroup.h`: `cpu_usage`,
  `cpu_migrations`, `cpu_perf`, `cpu_tlb_flush`, `scheduler_runqueue`,
  `cpu_bandwidth`, `ext4_ops`, `xfs_log` and `memory_pagecache`.
  `scheduler_runqueue` reads both `prev` and `next` from the `sched_switch`
  arguments rather than `current`, so it needs a variant of
  `current_task_group()` that takes a task pointer.

## Contention on the shared per-cgroup counters

Each per-cgroup counter is one u64 that every CPU adds to atomically, and
adjacent css ids share a cache line. `perf bench syscall basic` is
single-threaded, so the numbers above say nothing about many CPUs adding to
one counter. Per-CPU per-cgroup counters would remove the sharing, at
`MAX_CGROUPS` × 8 B × CPUs per series: `syscall_counts` has 17 series, so
544 KiB today would become 17 MiB at 32 CPUs and 102 MiB at 192. Measured
first to see whether that is needed.

Method: N processes, each pinned to its own CPU from CPU 8 up, each calling
`getppid()` in a loop for 5 s. In `same` mode all N are in one cgroup with
the CPU controller (`/rzb/same`, css id 82); in `distinct` mode each has its
own (`/rzb/c0`.., css ids 84 upward, so eight to a cache line). Only
`syscall_counts` enabled, built from main at `f8338afe`, two passes per arm.
delta (EPYC 4564P, 16 cores, SMT on; CPU n and n+16 are siblings), 6.12.90.
systemslab `01a0fb2e-4ed1-719e-7bda-5235ea3bd625` and
`01a0fb3b-29a2-7106-09af-288d22a9d330`.

`sys_enter_btf`, ns per run, two passes, from the second run:

| procs, mode | on | off | on minus off |
|---|---|---|---|
| 1 | 34.0, 33.4 | 25.4, 25.9 | 8–9 |
| 8, same | 33.1, 33.2 | 25.7, 25.3 | 7–8 |
| 16, same | 33.8, 34.0 | 27.3, 25.6 | 7–8 |
| 24, same | 39.2, 37.8 | 29.2, 28.8 | 9–10 |
| 24, distinct | 39.4, 37.8 | 29.1, 28.9 | 9–10 |

The first run gives 8–11 ns at the same points. The attribution cost stays
at 7–11 ns from one process to 24 in one cgroup, so contention on the shared
counter did not show here. The rise of about 4 ns at 24 processes is in the
off arm too: at 24 the processes fill both SMT threads of the cores they run
on, which slows the program whether or not it adds to a cgroup counter.

The host's own Rezolus 5.20 agent ran throughout with two `sys_enter`
programs. Its `syscall_counts` program (5,776 B, the old per-cgroup path)
read 142–169 ns per run in every arm and mode, so it showed no contention
either. Its other program (112 B) went from about 70 ns to 144–186 ns at 16
and 24 processes in `same` mode, and to 96–105 ns at 24 in `distinct` mode.
That program, not either per-cgroup path, is the likely reason `same` mode
had lower total throughput than `distinct` in most arms, including with our
agent stopped. I don't know what it shares between processes of one cgroup.

Not covered: a host with more cores, two sockets, or a different CPU vendor.
On this host per-CPU per-cgroup counters are not needed for
`syscall_counts`.
