# Contention on `process_cpu_usage`: stripes, write combining, a ring buffer

- **Opened:** 2026-10-27
- **Status:** GO — a sharded table of busy processes, assigned by the agent
  (measured 2026-10-31). Stripes, write combining and the ring buffer are
  NO-GO; reopen conditions under "Deferred / reopen".

## Problem

`cpu_usage`'s `process_cpu_usage` (#1557, behind `process_attribution`) is
one `PackedCounters` array of `MAX_PID` (4,194,304) `u64` slots, 32 MiB,
indexed by `pid_slot(tgid)`. Every CPU running one of a process's threads
adds to the same slot from `cpuacct_account_field`, about once per tick, and
ticks fire together on every CPU unless the kernel is booted with
`skew_tick=1`. A process with many busy threads therefore puts every busy CPU
on one cache line once per tick. Principle 7 in `docs/principles.md` records
the cost with one copy.

## Setup

All runs: a 56-vCPU KVM guest on a Zen2 host, 7 L3 domains of 8 vCPUs
(0-7, 8-15, ...), Debian 13, kernel 6.12.63, HZ=250. Workloads with N = 48
spinning threads: one process of 48 threads (1 × 48), and 48 processes of
one thread (48 × 1, the control: no slot is shared). The figure is the mean
ns per run of the `cpu_usage` programs. Each cell holds two passes run in
opposite order, separated by `/`, with two repeats per pass. Run-to-run noise
was 20-40 ns. These runs are separate from the ones principle 7 quotes; the
feature-off baseline here was 164-191 ns against 162-177 there. Correctness:
the exported `process_cpu_usage` delta against `/proc`'s, over reads
bracketing the window. The churn workload is one process with a thread
using about 30% of a CPU and two threads that each create 2 ms threads back
to back, measured over an 8 s window while it runs; `/proc`'s utime and
stime have 10 ms resolution.

## Stripes

`PROCESS_STRIPES` (S) copies of the array laid out stripe-major, the copy picked
per CPU from a table written at load, either `cpu % S` or by L3 domain (a
generalised `assign_cgroup_shards`). The experiment exported stripe 0 only and
checked the sum of the stripes with `bpftool`; every ratio against `/proc`
was 1.000.

| Copies | 1 × 48 | 48 × 1 | Map memory |
|---|---|---|---|
| none (feature off) | 182 178 / 183 189 | 179 179 / 187 182 | 0 |
| 1 (one copy) | 311 303 / 314 307 | 194 202 / 201 194 | 32 MiB |
| 4, `cpu % 4` | 278 265 / 296 300 | 209 210 / 226 227 | 128 MiB |
| 4, by L3 | 249 241 / 219 215 | 197 208 / 184 179 | 128 MiB |
| 8, `cpu % 8` | 259 270 / 261 257 | 191 198 / 177 183 | 256 MiB |
| 8, by L3 | 184 189 / 205 220 | 182 179 / 214 214 | 256 MiB |
| 16, `cpu % 16` | 250 246 / 257 257 | 200 197 / 215 220 | 512 MiB |
| 16, by L3 | 195 190 / 202 191 | 194 196 / 200 196 | 512 MiB |
| 56 (one per CPU) | 220 224 / 195 211 | 204 205 / 203 202 | 1,792 MiB |

Eight copies assigned by L3 remove the contention within noise. The cost is
memory: each copy is indexed by pid, so it costs 32 MiB whatever the number
of live processes. Sizing the arrays from `pid_max` at load saves nothing on
systemd hosts, which set `pid_max` to 4,194,304, and breaks if `pid_max` is
raised later. A copy can shrink only if it is indexed by a key denser than
the pid, which needs an allocator in BPF.

Code: `exp/process-stripes` on brayniac/rezolus,
[`e7af060f`](https://github.com/brayniac/rezolus/commit/e7af060ff8c1fda28d13e0a34bf5fad1d61e0202).
Build with `PROCESS_STRIPES` defined as S in `mod.bpf.c`. For L3
assignment, also run with `REZOLUS_STRIPE_L3=1` and `REZOLUS_STRIPES=S` (the
same S); without them a CPU uses copy `cpu % S`.

## Write combining

A `BPF_MAP_TYPE_PERCPU_ARRAY` of one entry per CPU holds the slot, the
process instance (start time), the summed delta and a tick count. A CPU adds
its delta there and commits it to the shared slot after K ticks, when the CPU
accounts a different process, or when a task exits on that CPU. A commit is
skipped when the slot's start-time marker no longer matches the pending
instance. `PROCESS_COMBINE_TICKS` = 0 is one copy without combining.

| K | 1 × 48 | 48 × 1 | Rate against `/proc` |
|---|---|---|---|
| feature off | 182 187 / 169 171 | 164 183 / 191 180 | |
| 0 (one copy) | 325 325 / 317 324 | 207 198 / 222 223 | 0.9996-1.0000 |
| 4 | 214 215 / 228 223 | 196 194 / 184 201 | 0.9992-1.0001 |
| 8 | 201 203 / 226 231 | 194 195 / 214 211 | 0.9982-1.0002 |

K = 8 matches eight L3 stripes, and K = 4 is within noise of K = 8, with no
extra memory. The churn, exec relabel and removal-after-exit checks passed at
each K. Under churn the ratio against `/proc` was 0.997 at K = 4 and K = 8
and 0.999 at K = 0, one run each.

It was rejected for what it leaves uncounted:

- **Deltas pending on idle CPUs.** A CPU commits only when it accounts a tick
  or a task exits on it. Kernels built with `NO_HZ_IDLE` or `NO_HZ_FULL`,
  which include Debian's, Ubuntu's and RHEL's, stop the tick on an idle CPU,
  so up to K − 1 ticks of a process's CPU stay pending on it until something
  runs there. Measured: 48 threads of one process spinning 4 s and then
  sleeping, `/proc` minus the exported total, two readings seconds apart,
  twice per pass:

  | K | Shortfall (ms) | Bound, (K − 1) × 4 ms × 48 CPUs |
  |---|---|---|
  | 0 | 104, 104, −39, −39; −37, −37, 150, 150 | |
  | 4 | −22, −22, −8, −20; 508, 496, −21, −29 | 576 |
  | 8 | 1163, 1107, 35, 15; 30, 30, 974, 910 | 1,344 |

  The bound assumes each thread stayed on one CPU; a thread that migrated can
  leave an entry on more CPUs. The K = 0 spread is consistent with tick
  accounting: at most one tick per thread (192 ms). A shortfall above that
  spread appeared in one of four K = 4 pairs and two of four K = 8 pairs;
  where it appeared, it shrank by at most 64 ms between readings. Against the
  burst's 192 CPU-seconds it is small (0.26% at K = 4, 0.61% at K = 8), but it
  stays missing while the CPUs are idle, and it is a larger fraction of a
  shorter burst.
- **Loss at exit.** The exit hook commits only the exiting CPU's entry. When a
  process's last thread exits, its deltas pending on other CPUs fail the
  marker check and are dropped. One copy also zeroes the counter at the last
  exit, so it loses what accrued since the agent's last read; combining also
  drops deltas accounted before that read and still pending. This follows from
  the code and was not measured: the churn ratio above is taken while the
  process runs.
- **Nested programs.** The update of the four-field per-CPU entry is not
  atomic, so a program nested on the same CPU can lose a delta. A single-word
  atomic (`xchg`, 5.12, above the 5.8 minimum) would not cover the whole
  entry.

Code: `exp/process-combine` on brayniac/rezolus,
[`6e472c39`](https://github.com/brayniac/rezolus/commit/6e472c39134875300bb797546313f4a7b8209a28).

## Ring buffer (not built)

Sending each delta to userspace as a ring buffer event is an event stream
for measurement that moves the aggregation out of the kernel, against
principles 3 and 6. The rate is one event per busy CPU per tick: about 12k/s
on this guest (48 busy CPUs at HZ=250), 56k/s on 56 CPUs at HZ=1000, 192k/s
on 192 CPUs at HZ=1000, at about 32 bytes each. Every reserve takes the
ring's one spinlock, so it does not remove the contention without per-CPU
combining in front of it, and combining brings back the idle-CPU problem
above unless each CPU is flushed (next section). Its benefit is memory:
keyed by (tgid, start time) in userspace, it would drop the 32 MiB counter
array, and possibly the 32 MiB marker.

## Flushing an idle CPU (not built)

An idle CPU can be flushed by running a program on it: a per-CPU
`PERF_COUNT_SW_CPU_CLOCK` perf event (4.9), `BPF_PROG_RUN` with
`BPF_F_TEST_RUN_ON_CPU` on a raw tracepoint program (5.10, an IPI the agent
sends before each read), or `bpf_timer` pinned to a CPU (6.7). Write combining
needs one of these, and so does a ring buffer fed by it. The IPI's cost was
not measured: 1-10 µs per CPU per refresh is a guess, which at one refresh per
second is 1-10 µs per CPU-second. Combining at K = 4 removes about 100 of the
140 ns per run, about 30 µs per CPU-second at HZ=250 (Decision below). The IPI
cost is paid on every CPU at every refresh; the saving applies only while the
contended workload runs. The IPI adds an interrupt on every CPU, including
`nohz_full` CPUs isolated to avoid interrupts. On those CPUs time is accounted
by vtime rather than the tick, and how often the `cpu_usage` program runs
there was not measured.

## Sharded table of busy processes

A map `process_table` beside `process_cpu_usage` holds counts for the
processes that run on many CPUs at once. It has `CGROUP_SHARDS` (16) copies,
and each CPU writes the copy `cgroup_shard_of_cpu` assigns it from the L3
topology, as `CgroupCounters` does. Each copy is direct-mapped: a bucket per
hash of the tgid, one cache line each, holding the process's start time, its
tgid and its count. `account_process` adds a delta to the CPU's copy when the
bucket holds the task's start time and tgid, and to `process_cpu_usage`
otherwise. A process that holds no entry, or shares its bucket with one that
does, is counted in `process_cpu_usage` as before the table. Nothing is left pending on a CPU, so an idle
CPU has nothing to flush.

### Claimed in BPF

The first version claimed a free bucket in BPF with a compare-and-swap on the
start time, at the process's first add, and freed it at the group's last
exit. Each copy had 4,096 buckets, 4 MiB in all. The harness mapped the table
and added each copy's count to `process_cpu_usage` for the ratio; the agent
did not export it.

| Arm | 1 × 48 | 48 × 1 | Exported against `/proc` |
|---|---|---|---|
| feature off | 174 175 / 197 199 | 168 160 / 188 183 | |
| one copy | 336 357 / 317 313 | 235 250 / 208 177 | 0.9998-1.0003 |
| table | 177 195 / 176 182 | 173 195 / 170 182 | 0.9997-1.0001 |

The table is within noise of the feature off. The churn ratio was 1.000 with
the table and 1.002 with one copy. It was not kept, for three reasons:

- The compare-and-swap is a BPF atomic that kernels before 5.12 reject, and
  rezolus supports 5.8.
- Any process that runs claims a bucket, so idle daemons can hold the bucket
  a busy process hashes to.
- A claim can race a release, and an add can race the last exit; on arm64
  the claim needs acquire and release ordering.

### Assigned by the agent

The agent assigns the buckets (`process_table.rs`). BPF only compares the
start time and tgid and adds, with no new instruction. Each refresh, the
agent:

1. moves every held entry's count into the holder's `process_cpu_usage` slot,
   adding to the slot before subtracting from the entry, and discards it if
   the holder has exited;
2. frees the bucket of a holder that exited, lost its labels, or used less
   than an eighth of a CPU since the last refresh, by zeroing the start time;
3. gives a free bucket to a labelled process that used at least half a CPU
   since the last refresh, writing the tgid in every copy before the start
   time.

A freed bucket is not given out again in the same refresh, so an add that
read the old start time lands before the last move. Refreshes are at least
three quarters of the TTL apart. Only busy processes take buckets, so each
copy has 256, 256 KiB in all; that was measured with one busy process, and
two busy processes in one bucket leave one of them on the contended slot. The export still reads only
`process_cpu_usage`, so a held process's CPU since the last move is exported
after the next refresh, which runs just before the snapshot is built. The
sweep records when the move ran, and the window of the `cpu_usage_process`
group begins no later than that (principle 18).

The agent writes a BPF map while it runs, through a writable mmap. A process
gets its entry one refresh after it becomes busy, so its first refresh of
load goes to the contended slot.

Measured against the version claimed in BPF in one job, three passes in
different orders, two repeats each:

| Arm | 1 × 48 | 48 × 1 |
|---|---|---|
| feature off | 188 185 / 201 216 / 207 210 | 189 167 / 204 208 / 208 205 |
| one copy | 333 342 / 311 309 / 325 334 | 207 219 / 202 180 / 202 217 |
| claimed in BPF | 197 194 / 236 209 / 218 229 | 199 196 / 212 219 / 206 228 |
| assigned | 219 210 / 225 230 / 278 269 | 218 212 / 240 246 / 273 279 |

The assigned arm was higher than the feature off and the claimed version in
every pass of that job, including 48 × 1, where no slot is shared. A second job ran four interleaved rounds of
the claimed version (4,096 buckets) and the assigned version at 256 and
4,096 buckets. The means were 225, 213 and 209 ns for 1 × 48 and 223, 215
and 210 for 48 × 1, against 186 and 189 with the feature off, so the excess
in the first job was variation between passes and the table size does not
matter. Both tables remove the contention. The assigned table leaves 21-27 ns
per run over the feature off and the claimed one 34-39, the same in 48 × 1,
where nothing is shared.

The exported total matched `/proc` at 0.9998-1.0001 over each 1 × 48
window. The churn ratio was 1.006-1.009 in four runs over three jobs, against
1.008 with one copy in one run.
In a 24-vCPU guest on kernel 5.10, one pass, 1 × 16 and 16 × 1 took 218 214
and 216 215 ns per run, and the churn ratio was 0.977. That job ran no
single-copy arm, so whether the table changes the ratio on 5.10 was not
measured.

## Decision

Ship the table assigned by the agent. Before it, `process_cpu_usage` was one copy: on
this guest the contention added about 140 ns to each run of the `cpu_usage`
programs, about 2 ms of CPU per second across 48 busy CPUs at the 15.3k runs
per second principle 7 quotes, or about 45 µs per CPU-second at HZ=250. Runs
scale with HZ, so HZ=1000 would be about four times that. Stripes cost
32 MiB per copy, write combining leaves deltas pending on idle CPUs, and the
table costs 256 KiB and leaves nothing pending.

## Deferred / reopen

- **Measure 1 × N on a two-socket or 100+ CPU host** with the table, to
  confirm it holds where the cost with one copy would be larger.
- **Two entries per bucket** — reopen if busy processes are seen sharing a
  bucket. Each copy's 256 buckets are indexed by a hash of the tgid, so two
  busy processes in one bucket leave one of them on the contended slot.
- **Write combining** — reopen only with a flush that does not depend on
  ticks and an exit path that keeps pending deltas.
- **Stripes** — superseded by the table.
- **Ring buffer** — reopen if the 64 MiB of `process_attribution` maps
  (`process_cpu_usage` and `process_start_times`) becomes the problem, with a
  read-time flush (`BPF_PROG_RUN` with `BPF_F_TEST_RUN_ON_CPU`, 5.10).
