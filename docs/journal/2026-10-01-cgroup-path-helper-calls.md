# The per-cgroup path's cost is its helper calls

**Status: `syscall_counts` converted and measured; the other nine samplers
that include `src/agent/bpf/cgroup.h` remain.**

## Goal

The per-cgroup path of `syscall_counts` measured 176–188 ns per syscall on
bare metal against 39–42 ns without it (#1391, `docs/metrics.md`, "Per-cgroup
and per-task series"). `perf bench syscall basic` ran 20% slower with it on.
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

The cgroup path went from about 110 ns per syscall to about 9 ns. The
off-path program went from 31–33 ns to 24–27 ns. `bpf_stats` times the
program only, so the trace record the classic tracepoint built before the
program ran is not in these numbers. Its saving is real but not measured
here.

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
- Contention on the shared per-cgroup counters is not measured.
  `perf bench syscall basic` is single-threaded. A multi-threaded service in
  one cgroup adds to the same cache line from every CPU.
