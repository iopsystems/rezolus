# One Rezolus program per hook

**Status: open. Measured, design decided, nothing built.**

## Goal

Several samplers attach their own program to the same kernel hook. Each
raw-tracepoint program is a callback of its own, and the kernel runs each
one's dispatch (recursion check, RCU and migration bookkeeping, an indirect
call) on every event. Measure what a second program on a hook costs. If it
costs enough, merge the samplers that share a hook so each hook carries one
Rezolus program.

## Where it happens

The `SEC()` attach points of every sampler on `main` at `1906d927`, grouped by
hook:

| hook | samplers | default |
|---|---|---|
| `sched_switch` | `scheduler_runqueue`, `cpu_migrations`, `cpu_perf` | all on (`cpu_perf`'s program only with `cgroup_attribution`) |
| `sys_enter` | `syscall_counts`, `syscall_latency` | both on |
| `block_rq_complete` | `blockio_latency`, `blockio_requests` | both on |
| `ext4_sync_file_enter`, `ext4_sync_file_exit` | `ext4_journal`, `ext4_ops` | `ext4_ops` opt-in |

No other hook is shared.

## Measured

The cost of a program on a hook is the dispatch. An empty program measures
that and nothing else. The test attached k empty `raw_tp` programs to one
hook and measured the benchmark that crosses it.

KVM guest on the CI image, with the image's agent stopped and nothing else
attached; k empty `bpftrace` raw-tracepoint probes; `perf bench syscall basic`;
four alternations (systemslab `01a1003c-cec7-71dd-c16f-7b5a29bfacf6`):

| empty programs on `sys_enter` | ns per syscall, from the mean |
|---|---|
| 0 | 157 |
| 1 | 212 |
| 2 | 276 |
| 4 | 377 |

The first program also turns on the syscall-tracepoint work for every task,
so the step from 1 to 2 is the dispatch alone: about 64 ns, and about 50 ns
per program from 2 to 4.

delta (EPYC 4564P, 6.12.90, bare metal), the host's own agent left running,
which already has the syscall tracepoints on. k empty programs were loaded
with `bpftool prog loadall ... autoattach`, six alternations
(systemslab `01a1004f-5caf-7151-1f90-7e65966cb03b`):

| empty programs | `sys_enter`, ns per syscall (median) | `sched_switch`, µs per pipe round trip (median) |
|---|---|---|
| 0 | 447 | 7.58 |
| 1 | 475 | |
| 2 | 503 | |
| 4 | 599 | 7.81 |

That is 25–40 ns per program per event on bare metal: 28 ns for each of the
first two programs on `sys_enter` and 38 ns each on average up to four, and
at most 29 ns per program per switch on `sched_switch` (230 ns per round
trip for four programs, and a round trip is at least two switches). delta's
syscall numbers are noisy: each condition spans up to about 20% across
passes. In the guest, `sched_switch` did
not separate: `perf bench sched pipe` there is about 18 µs per round trip and
varied more than the effect.

## Decision

Merge the samplers that share a hook into one sampler each, with one BPF
object and one program per hook (decided 2026-10-02):

| new sampler | replaces | programs |
|---|---|---|
| `syscall` | `syscall_counts`, `syscall_latency` | one `sys_enter`, one `sys_exit` |
| `blockio` | `blockio_latency`, `blockio_requests` | one `block_rq_complete`, one `block_rq_requeue` |
| `scheduler` | `scheduler_runqueue`, `cpu_migrations` | one `sched_switch`, the two wakeups |

Considered and not chosen: one shared BPF object per hook loaded by whichever
of the existing samplers is enabled first, keeping the sampler names; and a
dispatcher program per hook that tail-calls each sampler's program.

Left out of this effort:

- **`cpu_perf`.** Its `sched_switch` program exists only for the per-cgroup
  cycles and instructions. The rest of the sampler is per-CPU perf counters
  read from user space. Folding it into `scheduler` would make that sampler
  own PMU events. `sched_switch` goes from three Rezolus programs to two.
- **`ext4_journal` and `ext4_ops`.** `ext4_ops` is opt-in, and fsync is far
  too slow for 25–40 ns to matter.

What follows from merging:

- **Config.** `[samplers.syscall]` and the like are the new sections. Each
  part of a merged sampler keeps its own switch inside the section, so
  `syscall_latency`'s exit program can still be left out. A config that
  still names an old section is translated at load, with a warning, so that
  `[samplers.syscall_latency] enabled = false` keeps its meaning. The config
  accepts unknown sections silently today, so without the translation the
  old sections would stop applying with no warning.
- **Metric labels.** Metric names do not change. The `sampler` label on each
  metric, `rezolus status` and `rezolus_bpf_run_time` change to the new
  names, and so does the `<sampler>/<group>` table key in archives recorded
  afterwards. Readers dispatch by metric name, so recordings from before and
  after read the same.
- **Program bodies.** Each merged program runs the parts that are enabled,
  switched by read-only data the verifier folds, as `cgroup_attribution` is.

## GO criteria

For each family, the merged sampler must produce the same series with the
same values, within the race between two reads, as the samplers it
replaces. Each must be measured per event against main, on bare metal, with
the same benchmark that crosses its hook.

## Plan

1. `syscall`: the hook with the highest event rate and the simplest bodies.
2. `blockio`.
3. `scheduler`.
