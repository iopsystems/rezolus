# One Rezolus program per hook

**Status: in progress. Step 1, `syscall`, built and measured; `blockio` and `scheduler` remain.**

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

