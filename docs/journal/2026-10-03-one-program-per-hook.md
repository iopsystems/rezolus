# One Rezolus program per hook

**Status: done. All three steps built and measured: `syscall`, `blockio` and `scheduler`. `cpu_perf`, `tcp_destroy_sock` and the ext4 pair stay apart, as decided.**

## Goal

Several samplers attach their own program to the same kernel hook. Each
raw-tracepoint program is a callback of its own, and the kernel runs each
one's dispatch (recursion check, RCU and migration bookkeeping, an indirect
call) on every event. Measure what a second program on a hook costs. If it
costs enough, merge the samplers that share a hook so each hook carries one
Rezolus program.

## Where it happens

The `SEC()` attach points of every sampler on `main` at `1906d927`, grouped by
hook. A sampler's own `tp_btf`/`raw_tp` or `fentry`/`kprobe` twins load one
of the pair and are not counted:

| hook | samplers | default |
|---|---|---|
| `sched_switch` | `scheduler_runqueue`, `cpu_migrations`, `cpu_perf` | all on (`cpu_perf`'s program only with `cgroup_attribution`, which defaults on) |
| `sys_enter` | `syscall_counts`, `syscall_latency` | both on |
| `block_rq_complete` | `blockio_latency`, `blockio_requests` | both on |
| `tcp_destroy_sock` | `tcp_connect_latency` (classic tracepoint), `tcp_packet_latency` (`raw_tp`) | both on |
| `ext4_sync_file_enter`, `ext4_sync_file_exit` | `ext4_journal`, `ext4_ops` | `ext4_ops` opt-in |

## Measured

Each program on a hook has a fixed cost, its dispatch, on top of its body.
An empty program measures the dispatch alone. The test attached k empty
`raw_tp` programs to one hook and ran the benchmark that crosses it.

KVM guest on the CI image, with the image's agent stopped and nothing else
attached; k empty `bpftrace` raw-tracepoint probes; `perf bench syscall basic`
and `perf bench sched pipe`; four alternations (systemslab
`01a1003c-cec7-71dd-c16f-7b5a29bfacf6`):

| empty programs on `sys_enter` | ns per syscall, from the mean |
|---|---|
| 0 | 157 |
| 1 | 212 |
| 2 | 276 |
| 4 | 377 |

The steps are 55 ns from 0 to 1, 64 ns from 1 to 2 and 50 ns per program
from 2 to 4. Turning the syscall tracepoint on for the first program shows
no separate cost here. `perf bench sched pipe` in the guest runs at
18.4–18.6 µs per round trip whatever the number of programs on
`sched_switch`, and does not separate them.

delta (EPYC 4564P, 6.12.90, bare metal), the host's own agent left running,
which already has the syscall tracepoints on. k empty programs were loaded
with `bpftool prog loadall ... autoattach`, six alternations
(systemslab `01a1004f-5caf-7151-1f90-7e65966cb03b`):

| empty programs | `sys_enter`, ns per syscall (median, range) | `sched_switch`, µs per pipe round trip (median, range) |
|---|---|---|
| 0 | 447 (441–524) | 7.58 (7.49–7.72) |
| 1 | 475 (473–559) | 7.66 (7.57–7.73) |
| 2 | 503 (497–601) | 7.68 (7.65–7.77) |
| 4 | 599 (550–650) | 7.81 (7.76–8.11) |

Only 0 against 4 separates outside the spread of the passes. Over that span
a program costs 38 ns per syscall on `sys_enter`. On `sched_switch` it is at
most 29 ns per program per switch: 230 ns per round trip for four programs,
and a round trip is at least two switches. So on bare metal a program costs
38 ns per syscall on `sys_enter` and at most 29 ns per switch on
`sched_switch`, and 50–64 ns per syscall in the guest.

## Decision

Merge the samplers that share a hook into one sampler each, with one BPF
object and one program per hook. Decided 2026-10-02. Confirmed 2026-10-03
after the consequences below were listed, against keeping the sampler names
with one shared BPF object per hook, which saves the same dispatches without
any of them. The maintainer preferred one sampler per hook family.

| new sampler | replaces | programs |
|---|---|---|
| `syscall` | `syscall_counts`, `syscall_latency` | one `sys_enter`, one `sys_exit` |
| `blockio` | `blockio_latency`, `blockio_requests` | one `block_rq_complete`, one `block_rq_requeue` |
| `scheduler` | `scheduler_runqueue`, `cpu_migrations` | one `sched_switch`, the two wakeups |

Also considered: a dispatcher program per hook that tail-calls each
sampler's program. It keeps the sampler code apart. It was not measured or
prototyped, and was set aside because it keeps a program per sampler on the
hook path, now behind a tail call instead of a dispatch.

Left out of this effort:

- **`cpu_perf`.** Its `sched_switch` program exists only for the per-cgroup
  cycles and instructions. The rest of the sampler is per-CPU perf counters
  read from user space. Folding it into `scheduler` would make that sampler
  own PMU events. `sched_switch` goes from three Rezolus programs to two.
- **`ext4_journal` and `ext4_ops`.** `ext4_ops` is opt-in, and an O_DSYNC
  write on delta's loop-mounted ext4 took 250–450 µs (20,000 writes in 5 and
  8 s, timed to the second; systemslab `01a0fcf6-7b9e-715a-6a63-3c0d6d56c249`),
  against tens of nanoseconds per dispatch.
- **`tcp_destroy_sock`.** It fires once per socket close, and one of its two
  programs is a classic tracepoint, which has a different dispatch path.

## What follows from merging

- **Config.** `[samplers.syscall]` and the like are the new sections. Each
  part of a merged sampler has its own switch in the section (`counts`,
  `latency` and so on), so `syscall_latency`'s exit program can still be left
  out, and its own `cgroup_attribution` where the old sampler had one
  (`syscall_counts`, `scheduler_runqueue` and `cpu_migrations`). A config that still names an
  old section is translated at load, with a warning, so that it keeps its
  meaning:
  - each part's switch is the old sampler's resolved enable (its own
    section, else `[defaults]`, else on), so a missing old section neither
    adds nor drops a part;
  - the merged sampler is enabled when any part is;
  - an old section's `cgroup_attribution` becomes its part's;
  - a new section present alongside an old one wins, and the old one is
    reported as ignored.

  The config accepts unknown sections and unknown keys without a word
  today, so without the translation old sections would stop applying
  silently. The load will also warn on a section that names no known
  sampler.
- **The `sampler` label.** Metric names do not change. The `sampler` label
  comes from the metric's module path (`attribute_sampler` in
  `src/agent/samplers/mod.rs`), so each merged sampler's stats must sit
  under its module: `cpu_migrations`' metrics move from `cpu::linux` to
  `scheduler::linux`, or they resolve to `unattributed`. The static
  `sampler = "..."` metadata in the old `stats.rs` files is updated to the
  new name.
- **What is keyed on the sampler name.** Recordings from before and after
  read the same for anything queried by metric name. Not for what is keyed
  on `sampler`:
  - the Rezolus dashboard's overhead panels (`sum by (sampler)` over
    `rezolus_bpf_run_time`, `crates/dashboard/src/dashboard/rezolus.rs`);
  - `recording filter --samplers`, which matches the `<sampler>/<group>`
    table keys;
  - `rezolus status`, which reports health per sampler. A merged sampler
    whose part fails to load reports degraded as a whole.
- **Overhead attribution.** `rezolus_bpf_run_time` is per sampler, so the
  separate cost of `syscall_counts` and `syscall_latency` is no longer
  visible. Measuring a part's cost means switching the others off.
- **extract-features.** `src/analysis/extract/context.rs` lists the known
  samplers (`EXPECTED_SUBSYSTEMS`, guarded against the registrations by a
  test in `src/agent/samplers/mod.rs`), maps metric names to samplers
  (`METRIC_SAMPLERS`) and lists the BPF samplers. It is static so it can
  read recordings from other versions. It keeps the old names, adds the new
  ones and treats each pair as one subsystem, and its record version is
  bumped, since `subsystems_present` changes for new recordings.
- **Acquisition groups of a part that is off.** A group is bounded to no
  members only when its sampler is not live
  (`bound_groups_without_a_live_sampler`). With `syscall` live and its
  latency part off, the latency group would stay unbounded and every
  snapshot would carry empty members. The merged sampler bounds the groups
  of its disabled parts itself.
- **Program bodies.** Each merged program runs the parts that are enabled,
  switched by read-only data the verifier folds, as `cgroup_attribution` is.
- **Docs.** `config/agent.toml`, `docs/metrics.md`, `site/docs/telemetry.html`,
  the sampler list in `CLAUDE.md` and the CHANGELOG.

## GO criteria

For each family:

- the merged sampler produces the same series with the same values, apart
  from the `sampler` label, within the race between two reads, as the
  samplers it replaces;
- per event, on bare metal, with the benchmark that crosses its hook, the
  merged program costs less than the programs it replaces together, by
  about one dispatch per program removed, outside the spread of the passes;
- the `tp_btf` and `raw_tp` programs pass the verifier on the oldest kernel
  available to test.

## Plan

1. `syscall`: the hook with the highest event rate and the simplest bodies.
   It carries the config translation, the unknown-section warning and the
   extract-features change for all three families.
2. `blockio`.
3. `scheduler`.

## Step 1: `syscall`

`src/agent/samplers/syscall/linux/syscall/`: one BPF object with a
`tp_btf`/`raw_tp` pair on `sys_enter` and on `sys_exit`. `sys_enter` counts
the syscall, attributes it to a cgroup, then takes the latency start stamp.
The stamp comes last so that the latency does not include the counting. On
main the stamp was a separate program, and the attach order decided whether
the counting fell inside it. `sys_exit` records the latency.

The parts are the rodata switches `counts`, `latency` and
`cgroup_attribution`. A part that is off loads no code: with `latency` off
the `sys_exit` programs are not loaded. Its metrics have no values, its
acquisition group is bounded to no members, and its maps are left out of the
object with `set_autocreate(false)`. With `latency` off, that means no 32 MiB
`start` array, as when `syscall_latency` was disabled before.

The config translation, the unknown-section warning and the extract-features
change (`MERGED_SAMPLERS`, record schema version 3) are in the same change.
The translation carries `cgroup_attribution` only from `syscall_counts`; on
`syscall_latency` it did nothing, so it is reported and dropped.

Measured on delta against main (main: `syscall_counts` and `syscall_latency`;
branch: `syscall`; both with the per-cgroup path on). systemslab
`01a10270-7d0f-7130-d983-3b1b369f2faf`, run on the branch before the stamp
moved and the maps were left out. Neither change alters the work per syscall.

| | main | `syscall` |
|---|---|---|
| programs on the syscall path | 3: `sys_enter` 32.6 and 63.9 ns, `sys_exit` 61.6 ns | 2: `sys_enter` 76.8 ns, `sys_exit` 60.7 ns |
| program time per syscall, `bpf_stats` on | 158.1 ns | 137.5 ns |
| `cgroup_syscall` / `syscall` | 1.0000 | 1.0000 |
| latency samples / `syscall` | 0.9998 | 0.9999 |

Those two rows come from one pass each, with `bpf_stats` on, and in that
pass `perf bench syscall basic` ran slower on the branch (1.19 M ops/s
against 1.25 M on main, about 45 ns per syscall). It is a single unpaired
sample, against the six paired passes below.

Program time is what `bpf_stats` times inside the programs. It does not
include the dispatch around each program, and it does include the timing
overhead itself, which is now paid twice per syscall instead of three times.
Throughput with `bpf_stats` off, `perf bench syscall basic`, six
alternations: main 1.41–1.61 M ops/s, `syscall` 1.58–1.68 M. `syscall` was
faster in all six pairs, by 7–27 ns per syscall in five and 80 ns in one
(median 26 ns).

Each part was loaded in a KVM guest (systemslab
`01a10279-8cb4-7129-eafa-b720500f8c70`) and checked against what should be
present:

| config | programs | maps created | series with values |
|---|---|---|---|
| default | `sys_enter`, `sys_exit` | all | `syscall`, `cgroup_syscall`, `syscall_latency` |
| `latency = false` | `sys_enter` | no `start`, no histograms | `syscall`, `cgroup_syscall` |
| `counts = false` | `sys_enter`, `sys_exit` | no `counters`, no cgroup maps | `syscall_latency` |
| `cgroup_attribution = false` | `sys_enter`, `sys_exit` | no cgroup maps | `syscall`, `syscall_latency` |
| old sections, `syscall_latency` off | `sys_enter` | no `start`, no histograms | `syscall`, `cgroup_syscall` |

The maps column is inferred from four maps checked by name: `start`,
`read_latency`, `counters` and `cgroup_info`. Every case loaded healthy, with
`sampler="syscall"` on its series. The old
sections were read with the two deprecation warnings, and a
`[samplers.not_a_sampler]` section was reported.

Against the GO criteria:

- Same series, apart from the label, and the counts and latency samples are
  as consistent with each other as on main. The two builds ran one after
  the other, so their values were not compared one for one.
- Cheaper: faster in all six paired passes, by a median of 26 ns per
  syscall, against the 38 ns per empty program measured on `sys_enter`. The
  ranges of the passes overlap (1.58–1.61 M ops/s is in both), so the
  criterion's "outside the spread of the passes" is not met by the ranges;
  the paired wins are the evidence.
- Verifier on the oldest kernel: only 6.12 was run, and only the `tp_btf`
  programs.

## Step 2: `blockio`

`src/agent/samplers/blockio/linux/blockio/`: one BPF object with a
`tp_btf`/`raw_tp` pair on `block_rq_complete` and on `block_rq_requeue`. The
completion handler takes the timestamp first, reads the request's
`cmd_flags` once, counts the request (ops, bytes, size, errors), then records
the latency phases from the kernel's per-request timestamps. In the `tp_btf`
program the request's fields are direct loads (`BTF_READ`); on main each of
the two programs read them through `BPF_CORE_READ`, `blockio_latency` four
fields and `blockio_requests` one. The parts are the switches `requests` and
`latency`. With `requests` off the requeue programs are not loaded, and a
part that is off leaves its maps out, as in step 1. Neither old sampler had
a per-cgroup path.

Measured on delta against main, three alternating passes of 200,000 direct
4 KiB writes and reads on a loop device backed by tmpfs (systemslab
`01a10297-7c33-712d-d81b-d09c124c998a`):

| | main | `blockio` |
|---|---|---|
| programs on `block_rq_complete` | 2: 144.9–147.0 ns and 55.7–57.3 ns | 1: 97.0–99.2 ns |
| program time per completion | 201.6–204.3 ns | 97.0–99.2 ns |
| total, device and queue latency samples / operations | 1.0000, 0.9999, 0.9999 | 1.0000, 0.9999, 0.9999 |
| size samples / operations | 0.9998–0.9999 | 0.9998–0.9999 |

The program time halved. The reads are the likely reason: the two programs
on main made five `bpf_probe_read_kernel()` calls per completion between
them, and the merged `tp_btf` program makes none. Both old programs are
named `block_rq_complete_btf`, so the run does not say which of the two
costs was which sampler's. The dispatch saved, one per completion, is on top and is not in these
numbers. A block IO costs microseconds, so neither saving was visible in the
IO rate, which this run timed only to the second.

Each part was loaded on delta. delta's own agent has programs and maps with
the same names, so the table gives what this agent added:

| config | programs added | maps checked | series with values |
|---|---|---|---|
| default | `block_rq_complete`, `block_rq_requeue` | `counters`, `requeues`, two latency histograms | the three latency phases, `blockio_bytes`, `blockio_operations`, `blockio_size` |
| `latency = false` | `block_rq_complete`, `block_rq_requeue` | no latency histograms | `blockio_bytes`, `blockio_operations`, `blockio_size` |
| `requests = false` | `block_rq_complete` | no `counters`, no `requeues` | the three latency phases |
| old sections, `blockio_latency` off | `block_rq_complete`, `block_rq_requeue` | no latency histograms | `blockio_bytes`, `blockio_operations`, `blockio_size` |

The maps column checks four maps by name: `counters`, `requeues`, and two
of the twelve latency histograms. Every case loaded healthy with `sampler="blockio"`, and the old sections were
read with the two deprecation warnings. No errors or requeues occurred in
the run, so `blockio_errors` and `blockio_requeues` had no values in any
case.

Against the GO criteria: same series, with the latency and size samples in
the same proportion to operations as on main; the program time per
completion halved; and only 6.12 and the `tp_btf` programs were run.

## Step 3: `scheduler`

`src/agent/samplers/scheduler/linux/scheduler/`: one BPF object with a
`tp_btf`/`raw_tp` pair on `sched_switch` and on each wakeup. The switch
program runs the runqueue part, which returns the next task's cgroup id, and
then the migrations part, which reuses that id. With the runqueue part off,
the migrations part resolves the id itself, and only on a migration, as
`cpu_migrations` did. One `resolve_cgroup()` reads the task group, checks for
a new cgroup and zeroes the per-cgroup counters of each part that is on. The
two old samplers each kept their own serial map; now there is one, and every
path that sees a new serial zeroes every enabled part. The migration counter
map is `migrations_counts`, since `migrations` is the part's switch.

The parts are `runqueue` and `migrations`. A part that is off leaves out its
maps: `last_cpu` (16 MiB) for migrations, the three per-pid stamp arrays
(32 MiB each) for the runqueue. Its groups are bounded, and the wakeup
programs load only with the runqueue part. Each combination of parts has its
own cgroup identity and ringbuf handler, so a new cgroup's labels go only to
metrics that are backed. The migration groups are renamed
`scheduler_migrations` and `scheduler_cgroup_migrations`, because a group's
name starts with its sampler's. `cpu_perf` keeps its own program on
`sched_switch`, so the hook goes from three Rezolus programs to two.

Both old samplers had a `cgroup_attribution`, and the merged sampler has one.
The translation takes each enabled part's value as its old sampler resolved
it (its own section, else `[defaults]`, else on). If they differ the merged
value is on, so no exported series disappears, and the difference is
reported. The first version compared only explicit values, and the review
found that `[samplers.cpu_migrations] cgroup_attribution = false`, the old
packaged config's commented example, would then have dropped the runqueue
part's per-cgroup series with no warning.

Measured on delta against main, the `sched_switch` program time per switch
(`bpf_stats`), five alternations of `perf bench sched pipe -l 500000` pinned
to two CPUs (systemslab `01a102c3-1b62-71ee-0b50-fa8e6c935170`):

| pipe CPUs | main (2 programs) | `scheduler` (1 program) | `scheduler` cheaper in |
|---|---|---|---|
| 8 and 9, separate cores | 177.2–188.8 ns | 163.1–174.5 ns | 5 of 5 pairs, by 13–20 ns |
| 8 and 24, one core's two threads | 176.9–186.1 ns | 163.3–180.1 ns | 5 of 5 pairs, by 3–18 ns |

On separate cores the pipe throughput was also higher in all five pairs, by
0.4–1.3%. That is about 20 ns per switch, which includes the saved dispatch
that `bpf_stats` does not time. Unpinned (CPUs 8–31), passes alternated
between about 1.1 M and 2.2 M switches as the scheduler placed the two tasks,
and the per-switch figures do not compare.

The first run, unpinned, checked the counts and loaded each part
(systemslab `01a102b6-0ea5-71f3-857a-c46e93993eb5`):

| | main | `scheduler` |
|---|---|---|
| `cgroup_scheduler_context_switch` / `scheduler_context_switch` | 1.0000 | 1.0000 |
| migrations from / to | 1.0000 | 1.0000 |
| `cgroup_cpu_migrations` / migrations | 1.0000 | 1.0000–1.0005 |

| config | wakeup programs added | maps checked | series with values |
|---|---|---|---|
| default | both | `last_cpu`, `migrations_counts`, `enqueued_at`, `runqlat`, `cgroup_info` | all ten |
| `runqueue = false` | none | no `enqueued_at`, no `runqlat` | `cpu_migrations`, `cgroup_cpu_migrations` |
| `migrations = false` | both | no `last_cpu`, no `migrations_counts` | the eight runqueue series |
| `cgroup_attribution = false` | both | no `cgroup_info` | the six host-level series |
| old sections, runqueue off | none | no `enqueued_at`, no `runqlat` | `cpu_migrations`, `cgroup_cpu_migrations` |

delta's own agent has programs and maps of the same names, so the table gives
what this agent added. Every case loaded healthy with `sampler="scheduler"`.
The review loaded all twelve combinations of the switches and twins in a
verifier on a 7.0 kernel; on delta only 6.12 and the `tp_btf` programs ran.

Against the GO criteria: same series, with the per-cgroup and direction
ratios as on main; cheaper in every pinned pair; and only 6.12 and 7.0, not
the oldest kernel.

## Outcome

| hook | Rezolus programs before | after | measured on delta |
|---|---|---|---|
| `sys_enter` | 2 | 1 | median 26 ns per syscall cheaper in throughput |
| `block_rq_complete` | 2 | 1 | program time per completion 202 to 98 ns |
| `sched_switch` | 3 | 2 (`cpu_perf` stays) | 13–20 ns per switch cheaper in program time |

