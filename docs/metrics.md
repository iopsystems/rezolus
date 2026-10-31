# Rezolus Metrics Documentation

Rezolus is a Linux performance telemetry agent that provides detailed insights
into system behavior through efficient, low-overhead instrumentation.

This guide walks you through all the available metrics, organized by category.

## Per-cgroup and per-task series

The `cgroup_*` series of the BPF samplers come from a path in each sampler's
hook that reads the task's cgroup, checks whether the cgroup is new, and adds
to a per-cgroup counter. That path runs on every event the hook sees, so it
is a share of the sampler's cost, and the config option `cgroup_attribution`
removes it. With `cgroup_attribution = false` in a sampler's section (or in
`[defaults]`, which reaches every sampler that has the option), the path is
folded out of the loaded program, the `cgroup_*` series are absent, and the
host-level series are unchanged.

| Sampler | `cgroup_attribution` default | Series it controls |
|---|---|---|
| `cpu_usage` | on | `cgroup_cpu_usage`, `cgroup_cpu_usage_exited_tasks` |
| `cpu_perf` | on | `cgroup_cpu_cycles`, `cgroup_cpu_instructions` (off also drops the `sched_switch` program) |
| `cpu_tlb_flush` | on | `cgroup_cpu_tlb_flush` |
| `scheduler` | on | `cgroup_scheduler_runqueue_wait`, `cgroup_scheduler_offcpu`, `cgroup_scheduler_uninterruptible_wait`, `cgroup_scheduler_context_switch`, `cgroup_cpu_migrations` |
| `syscall` | on | `cgroup_syscall` |
| `ext4_ops` | off | `cgroup_ext4_ops`, `cgroup_ext4_op_time` |
| `xfs_log` | off | `cgroup_xfs_log_waits`, `cgroup_xfs_log_wait_time` |
| `memory_pagecache` | off | `cgroup_pagecache_*` |

`cpu_bandwidth` has no option: all of its series are per cgroup.

What the path costs, per run of each hook, measured with
`kernel.bpf_stats_enabled` under `perf bench sched pipe`, `perf bench syscall
basic` and a multi-threaded mmap/munmap loop, on bare metal (AMD EPYC 4564P,
Zen 4, Debian 13, 6.12) and in a 56-vCPU KVM guest (Zen 2 host):

| Sampler, hook | Bare metal on | Bare metal off | Guest on | Guest off |
|---|---|---|---|---|
| `syscall_counts`, `sys_enter` (before #1392) | 176–188 ns | 39–42 ns | 250–291 ns | 62–75 ns |
| `cpu_tlb_flush`, `tlb_flush` | 180–197 ns | 36–39 ns | 293–352 ns | 72–76 ns |
| `scheduler_runqueue`, `sched_switch` | 491–575 ns | 213–256 ns | 629–775 ns | 240–349 ns |
| `cpu_usage`, `cpuacct_account_field` | 502–599 ns | 285–351 ns | 416–776 ns | 238–518 ns |
| `cpu_migrations`, `sched_switch` | 77–98 ns | 77–90 ns | 106–126 ns | 107–132 ns |
| `ext4_ops`, fsync and write end hooks | | | 531–537 ns | 266–271 ns |

`cpu_migrations`'s cgroup path runs only on a migration, so it costs nothing
per switch. On bare metal, before #1392, `perf bench syscall basic` ran at
715 K ops/s with the five samplers' cgroup paths off, the same as without
these samplers (711 K), and at 564–582 K with them on.

Since #1392, `syscall_counts` (now part of `syscall`) reads the task group once, as direct loads from
a BTF task pointer, from a `tp_btf` program, and the other samplers in the
table do the same since #1397. The table above is the cost
before either. Measured on the same bare-metal host with `syscall_counts`
alone enabled, the two builds run in alternation, under
`perf bench sched pipe` and `perf bench syscall basic`:

| `syscall_counts`, `sys_enter` | On | Off |
|---|---|---|
| before #1392 | 140–151 ns | 31–33 ns |
| since #1392 | 33–39 ns | 24–27 ns |

For the other samplers, with all of them enabled and each sampler's programs
averaged together (`rezolus_bpf_run_time` over `rezolus_bpf_run_count`), two
passes each:

| Sampler | Before | After |
|---|---|---|
| `scheduler_runqueue` | 280–328 ns | 137–139 ns |
| `cpu_tlb_flush` | 146–162 ns | 37–45 ns |
| `ext4_ops` | 208–217 ns | 148–156 ns |
| `memory_pagecache` | 207–271 ns | 155–187 ns |

`cpu_migrations` and `cpu_usage` did not change outside noise. The first
reaches its cgroup path only on a migration; the second's average includes
its softirq programs, which have no cgroup path.

A task is attributed to its CPU controller's task group. A service whose
cgroup has no CPU controller of its own is counted under the nearest
ancestor that has one, usually its slice.

`task_attribution` and `process_attribution` are the per-task and
per-process counterparts, for `cpu_usage` only, off by default
(`process_attribution` follows `task_attribution` unless set): see
[cpu_usage](#cpu_usage).

## Table of Contents

- [Per-cgroup and per-task series](#per-cgroup-and-per-task-series)
- [Block I/O](#block-io)
  - [blockio](#blockio)
- [CPU](#cpu)
  - [cpu_bandwidth](#cpu_bandwidth)
  - [cpu_cores](#cpu_cores)
  - [cpu_frequency](#cpu_frequency)
  - [cpu_l3](#cpu_l3)
  - [cpu_perf](#cpu_perf)
  - [cpu_power](#cpu_power)
  - [cpu_tlb_flush](#cpu_tlb_flush)
  - [cpu_usage](#cpu_usage)
- [Drive](#drive)
  - [drivehealth](#drivehealth)
- [ext4](#ext4)
  - [ext4_alloc](#ext4_alloc)
  - [ext4_journal](#ext4_journal)
  - [ext4_ops](#ext4_ops)
- [Filesystem](#filesystem)
  - [filesystem](#filesystem-1)
- [XFS](#xfs)
  - [xfs_stats](#xfs_stats)
  - [xfs_log](#xfs_log)
- [GPU](#gpu)
  - [gpu_nvidia](#gpu_nvidia)
  - [gpu_intel_pmu](#gpu_intel_pmu)
- [Memory](#memory)
  - [memory_meminfo](#memory_meminfo)
  - [memory_pagecache](#memory_pagecache)
  - [memory_slabinfo](#memory_slabinfo)
  - [memory_vmstat](#memory_vmstat)
  - [memory_writeback](#memory_writeback)
- [Network](#network)
  - [network_interfaces](#network_interfaces)
  - [network_traffic](#network_traffic)
- [Scheduler](#scheduler)
  - [scheduler](#scheduler-1)
- [Hardware Sensors](#hardware-sensors)
- [Syscall](#syscall)
  - [syscall](#syscall-1)
- [TCP](#tcp)
  - [tcp_connect_latency](#tcp_connect_latency)
  - [tcp_mib](#tcp_mib)
  - [tcp_packet_latency](#tcp_packet_latency)
  - [tcp_receive](#tcp_receive)
  - [tcp_retransmit](#tcp_retransmit)
  - [tcp_traffic](#tcp_traffic)
- [Rezolus](#rezolus)
  - [rezolus_rusage](#rezolus_rusage)

## Block I/O

Samplers for measuring how disk and storage devices are performing.

### blockio

This sampler traces block I/O request completion and requeue, with one
program on each. It replaced the `blockio_requests` and `blockio_latency`
samplers, which each had a program on `block_rq_complete` (see
docs/journal/2026-10-03-one-program-per-hook.md); a config that still names
them is read as `[samplers.blockio]`, with a warning. Its two parts are the
options `requests` and `latency`, both on by default.

The latency part measures the distribution of each phase of a request's life:

| Metric | Description | Metadata |
|--------|-------------|----------|
| `blockio_device_latency` | Distribution of block IO device service latency in nanoseconds, from the moment the device began servicing the request until it completed. Recordings made before this metric was renamed carry the same measurement as `blockio_latency` | `op={read,write,flush,discard}` |
| `blockio_queue_latency` | Distribution of time requests spent queued before the device began servicing them, in nanoseconds. This is the component that grows under saturation, where device latency alone stays flat. A request that goes straight to the driver has no queue phase and records no sample, so on such a device the histogram is present but empty | `op={read,write,flush,discard}` |
| `blockio_total_latency` | Distribution of end-to-end latency in nanoseconds, from the request entering the queue until it completed — queue and device together. Measured directly rather than summed, because two histograms cannot be added | `op={read,write,flush,discard}` |

The requests part counts requests and bytes by request type and records the
size distribution. These metrics help monitor I/O throughput and understand
the characteristics of disk access patterns. This information is useful for
storage system tuning, application optimization, and capacity planning.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `blockio_size` | Distribution of blockio operation sizes | `op={read,write,flush,discard}` |
| `blockio_operations` | The number of completed operations for block devices | `op={read,write,flush,discard}` |
| `blockio_bytes` | The number of bytes transferred for block devices | `op={read,write,flush,discard}` |

On macOS there is no request trace. The sampler reads each
`IOBlockStorageDriver`'s cumulative statistics from the I/O registry (the
counters `iostat` reads), unprivileged, and sums them over every driver,
disk images included. It publishes `blockio_operations` and `blockio_bytes`
for reads and writes, and the macOS-only counters below. Nothing writes the
latency histograms, `blockio_size`, `blockio_errors` or `blockio_requeues`
there; the histograms appear with no value. Setting `requests = false` turns
the macOS sampler off; `latency` has no effect.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `blockio_retries` | Retries the storage driver performed | `op={read,write}` |
| `blockio_service_time` | Nanoseconds the driver reports spending on IO ("Total Time") | `op={read,write}` |
| `blockio_driver_errors` | Errors the driver reports, unclassified; `IOBlockStorageDriver.h` does not say whether a request that succeeded on retry is counted | `op={read,write}` |

## CPU

Metrics related to CPU performance and usage. These metrics provide insight for
understanding CPU utilization and identifying performance issues.

### cpu_bandwidth

Instruments CPU bandwidth quotas and throttling in container environments
(cgroups).

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cgroup_cpu_bandwidth_period` | The duration of the CFS bandwidth period in nanoseconds | `name`: the name of the cgroup |
| `cgroup_cpu_bandwidth_quota` | The CPU bandwidth quota assigned to the cgroup in nanoseconds | `name`: the name of the cgroup |
| `cgroup_cpu_throttled_time` | The total time a cgroup has been throttled by the CPU controller | `name`: the name of the cgroup |
| `cgroup_cpu_throttled` | The number of times a cgroup has been throttled by the CPU controller |  `name`: the name of the cgroup |

### cpu_cores

Tracks the number of online CPU cores. This metric is primarily used for
normalizing other CPU metrics.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_cores` | The total number of logical cores that are currently online | |

### cpu_frequency

Gets CPU frequency data from special CPU registers (MSRs). This lets you check
if the CPU is running at the speed you'd expect, or if it's being throttled due to heat or power limits.

To figure out the running CPU frequency, use this formula:
```
Running Frequency = rate(TSC) * (rate(APERF) / rate(MPERF))
```

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_aperf` | APERF register value | |
| `cpu_mperf` | MPERF register value | |
| `cpu_tsc` | TSC register value | |

On macOS, see [cpu_frequency on macOS](#cpu_frequency-on-macos).

#### cpu_frequency on macOS

Apple silicon has no APERF or MPERF, so there the sampler reads IOReport's
`CPU Core Performance States` channels, which count, per core, the ticks of
the 24 MHz system counter spent in each performance state, idle and powered
down. The device tree lists each cluster type's state frequencies
(`voltage-states1-sram` for the E cores, `voltage-states5-sram` for the P
cores). The sampler publishes the same three counters as on Linux, per
logical CPU, so the formula above gives each core's average frequency while
running:

- `cpu_tsc` is the system counter (`CLOCK_UPTIME_RAW` in 24 MHz ticks), so
  its rate is 24 MHz on every core.
- `cpu_mperf` is the ticks spent in any performance state.
- `cpu_aperf` is the ticks in each performance state times that state's
  frequency over 24 MHz.

`cpu_aperf` is not a count of cycles. IOReport brings a core's
residencies up to date when the core changes state, so between two reads
`cpu_mperf` can advance by more or less than the core ran in that interval,
by up to a second on a P core. `cpu_aperf` lags with it, so their ratio is
the frequency over the time counted.

IOReport is a private framework with no published interface; it is the one
`powermetrics` reads, and it needs no privileges. A cluster type whose
frequency table does not have one entry per performance state is not
published, and the sampler reports itself unsupported when no core can be
read. An Intel Mac, whose CPUs have no `cluster-type`, reports it unsupported.

### cpu_l3

Tracks L3 cache misses and accesses. A high miss rate might mean inefficient
memory access patterns or programs competing for cache space.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_l3_access` | The number of L3 cache access | |
| `cpu_l3_miss` | The number of L3 cache miss | |

### cpu_perf

Uses CPU performance counters to track cycles and completed instructions. This
allows for calculating Instructions Per Cycle (IPC), which shows how efficiently
your CPU is running code.

To calculate IPC:
```
IPC = Instructions / Cycles
```

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_cycles` | The number of elapsed CPU cycles | |
| `cpu_instructions` | The number of instructions retired | |
| `cgroup_cpu_cycles` | The number of elapsed CPU cycles on a per-cgroup basis | name: the name of the cgroup |
| `cgroup_cpu_instructions` | The number of instructions retired on a per-cgroup basis | name: the name of the cgroup |

The per-cgroup series come from a program on `sched_switch` that reads both
counters at every context switch. On bare metal a counter read costs tens of
nanoseconds; in a virtual machine whose PMU the hypervisor emulates, each
read is a VM exit, measured at 1-11 µs on KVM and 20 µs per context switch
for the pair. Set `cgroup_attribution = false` in `[samplers.cpu_perf]` (or
in `[defaults]`, which also reaches the other samplers that have the option)
to leave the program unloaded: `cpu_cycles` and `cpu_instructions` are still
read per CPU at scrape time, and the `cgroup_cpu_*` series are absent. On by
default for this sampler.

### cpu_power

Reports CPU energy and idle-state residency from the perf PMUs the kernel
exposes for them. Everything is read through perf; the sampler needs
`CAP_PERFMON` but not `CAP_SYS_RAWIO`, and does not open `/dev/cpu/*/msr`.
On macOS, see [cpu_power on macOS](#cpu_power-on-macos).

Four PMUs are consulted, each optional and each discovered at runtime from
`/sys/bus/event_source/devices`:

| PMU | provides |
|-----|----------|
| `power` | package-scope RAPL energy (`energy-pkg`, `energy-cores`, `energy-gpu`, `energy-ram`, `energy-psys`) |
| `power_core` | per-core RAPL energy (`energy-core`) |
| `cstate_core` | per-core idle residency (`cN-residency`) |
| `cstate_pkg` | package idle residency (`cN-residency`) |

There is no CPU vendor detection. Which PMUs exist, which events they expose,
and which CPUs may read them are all published by the kernel, and they vary by
part as much as by vendor. Only what the hardware actually implements is
reported, so the metric set differs between machines.

Each PMU's `cpumask` states its counters' scope: a package-scope PMU names one
CPU per package, a core-scope PMU one CPU per physical core with SMT siblings
excluded. Core-scope metrics are indexed by that CPU id, so the id identifies a
core — on hybrid parts this distinguishes P-cores from E-cores, which idle in
different states. Package-scope metrics are indexed by the package's ordinal
position in the mask instead, since the mask's CPU ids are sparse (`0,64` on a
two-socket host) while the ordinal is dense.

Energy is the metric worth keeping: it is a monotonic counter that can be
aggregated over any window, and power can always be recomputed from it.

C-state residency is reported as raw TSC cycles rather than a percentage, so a
residency fraction is `rate(core_c6_residency) / rate(cpu_tsc)` (`cpu_tsc` comes
from the `cpu_frequency` sampler). Reporting cycles keeps the ratio exact over
any window. `core_cstate_residency` sums every core level the part exposes, so
total idle residency is available without needing to know which levels this
particular part implements.

Note that `package` energy excludes DRAM, so it is not a whole-system figure.
These PMUs are typically unavailable in virtualized environments, where the
sampler reports itself unsupported rather than disabled -- nobody turned it
off, the hardware simply is not there.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_package_energy` | Cumulative package energy, including cores, uncore, and integrated graphics | id: package ordinal; unit: `microjoules` |
| `cpu_cores_energy` | Cumulative energy for all cores in the package (Intel PP0) | id: package ordinal; unit: `microjoules` |
| `cpu_core_energy` | Cumulative energy for a single physical core (AMD) | id: the CPU reading that core; unit: `microjoules` |
| `cpu_igpu_energy` | Cumulative integrated graphics energy | id: package ordinal; unit: `microjoules` |
| `cpu_dram_energy` | Cumulative energy for the DRAM attached to the package | id: package ordinal; unit: `microjoules` |
| `cpu_platform_energy` | Cumulative whole-platform energy (PSys) | id: always `0`; unit: `microjoules` |
| `core_c1_residency` .. `core_c10_residency` | Cumulative TSC cycles a physical core spent in that idle state | id: the CPU reading that core; unit: `cycles` |
| `core_cstate_residency` | Cumulative TSC cycles a core spent in any idle state, summed across every level exposed | id: the CPU reading that core; unit: `cycles` |
| `package_c1_residency` .. `package_c10_residency` | Cumulative TSC cycles a package spent in that idle state | id: package ordinal; unit: `cycles` |

Only the domains and C-state levels the hardware implements appear; the rest are
absent rather than zero. `cpu_cores_energy` (Intel, pre-aggregated per package)
and `cpu_core_energy` (AMD, per core) measure the same physical quantity at
different granularities — summing the latter across its ids gives the former.

Only energy is reported; there is no power metric. Power is the derivative and
is computed at query time:

```
irate(cpu_package_energy[5m]) / 1000000
```

The counters are microjoules, so `irate` yields uJ/s (= uW) and the divisor
converts to watts. This is correct over any window, which a sampled gauge is
not: a gauge could only describe the sampler's own refresh interval, so a scrape
landing mid-interval would read a stale value, and at integer-milliwatt
resolution a domain drawing microwatts (an idle iGPU) would read `0` while its
energy counter still advanced.

#### cpu_power on macOS

On Apple silicon the sampler reads IOReport's `Energy Model` channels, which
count the energy each block of the SoC has used since boot. On an Intel Mac it
reports itself unsupported. It needs no
privileges; IOReport is the private framework `powermetrics` reads. Each
channel is counted in mJ, uJ or nJ and converted to microjoules.

| Metric | IOReport channel |
|--------|------------------|
| `cpu_cores_energy` (id `0`) | `CPU Energy`: every CPU core |
| `cpu_core_energy` | `EACC_CPU<n>` and `PACC<c>_CPU<n>`: one core, at its logical CPU id |
| `cpu_igpu_energy` (id `0`) | `GPU Energy` |
| `cpu_dram_energy` (id `0`) | `DRAM` |
| `cpu_soc_energy` | every other channel, labelled with its name as `block` |

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_soc_energy` | Cumulative energy of an SoC block other than the CPU cores, GPU and DRAM. macOS only. | block: the IOReport channel name, less a trailing ` Energy`; unit: `microjoules` |

The blocks vary by chip. An M4 Max reports 14: `AFR`, `AMCC`, `ANE` (the
Neural Engine), `AVE` (the video encoder), `DCS`, `DISP` and `DISPEXT` (the
display pipelines), `ISP` (the image signal processor), `MSR`, `PCIe Port 0`,
`PCIe Port 1` and `apciec0` to `apciec2`. Not published:
the clusters' totals (`EACC_CPU`, `PACC<c>_CPU`), which `cpu_core_energy`
sums to, the clusters' SRAM and CPM channels, the `ECPUDTL` and `PCPUDTL`
channels, and `GPU`, a second GPU channel in mJ, which read about 1% above
`GPU Energy` over 20 s on an M4 Max. The `Energy Model` group has no package
or platform channel, so `cpu_package_energy` and `cpu_platform_energy` are not
set, and there are no C-state residencies.

### cpu_tlb_flush

Instruments TLB (Translation Lookaside Buffer) flush events. TLB flushes are
operations that clear address translation caches, which can affect application
performance.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_tlb_flush` | The number of tlb_flush events | `reason={task_switch,remote_shootdown,local_shootdown,local_mm_shootdown,remote_send_ipi}` |
| `cgroup_cpu_tlb_flush` | The number of tlb_flush events on a per-cgroup basis | `reason={task_switch,remote_shootdown,local_shootdown,local_mm_shootdown,remote_send_ipi}`, `name`: the name of the cgroup |

### cpu_usage

Instruments CPU usage by state. This provides a breakdown of how CPU time is
being spent across different states, which can help identify what's consuming
CPU resources. Understanding the distribution of CPU time (user, system, ...)
can be useful for diagnosing performance issues, capacity planning, and
optimizing workloads.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_usage` | The amount of CPU time spent in different CPU states | `state={user,nice,system,softirq,irq,steal,guest,guest_nice}` |
| `cgroup_cpu_usage` | The amount of CPU time spent in different CPU states on a per-cgroup basis | `state={user,nice,system,softirq,irq,steal,guest,guest_nice}`, `name`: the name of the cgroup |
| `softirq` | The count of softirqs | `kind={hi,timer,net_tx,net_rx,block,irq_poll,tasklet,sched,hrtimer,rcu}` |
| `softirq_time` | The time spent in softirq handlers | `kind={hi,timer,net_tx,net_rx,block,irq_poll,tasklet,sched,hrtimer,rcu}` |
| `cpu_usage_exited_tasks` | CPU time of tasks that have exited, per CPU the exit ran on; includes the CPU a thread used under its own thread id before it exec'd from outside the group leader | `id` |
| `cgroup_cpu_usage_exited_tasks` | CPU time of a cgroup's tasks that have exited | `name` |
| `task_cpu_usage` | CPU time (user and system) per thread; only with `task_attribution = true` | `pid`, `tgid`, `comm`, `cgroup` |
| `process_cpu_usage` | CPU time (user and system) per process, by every thread of the process including exited ones; only with `process_attribution = true`, or with `task_attribution = true` and `process_attribution` unset | `tgid`, `comm`, `cgroup` |

The sampler accounts CPU per task in every configuration: the per-CPU and
per-cgroup deltas are computed from each thread's `utime`/`stime` whether
or not per-task series are exported, so turning the export off does not
change the host or cgroup totals. Exporting that per-task accounting as
`task_cpu_usage` is the option `task_attribution`, **off by default**. On, it
costs a task-metadata event per new thread (comm and three cgroup names)
and per rename, an exit event per thread, a walk of the per-pid map's
populated slots each time a snapshot is served, and one series per thread
still running at a snapshot after it has lived `task_min_lifetime`, plus
one per rename after its series appears, in every recording. On a host with many threads that live longer than that, or with
frequent renames, those series are most of the recording. Set `task_attribution = true`
in `[samplers.cpu_usage]` (or `[defaults]`) to get it.

A thread gets a `task_cpu_usage` series only once it has lived
`task_min_lifetime` (a humantime string, default `"1s"`; `"0s"` turns the
minimum off). The age is checked each time a snapshot is taken, when the
sampler drains its event rings. A thread still running at the first
snapshot after it reaches that age gets its series in that snapshot. The
series' first value is the thread's CPU since it started, or since the
agent first saw it for a thread that predates the agent. A thread that
execs from outside the group leader takes the leader's pid and start time,
so its age is the process's, and its CPU before the exec is in
`cpu_usage_exited_tasks` under its old thread id, not in this series. A
thread without a series has its CPU in `cpu_usage`, and in
`cpu_usage_exited_tasks` once it exits. Unless `cgroup_attribution = false`,
it is also in `cgroup_cpu_usage`, and in `cgroup_cpu_usage_exited_tasks`
once it exits. When `process_cpu_usage` is exported, it is in
`process_cpu_usage` until its process exits. Renames
before the series first appears add no series; the series has the newest
name. With `snapshot_format = "v2"` the minimum is 0: a V2 snapshot lists
every non-zero slot, labelled or not, so a thread held back would appear
with only its `id`. With either snapshot format, a value that does not
parse as a humantime duration stops the agent at startup. The rates of a
process's `task_cpu_usage` series sum to at most its CPU, and usually less. They leave out threads
that never got a series, each thread's CPU before its series' first sample,
and each thread's CPU after its last sample. `process_cpu_usage` includes
all three.

`process_cpu_usage` is the option `process_attribution`. Set in neither
`[samplers.cpu_usage]` nor `[defaults]`, it takes the value
`task_attribution` resolves to, so it is off by default and on with
per-task series. Set `process_attribution = false` to keep only the per-task series, or
`true` to have it without them. It is one series per process (thread
group), keyed by `tgid`. Every CPU delta credited to `cpu_usage` for
a thread of the process is added to it, until the exit of its last thread,
including the CPU of threads that have exited. So `rate(process_cpu_usage)` is the process's
CPU however many threads it created and however many of them have their own
series. Like `task_cpu_usage`, it counts from when the process started, or
from when the agent first saw it for a process that predates the agent. Its
`comm` and `cgroup` are the group leader's. The leader's renames relabel
it, and so does exec, so a forked child is relabelled with the program it
runs. The series ends within one snapshot interval of the process's last
thread exiting. As with `task_cpu_usage`, a process that exits between a
drain and the snapshot that follows it has a last sample of 0, so read it
with `rate()` or `increase()`. It costs two per-pid arrays (64 MiB of
kernel memory), a 256 KiB ring, a metadata event per new process and per
leader rename, an exit event per process when `task_attribution` is off,
the `task_rename` program (one run per thread rename), and two marker
reads and a counter add per accounting event that adds CPU. A process's counter is written by every CPU running one of
its threads: with 48 threads of one process spinning on a 56-vCPU host
at HZ=250, each run of the `cpu_usage` BPF programs took about 310 ns,
against about 200 ns with 48 one-thread processes and 170 ns without
`process_cpu_usage`.

`comm` follows renames. A thread whose name changes (`pthread_setname_np`,
`prctl(PR_SET_NAME)`, a write to `/proc/<pid>/task/<tid>/comm`, or exec) is
relabelled with the new name, and from then on it is a new series. The new
series continues the thread's cumulative counter rather than starting from
zero, so read it with `rate()` or `increase()`, not as raw values. A rename
also re-reads `cgroup`, so a thread that has moved cgroups since it was
first seen gets its current one. The kernel keeps 15 bytes of a thread's
name, so a longer name arrives truncated.

The Prometheus exporter drops the internal `__uid__` label, so in its output
a thread that returns to an earlier name (A, then B, then A) with the same
cgroup continues the first A series after a gap. `rate()` or `increase()` over a range that
spans the gap counts the CPU used under B in the A series as well. Rezolus
recordings keep the two A series apart.

## Drive

Metrics related to physical drive health.

### drivehealth

Reports per-drive temperature for NVMe and SATA drives, read directly from the
drive via **read-only pass-through ioctls** — the same mechanism `smartctl` and
`hddtemp` use, with **no kernel module** (temperature has no BPF or perf hook;
this is the deliberate device-read exception in `docs/principles.md`). SATA uses
`SG_IO` ATA PASS-THROUGH (`SMART READ DATA`, attribute 194); NVMe uses the admin
Get Log Page 0x02 (Composite Temperature). Drives are enumerated once at startup
from `/sys/block` and `/sys/class/nvme`.

On macOS the sampler reads NVMe drives only, through IOKit's NVMe SMART user
client (Get Log Page 0x02, unprivileged), from the block storage devices whose
`NVMe SMART Capable` property is set. `device` is the whole disk's BSD name
(`disk0`). A drive accepts one SMART client at a time, so a sweep that finds
it held by another process (`smartctl`) skips that drive until the next sweep.

Because each read is a device command (measured ~7.6 ms/drive) and temperature
moves on the order of seconds, reads are throttled and offloaded from the
scrape/TTL sample cycle: at most once per `interval` the sampler dispatches the
reads (all drives in parallel) to a blocking thread pool and returns
immediately, so the sample cycle stays ~microseconds; the gauge retains its last
value between reads. The cadence defaults to 60s and is configurable via
`interval` in `[samplers.drivehealth]`. On macOS the drives are read one after
another in one blocking task.

Pass-through ioctls require `CAP_SYS_RAWIO` (the agent already runs privileged
for eBPF); unprivileged, reads fail closed — zero series, no error. Hosts with
no supported drive likewise emit no series. The `serial` label is potentially
sensitive — included for stable cross-reboot fleet identity, and omitted when
unavailable (SATA serial via ATA IDENTIFY is deferred; NVMe serial comes from
sysfs).

For NVMe, the same SMART/Health log read also yields **thermal-throttling
counters**. These are monotonic, so a coarse read cadence never misses an event —
`rate(drive_temperature_critical_time[5m])` reads as fraction of time throttled
(≈1.0 = continuously throttled), which is the direct signal for diagnosing NVMe
thermal throttling. The `drive_thermal_throttle_*` counters populate only when the
drive has Host-Controlled Thermal Management enabled; the warning/critical time
counters are always maintained by the controller.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `drive_temperature` | The current drive temperature in degrees Celsius | `device` (kernel name, e.g. `nvme0`, `sda`), `type={nvme,sata}`, `model`, `serial` (when available) |
| `drive_temperature_warning_time` | Cumulative seconds at/above the NVMe warning temperature threshold (WCTEMP) | `device`, `type=nvme`, `model`, `serial` |
| `drive_temperature_critical_time` | Cumulative seconds at/above the NVMe critical temperature threshold (CCTEMP) | `device`, `type=nvme`, `model`, `serial` |
| `drive_thermal_throttle_time` | Cumulative seconds in NVMe host thermal-management state | `level={1,2}`, `device`, `type=nvme`, … |
| `drive_thermal_throttle_transitions` | Count of transitions into NVMe host thermal-management state | `level={1,2}`, `device`, `type=nvme`, … |
| `drive_temperature_sensor` | An NVMe Temperature Sensor (1 to 8) in degrees Celsius; no value for a sensor the controller does not implement | `sensor={1..8}`, `device`, `type=nvme`, … |

On Linux on Amazon EC2 Nitro instances, the same read of an EBS volume's or NVMe
instance store volume's controller also fetches the device's detailed
performance statistics, vendor log page 0xD0 ([EBS][ebs-stats], [instance
store][is-stats]; layout from nvme-cli's `amzn` plugin, AWS's `ebsnvme` script
and the CloudWatch agent's NVMe receiver). A drive is read for these only when
its NVMe model is `Amazon Elastic Block Store` or starts with `Amazon EC2 NVMe
Instance Storage`. The counters are cumulative microseconds, published in
nanoseconds, so `rate(drive_performance_exceeded_time[5m]) / 1e9` is the
fraction of time the device was throttled by that limit. AWS documents reads up
to once a second; the sampler reads at its `interval`. The macOS sampler does
not read this page, and neither decode has been checked on a device.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `drive_performance_exceeded_time` | Cumulative time I/O demand exceeded a performance limit, in nanoseconds | `storage=ebs` with `limit=ebs_volume` (the volume's provisioned IOPS/throughput) or `limit=ec2_instance` (the instance's EBS IOPS/throughput limit); `storage=instance_store` with `limit=instance_store_volume` (the volume's maximum); `resource={iops,throughput}`, `device`, `type=nvme`, `model`, `serial` (for EBS, the volume id without its dash, `vol0123…`) |
| `drive_queue_length` | Read and write operations waiting on the device when it was read | `storage={ebs,instance_store}`, `device`, `type=nvme`, `model`, `serial` |

[ebs-stats]: https://docs.aws.amazon.com/ebs/latest/userguide/nvme-detailed-performance-stats.html
[is-stats]: https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/nvme-detailed-performance-stats.html

## ext4

Metrics from inside the ext4 filesystem and its journal, between the syscall
and the block device.

### ext4_journal

BPF sampler on the jbd2 and ext4 tracepoints. Reports every phase of each
journal commit, checkpoint cost, lock-buffer stalls, fsync counts and errors,
and filesystem errors.

**Counters are per filesystem.** Every counter below has one series per
mounted filesystem, carrying the labels the `filesystem` sampler gives the
same mount, `mount`, `fstype`, `devnum` (major:minor) and `block_device`
(when the device has a kernel name), so the two join; plus one series labeled
only `mount="other"` for a device the agent's mount table does not know, a
mount younger than the last rescan or beyond the 63-slot cap. Every event
lands in some series, so `sum(irate(...))` over a metric is the host rate.
The cumulative sum is not a host total: an unmounted filesystem's series ends
and a remount starts a fresh one from zero, so rates, not raw values, are the
thing to compare across mounts. The BPF program looks the device up in a
`dev_t → slot` map on each event; the agent re-reads `/proc/self/mountinfo`
every 10 s, and sooner (at most once a second) when `other` moves, so a new
mount is attributed within two refreshes of its first event, and the events
before that stay under `other`. jbd2 is also ocfs2's journal, and an ocfs2
mount gets its own slot with `fstype="ocfs2"`. **Histograms are host-wide**
until histogram groups have slots.

Commit phases, handle start and checkpoints are timed in BPF with
`bpf_ktime_get_ns()` between jbd2's tracepoints (and, for handle start, the
entry of `jbd2__journal_start`, `jbd2_journal_start` and
`jbd2_journal_start_reserved`). jbd2's own figures for these are differences
of two jiffies reads and are not used. Lock-buffer stalls are reported by
jbd2 in whole milliseconds, and only stalls over 100 ms (`HZ/10`).

Reading the jbd2 statistics needs their types in BTF: vmlinux when ext4 is
built in, module BTF (`/sys/kernel/btf/jbd2`, kernels 5.11+) when it is a
module. A 5.8–5.10 kernel with `CONFIG_EXT4_FS=m` has neither, and the sampler
reports failed there. `ext4_errors` needs the `ext4_error` tracepoint, which
is younger than the 5.8 floor; on a kernel without it the sampler reports
degraded and the rest of the metrics are unaffected.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `ext4_journal_commit_latency` | Distribution of each phase of a journal commit, in nanoseconds. `running`: how long the transaction was open before the commit began; `locked`: waiting for outstanding handles, cleaning the checkpoint list and switching the revoke table; `flushing`: submitting the data writes (ordered mode) and revoke records; `logging`: writing metadata to the journal, waiting for the data writes, flushing the device cache and writing the commit record, where fsync waits on the device | `phase={running,locked,flushing,logging}` |
| `ext4_journal_handle_start_latency` | Distribution of the time a thread took to start a journal handle, from calling into jbd2 to the handle being granted, including waits for the committing transaction to leave the locked state, for journal space and for credits, in nanoseconds; handle restarts are not included | |
| `ext4_journal_commits` | The number of journal transaction commits | |
| `ext4_journal_commit_handles` | Handles (metadata operations) committed, summed over commits | |
| `ext4_journal_commit_blocks` | Blocks per commit, summed: `dirtied` is metadata blocks in the transaction, `logged` is blocks written to the journal including descriptor and commit blocks | `kind={dirtied,logged}` |
| `ext4_journal_checkpoint_latency` | Distribution of the time from a checkpoint first working on a transaction to that transaction being dropped, in nanoseconds; transactions dropped without a checkpoint are not included | |
| `ext4_journal_checkpoints` | The number of journal checkpoints | |
| `ext4_journal_checkpoint_buffers` | Buffers a checkpoint wrote to their final location, or found already written and dropped | `outcome={written,dropped}` |
| `ext4_journal_checkpoint_forced_to_close` | Transactions a checkpoint forced closed to free journal space; a rising rate means the journal is too small for the write rate | |
| `ext4_journal_lock_buffer_stall_latency` | Distribution of time the journal stalled on a locked buffer, in nanoseconds (jbd2 reports whole milliseconds) | |
| `ext4_journal_lock_buffer_stalls` | The number of lock-buffer stalls | |
| `ext4_sync_file` | fsync and fdatasync calls that reached ext4 | `op={fsync,fdatasync}` |
| `ext4_sync_file_errors` | fsync and fdatasync calls ext4 completed with an error | |
| `ext4_errors` | Errors ext4 reported, counted as they occur and before any `errors=remount-ro` takes effect | |
| `ext4_shutdowns` | Forced ext4 filesystem shutdowns | |

Host-wide fsync *latency* is not here: it is the `sync` class of
`syscall_latency`, which already holds the per-thread start timestamp.

### ext4_alloc

BPF sampler on ext4's block allocator and the metadata reads around it.
Reports each extent allocation's requested versus returned length, the block
groups scanned and the criterion the allocator finished at, blocks freed,
inodes allocated and freed, writeback passes with their pages written and
skipped, discards and preallocation releases, and the synchronous inode-table
and bitmap reads that land on the calling thread. Counters are per filesystem
exactly as `ext4_journal`'s are (`mount`, `fstype`, `devnum`, `block_device`,
plus `mount="other"`); `ext4_allocation_size` is host-wide. Same
kernel-support rule as `ext4_journal`: the allocator hook reads
`struct ext4_allocation_context` through CO-RE, which needs ext4's types in
vmlinux or module BTF.

The allocator signals are the always-on form of what `e2freefrag` reports
offline: free space fragmenting shows as allocations returning fewer blocks
than requested, more block groups scanned per allocation, and a rising share
of allocations finishing at the slow criteria. `ext4_inode_loads` fires once
per inode-table block read from the device, so it is the rate of synchronous
metadata reads a cold inode cache imposes on `stat` and atime updates.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `ext4_allocation_size` | Distribution of the length of each extent the allocator returned, in filesystem blocks | |
| `ext4_allocations` | Extent allocations (`ext4_mballoc_alloc`); more than one per file created is a file split across extents | |
| `ext4_allocation_blocks` | Blocks requested of and returned by the allocator, summed; returned falling short of requested is fragmentation | `kind={requested,allocated}` |
| `ext4_allocation_groups_scanned` | Block groups scanned, summed over allocations; per allocation it is the allocator's effort | |
| `ext4_allocations_by_criterion` | Allocations by the criterion the allocator finished at. 0 and 1 are the fast paths on every kernel. From 6.5: 2 trims to the best available length, 3 scans every group, 4 takes any free block; before 6.5: 2 scans every group, 3 takes any free block, and 4 never occurs. A rising share at 2 or above is fragmentation | `criterion={0,1,2,3,4}` |
| `ext4_freed_blocks` | Blocks returned to the free pool (`ext4_free_blocks`) | |
| `ext4_inodes` | Inodes allocated and freed | `op={allocated,freed}` |
| `ext4_writepages` | Writeback passes over an inode's dirty pages (`ext4_writepages_result`) | |
| `ext4_writepages_pages` | Pages those passes wrote, or skipped and left for later | `outcome={written,skipped}` |
| `ext4_writepages_errors` | Passes that ended in an error | |
| `ext4_trimmed_blocks` | Blocks discarded to the device by fstrim or online discard | |
| `ext4_preallocation_discards` | Times an inode's preallocated blocks were released (close, truncate, unlink) | |
| `ext4_preallocation_discarded_blocks` | Preallocated blocks released, summed | |
| `ext4_inode_loads` | Inode-table reads from the device because an inode was not cached (`ext4_load_inode`), each a synchronous read of up to `inode_readahead_blks` blocks (32 by default), so 20,000 cold `stat`s cost about 40 reads on a fresh filesystem | |
| `ext4_bitmap_loads` | Block-allocation bitmaps (including the allocator's prefetches) and inode-allocation bitmaps read from the device | `kind={block,inode}` |

### ext4_ops

BPF sampler that times ext4's request-path operations from the calling
thread's side: how long each fsync, unlink, write and rename held the thread
inside the filesystem, per filesystem and per cgroup. fsync and unlink are the
`ext4_sync_file_enter`/`_exit` and `ext4_unlink_enter`/`_exit` tracepoints;
write and rename have no tracepoints and are `fentry`/`fexit` on
`ext4_file_write_iter` and `ext4_rename2`. The start timestamp lives in task
local storage, one slot per operation, so an O_SYNC write's inner fsync does
not lose the outer write's timing.

**Opt-in.** Both probes of a pair run on the request path once per call, so
this is the most expensive of the ext4 samplers: measured at +4,070
instructions and +2.3 µs per write-plus-fsync pair (four probes) on a
`null_blk` fsync bench, 12% of the CPU at a saturating 450 K operations per
second, about 4.5% of one core at 20 K fsync/s. It is one of the samplers the
`[defaults]` section never turns on (with `gpu_amd_pmu`, `memory_pagecache`
and `xfs_log`):
enable it with `[samplers.ext4_ops] enabled = true` when that cost is
acceptable for the workload; the journal entry has the bench.

An asynchronous direct write (io_uring, libaio with `O_DIRECT`) returns from
`ext4_file_write_iter` as queued, so for it the write latency is the
submission time, it is not an error, and its bytes are reported at completion
where this sampler does not see them: `ext4_write_bytes` undercounts such
workloads by exactly their direct-IO bytes.

Counters are per filesystem exactly as `ext4_journal`'s are (`mount`,
`fstype`, `devnum`, `block_device`, plus `mount="other"`); the latency
histograms are host-wide. `ext4_op_time / ext4_ops` is the mean latency per
filesystem. `ext4_write_bytes` is the first term of write amplification, which
the ext4 dashboard's Write Path group draws against `ext4_writepages_pages`,
`ext4_journal_commit_blocks{kind="logged"}` and `blockio_bytes{op="write"}`.
The per-cgroup series answer "how long are this service's request threads held
inside the filesystem", the mechanism a slow disk reaches a request through.
They exist only with `cgroup_attribution = true` in the sampler's section
(or `[defaults]`): the per-cgroup path is 265 ns of the end hook's 535 ns,
half its cost, so it is off by default and, when off, is removed from the
loaded program rather than skipped at run time. Without it a write+fsync
pair costs about 1.1 µs of probe time instead of 1.7.

Kernel support: task local storage became usable from tracing programs in
5.12, and `fentry` on a module's functions needs module BTF (5.11), so the
sampler needs 5.12 or later. The sampler probes both at init (the helper with
libbpf's probe, the tracepoints in BTF) and on an older kernel `rezolus
status` shows it unsupported rather than failed. `ext4_rename2` has taken six arguments since 5.12; the sampler
confirms that from BTF and disables the rename pair on any other count. If
fentry cannot attach to `ext4_file_write_iter` or `ext4_rename2` (not in BTF,
or aarch64 before 6.4), that operation's histogram stays empty and its
counters read 0 while the rest run, and `rezolus status` shows the sampler
unsupported and names the operation.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `ext4_op_latency` | Distribution of the time a call held the calling thread inside ext4, in nanoseconds | `op={fsync,unlink,write,rename}` |
| `ext4_ops` | Calls that completed | `op`, `mount`, `fstype`, `devnum`, `block_device` |
| `ext4_op_time` | Nanoseconds the calls held their threads, summed; over `ext4_ops` it is the mean latency | `op`, `mount`, ... |
| `ext4_op_errors` | Calls that returned an error | `op`, `mount`, ... |
| `ext4_write_bytes` | Bytes applications wrote into ext4 (the return values of `ext4_file_write_iter`), summed | `mount`, ... |
| `cgroup_ext4_ops` | Calls that completed, by the calling thread's cgroup | `op`, `name` |
| `cgroup_ext4_op_time` | Nanoseconds a cgroup's threads spent inside each operation, summed | `op`, `name` |

## Filesystem

Metrics related to filesystem occupancy.

### filesystem

Reports total, free and available bytes, total and free inodes, and read-only
state for every **locally mounted** filesystem: one series per filesystem
(superblock), read with one `statvfs` each.

The mount table (`/proc/self/mountinfo`) is re-read on every sweep, so a
filesystem mounted after the agent started is picked up, and one that is
unmounted drops out. A filesystem mounted more than once (bind mounts, btrfs
subvolumes) is reported once, under its shortest mount point.

**Local only.** Block-backed filesystems with a `/dev` source, plus `zfs`, are
sampled. Network filesystems (`nfs`, `nfs4`, `cifs`, `smb3`, `ceph`,
`glusterfs`, ...), every FUSE filesystem, autofs triggers and the kernel's
pseudo-filesystems (`overlay`, `proc`, `sysfs`, squashfs images, ...) are never
touched. tmpfs is the one exception, read into its own metrics (below).
Neither is a local filesystem that another mount covers — an NFS share
mounted over `/data`, or over a directory above it — since its path
now leads to the mount on top — nor one whose path passes through a network,
FUSE or autofs mount, since looking the path up walks that mount. An agent in a
chroot whose mount table omits the mount holding its root samples nothing, since
that mount's type is unknown. Each skipped filesystem is logged with the
reason: as a warning when skipping starts, at debug when another is skipped
while some already are, and at info when every candidate resolves again. A `statvfs` on a `hard` network mount
blocks until the server answers, with no timeout the agent can set, and one on
an autofs trigger starts a mount attempt; local filesystems answer from kernel
state without a network round trip (ext4 and XFS from superblock counters,
btrfs after its own space accounting), which is what bounds the sweep. Network
mounts stay out of scope until there is demand for them (#1202).

Occupancy moves slowly, so sweeps are throttled and run off the scrape/TTL
sample cycle: at most once per `interval` the sampler dispatches the sweep to a
blocking thread pool and returns immediately; the gauges keep their last value
between sweeps. The cadence defaults to 60s and is configurable via `interval`
in `[samplers.filesystem]`. Measured cost (release, 87-line mount table, 3
local filesystems): a 330–570 µs sweep once per interval, most of it the
kernel generating the mount table; `refresh()` on the scrape path is 0–8 µs.

On macOS, see [Filesystem on macOS](#filesystem-on-macos).

`filesystem_available` is the number `df` reports as available and the one to
alert on; `filesystem_free` also counts the blocks reserved for the superuser.
Inode exhaustion is the other way a disk fills, and comes from the same call.

`filesystem_readonly` is 1 while the filesystem as a whole is read-only: its
superblock is read-only, which a read-only mount or a btrfs forced read-only
sets, or ext4 has gone emergency read-only after an error. Current ext4 marks
that with `emergency_ro` in the superblock options instead of setting the
superblock flag, so both are checked. It does not say why; whether a 1 is a
read-only mount or an error is for the operator to judge. Filling up does not
set it — writes to a full filesystem fail with `ENOSPC` while it stays writable
— and a read-only bind of a writable filesystem reads 0, because the filesystem
is still writable through its other mounts. An XFS shutdown sets neither
signal, so 0 does not prove the filesystem is writable.

`block_device` is the kernel's name for the filesystem's partition or mapped
device, such as `nvme0n1p5` or `dm-0`, not the drive. It does not match
`drivehealth`'s `device` label for the same disk (#1217). ZFS datasets and btrfs have no
block device and carry no `block_device`; `devnum` is always present.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `filesystem_total` | Size of the filesystem in bytes | `mount`, `fstype`, `devnum` (major:minor), `block_device` (when block-backed) |
| `filesystem_free` | Unallocated bytes, including the superuser reserve | `mount`, `fstype`, `devnum`, `block_device` |
| `filesystem_available` | Bytes an unprivileged process can still write | `mount`, `fstype`, `devnum`, `block_device` |
| `filesystem_inodes_total` | Inodes the filesystem reports it can hold; absent on btrfs and vfat, which report no inode limit | `mount`, `fstype`, `devnum`, `block_device` |
| `filesystem_inodes_free` | Free inodes | `mount`, `fstype`, `devnum`, `block_device` |
| `filesystem_readonly` | 1 when the filesystem is read-only as a whole: superblock `ro`, or ext4 `emergency_ro` | `mount`, `fstype`, `devnum`, `block_device` |
| `filesystem_errors` | ext4 only: errors recorded in the superblock (sysfs `errors_count`), persisting until e2fsck clears them; absent on other types | `mount`, `fstype`, `devnum`, `block_device` |
| `filesystem_written_bytes` | ext4 only: bytes written to the block device over the filesystem's lifetime, journal included (sysfs `lifetime_write_kbytes`); a counter, so its rate is the filesystem's write bandwidth on the device; absent on other types | `mount`, `fstype`, `devnum`, `block_device` |

The two ext4 series come from `/sys/fs/ext4/<block_device>/`, read on the same
sweep. `errors_count` is the count the superblock persists (`s_error_count`),
so it survives a remount and a reboot and is cleared only by `e2fsck`; a
non-zero value says the filesystem has hit an error since its last check, and a
step says one just happened, whether or not `errors=remount-ro` then set
`filesystem_readonly`. `lifetime_write_kbytes` is the superblock's persisted
write total plus the device's write sectors since mount, so it counts every
write the filesystem issued, journal commits included; against the bytes
applications wrote it is the filesystem's term of write amplification. Both are
sysfs text reads on the 60 s off-cycle sweep, the same principle 15 exception
the sweep itself received, and they work on kernels the ext4 BPF samplers
cannot run on.

**tmpfs.** A tmpfs mount holds its files in memory, and meminfo counts the
resident pages in `memory_shmem` without saying where. The same sweep reads
every visible tmpfs mount with the same `statvfs`, answered from the mount's
own counters, and publishes its use and limits as `filesystem_tmpfs_*`,
labeled `mount`, `fstype` and `devnum`. They are kept out of
`filesystem_total` and its siblings because a tmpfs size is a limit on memory, by default half of RAM, and not capacity on a
device: two default tmpfs mounts on a 251 GB host would add 251 GB to a sum of
disk capacity. `filesystem_tmpfs_used` counts a mount's pages whether resident
or swapped out (`shmem_statfs` reports allocated blocks), while `Shmem` drops a
page when it goes to swap, so used is resident plus swapped and only the
resident part is in `memory_shmem`. `memory_shmem` minus
`sum(filesystem_tmpfs_used)` is what Shmem holds outside the visible tmpfs
mounts: SysV shared memory, shared anonymous mappings, memfd files and GPU
driver (DRM/GEM) buffers, which live on kernel-internal mounts that no mount
table lists, plus tmpfs mounts in other mount namespaces or
over the cap. POSIX shared memory (`shm_open`) is files in `/dev/shm` and is
counted there. When tmpfs pages are swapped out the difference can be
negative. A
tmpfs mounted with `size=0` has no limit and reports no usage, so its byte
series are absent, and likewise its inode series with `nr_inodes=0`. ramfs is
not read: `statvfs` on it reports nothing. hugetlbfs pages are in
`memory_hugetlb`.

tmpfs mounts appear and go with logins (`/run/user/<uid>`), services (systemd's
`/run/credentials/<unit>` mounts) and containers (one `/dev/shm` per Docker
container, one per pod volume under Kubernetes), and each is its own series.
They are capped at 64, apart from the 256 local filesystems, so they never take
a local filesystem's place. Over the cap, the mounts with the fewest path
components are placed first, so `/tmp`, `/dev/shm` and `/run` come before a
container's mounts, and a mount once placed keeps its series until it is
unmounted. The first mount left out is logged as a warning, changes in the
number at debug, and fitting again at info, so a host whose containers churn
does not warn every sweep. A tmpfs mount whose path passes through a network,
FUSE or autofs mount is skipped like a local filesystem there.

Each sweep logs at debug level `filesystem: sweep published <n>/<m>
filesystem(s) (<l> local, <t> tmpfs selected; <l> local, <t> tmpfs over the
cap) in <us> us (read <us> us, parse <us> us, statvfs+publish <us> us)`.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `filesystem_tmpfs_used` | Bytes of files in the tmpfs mount, resident or swapped out; the resident part is in `memory_shmem`; absent with no size limit | `mount`, `fstype`, `devnum` |
| `filesystem_tmpfs_size` | The mount's size limit in bytes (`size=`); a limit only, not memory held | `mount`, `fstype`, `devnum` |
| `filesystem_tmpfs_inodes_used` | Files, directories and links on the mount; absent with no inode limit | `mount`, `fstype`, `devnum` |
| `filesystem_tmpfs_inodes_total` | The mount's inode limit (`nr_inodes=`) | `mount`, `fstype`, `devnum` |

#### Filesystem on macOS

On macOS the sampler lists the mounts with `getfsstat(MNT_NOWAIT)`,
unprivileged, and samples those that are local (`MNT_LOCAL`), report a
nonzero size, and are not `devfs`, `autofs` or macFUSE: APFS and HFS+ volumes,
including the system's hidden volumes under `/System/Volumes`, and mounted
disk images. Network mounts are left out.

The counters `getfsstat(MNT_NOWAIT)` returns are the ones each mount's last
refresh left (a `statfs`, or a `getfsstat` without `MNT_NOWAIT`, by any
process), so on a volume nothing else reads they can be hours old. Each
selected volume is therefore read again with `statfs` and `getattrlist` on its
mount point, and a reading where either call reports a device other than the
listed one, because the mount was replaced or unmounted, is dropped. A
`getattrlist` that fails leaves only `filesystem_volume_used` unset. Each call
looks the path up, which walks every mount above it, so a volume mounted
beneath a network, autofs or macFUSE mount is skipped. The sweep runs on a
blocking thread at most once per `interval` (60s by default); one still
running after three intervals is logged as a warning. With 10 volumes, `getfsstat` and the
`statfs` and `getattrlist` calls took 36 µs in a debug build, 12 µs of it
`getattrlist`.

`filesystem_total`, `filesystem_free`, `filesystem_available`,
`filesystem_inodes_total`, `filesystem_inodes_free` and `filesystem_readonly`
are set, with the same labels as on Linux: `devnum` is the `major:minor` of the
volume's device node and `block_device` its BSD name, such as `disk3s5`.
`filesystem_readonly` is the mount's `MNT_RDONLY` flag, so the sealed system
volume at `/` reads 1. `filesystem_errors`, `filesystem_written_bytes` and the
`filesystem_tmpfs_*` series are not set.

The volumes of an APFS container share its space. A volume without a quota or
reserve reports the container's size as its total and the container's free
space as its free and available bytes, so total minus free is the space used
in the whole container, not by the volume. The volume's own use is
`getattrlist(ATTR_VOL_SPACEUSED)`, the number `df` reports as used, published
as `filesystem_volume_used`. HFS+, FAT and exFAT volumes report it as total
minus free. APFS reports an inode total that moves with free space.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `filesystem_volume_used` | Bytes the volume itself uses (`ATTR_VOL_SPACEUSED`); macOS only | `mount`, `fstype`, `devnum`, `block_device` |

## XFS

Metrics from inside XFS, between the syscall and the device.

### xfs_stats

XFS's own per-mount counters, read from `/sys/fs/xfs/<block_device>/stats/stats`
(the same numbers `/proc/fs/xfs/stat` sums over mounts): the log, log space,
the AIL pusher, transactions, the inode cache, the allocator, directories,
file I/O and the metadata buffer cache. No probes: XFS maintains these at
event rate itself, and counting the same events in BPF would cross about 5.6
hooks per fsync (measured in `docs/journal/2026-09-29-xfs-samplers.md`) for
numbers the kernel already has. What the file cannot say, how long a
transaction waited for log space or a log force took and which cgroup waited,
is the planned `xfs_log` BPF sampler's job.

One read of the file costs about 160 µs, so the sweep runs off the scrape
cycle on the blocking pool at most once per `interval` (default 1 s,
`[samplers.xfs_stats]`); the counters keep their last values between sweeps.

Every series is per mount, labeled `mount`, `fstype`, `devnum` and
`block_device` exactly as the `filesystem` and ext4 samplers label the same
mount, through the same slot registry, so the three join. A mount that is
not XFS has no series here, and a host with no XFS mount has no table; a
kernel whose file lacks a line or field leaves that counter absent. Field
names below are the kernel's `xfsstats` (`fs/xfs/xfs_stats.h`). Every field
but the byte counts is a 32-bit counter in the kernel, so a busy mount wraps
one eventually (`xfs_log_blocks_written` after 2 TiB of log writes,
`xfs_file_calls` after 4.29 billion calls); a wrap reads as a counter reset,
the same as a remount.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `xfs_log_writes` | Log writes (`log/writes`) | `mount`, `fstype`, `devnum`, `block_device` |
| `xfs_log_blocks_written` | 512-byte blocks written to the journal (`log/blocks`); ×512 is the journal's share of device writes | `mount`, ... |
| `xfs_log_iclog_stalls` | Log writes that waited for a free in-core log buffer (`log/noiclogs`) | `mount`, ... |
| `xfs_log_forces` | Log forces, the synchronous flush an fsync demands (`log/force`) | `mount`, ... |
| `xfs_log_force_sleeps` | Forces that waited for a log write to complete (`log/force_sleep`); a synchronous force sleeps once for its own write, and a second sleep per force is a wait on the previous in-core log buffer, that is on another caller's commit | `mount`, ... |
| `xfs_log_space_requests` | Transactions that reserved log space (`push_ail/try_logspace`) | `mount`, ... |
| `xfs_log_space_sleeps` | Transactions that slept for log space (`push_ail/sleep_logspace`); any rate means the log is too small or too slow for the write rate | `mount`, ... |
| `xfs_ail_pushes` | AIL push attempts (`push_ail/pushes`) | `mount`, ... |
| `xfs_ail_push_items` | Items the pusher visited, by outcome (`push_ail/success`, `pushbuf`, `pinned`, `locked`, `flushing`) | `outcome={success,pushbuf,pinned,locked,flushing}`, `mount`, ... |
| `xfs_ail_push_restarts` | Pushes that restarted after too many pinned or locked items (`push_ail/restarts`) | `mount`, ... |
| `xfs_ail_flushes` | Pushes that forced the log because everything was pinned (`push_ail/flush`) | `mount`, ... |
| `xfs_transactions` | Transactions committed (`trans/sync`, `async`, `empty`) | `kind={sync,async,empty}`, `mount`, ... |
| `xfs_inode_cache_lookups` | Inode-cache lookups by outcome (`ig/found`, `missed`, `frecycle`, `dup`); a miss reads the inode from disk on the calling thread | `outcome={found,missed,recycled,duplicate}`, `mount`, ... |
| `xfs_inode_reclaims` | Inodes reclaimed from the cache (`ig/reclaims`) | `mount`, ... |
| `xfs_extents` | Extents allocated and freed (`extent_alloc/allocx`, `freex`) | `op={allocated,freed}`, `mount`, ... |
| `xfs_extent_blocks` | Blocks allocated and freed (`extent_alloc/allocb`, `freeb`); allocated blocks over allocated extents is the mean extent length | `op={allocated,freed}`, `mount`, ... |
| `xfs_directory_ops` | Directory operations (`dir/lookup`, `create`, `remove`, `getdents`) | `op={lookup,create,remove,getdents}`, `mount`, ... |
| `xfs_file_calls` | Write and read calls into XFS (`rw`) | `op={write,read}`, `mount`, ... |
| `xfs_file_bytes` | Bytes written into and read from XFS (`xpc/write_bytes`, `read_bytes`) | `op={written,read}`, `mount`, ... |
| `xfs_buffer_lookups` | Metadata buffer lookups (`buf/get`) | `mount`, ... |
| `xfs_buffer_creates` | Buffers created on a lookup that found none (`buf/create`) | `mount`, ... |
| `xfs_buffer_lock_waits` | Lookups that waited for the buffer lock (`buf/get_locked_waited`) | `mount`, ... |
| `xfs_buffer_busy_locks` | Trylocks that found the buffer busy (`buf/busy_locked`) | `mount`, ... |
| `xfs_buffer_misses` | Lookups that missed the cache (`buf/miss_locked`) | `mount`, ... |
| `xfs_buffer_reads` | Buffers read from the device (`buf/get_read`), the synchronous metadata reads of a cold buffer cache | `mount`, ... |

### xfs_log

BPF sampler that times the two places a thread blocks on the XFS log, per
filesystem and per cgroup: waiting for log space (a transaction reservation
that found the log full, `xfs_log_grant_sleep` to `xfs_log_grant_wake` on the
same thread) and forcing the log (the synchronous log write an fsync waits
for, `fentry`/`fexit` on `xfs_log_force` and `xfs_log_force_seq`). It also
counts transactions that found the CIL over its hard limit (`xfs_log_cil_wait`;
a count only, since nothing traces that wake). The start timestamp lives in
task local storage, one slot per pair, so a force that sleeps for log space
inside it keeps both timings.

The counts here duplicate two fields `xfs_stats` reads from sysfs and equal
them per mount: `xfs_log_waits{wait="space"}` is `xfs_log_space_sleeps`
(both are incremented per trip through the grant wait loop) and
`xfs_log_waits{wait="force"}` is `xfs_log_forces` (each force function counts
once at entry). What the stats file cannot carry is how long anyone waited and
which cgroup did; that is what this sampler adds. `xfs_log_wait_time /
xfs_log_waits` is the mean per mount. The `cgroup_*` series exist only with
`cgroup_attribution = true` in the sampler's section (or `[defaults]`): the
per-cgroup path is about half the end hook's cost (as measured on
`ext4_ops`, the same shape), so it is off by default and removed from the
loaded program when off.

**Opt-in.** The force pair runs on every fsync, so this costs on the request
path; the journal entry (`docs/journal/2026-09-29-xfs-samplers.md`) has the
bench. It is one of the samplers the `[defaults]` section never turns on
(with `ext4_ops`, `gpu_amd_pmu` and `memory_pagecache`): enable it with
`[samplers.xfs_log] enabled = true`.

Kernel support: task local storage became usable from tracing programs in
5.12, and attaching to XFS's tracepoints and functions needs XFS's BTF (vmlinux
if built in, module BTF from 5.11 if a module), so the floor is 5.12 with XFS
loaded. The fsync path's force is `xfs_log_force_seq` from 5.13 and
`xfs_log_force_lsn` before; the sampler loads whichever the kernel's BTF has,
and on a kernel with neither the fsync-path forces are not timed. On an older
kernel, or a host with no XFS, `rezolus status` shows the sampler unsupported
rather than failed. A kernel without `xfs_log_cil_wait` in BTF reads 0 for the
CIL count. On aarch64 without ftrace direct calls (before 6.4) fentry cannot
attach to the force functions: the force series read 0 and the log-space and
CIL series are recorded. Each hook left out is named in `rezolus status`, which
shows the sampler unsupported.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `xfs_log_wait_latency` | Distribution of the time a thread was blocked on the log, by wait: `space` is one grant sleep, `force` is one log force entry to return (a force without `XFS_LOG_SYNC`, as inode unpinning issues, returns once the write is issued and sits in the low tail) | `wait={space,force}`, `unit=nanoseconds` |
| `xfs_log_waits` | Log waits completed: grant sleeps, forces, and CIL-full waits (count only) | `wait={space,force,cil}`, `mount`, `fstype`, `devnum`, `block_device` |
| `xfs_log_wait_time` | Nanoseconds threads were blocked, summed, by wait; over `xfs_log_waits` it is the mean | `wait={space,force}`, `mount`, ... |
| `cgroup_xfs_log_waits` | Log waits by the waiting thread's cgroup | `wait={space,force}`, `name` |
| `cgroup_xfs_log_wait_time` | Nanoseconds a cgroup's threads were blocked on the log, summed: time request threads were held by a full log or by durability | `wait={space,force}`, `name` |

## GPU

Metrics related to GPU performance. These metrics provide visibility into GPU
resource utilization, power consumption, and operational characteristics. They
can be helpful for monitoring GPU-accelerated workloads, optimizing resource
allocation, and identifying performance bottlenecks in GPU-intensive
applications.

### gpu_nvidia

Produces various NVIDIA specific GPU metrics using NVML (NVIDIA Management
Library). These metrics give insights into GPU performance, memory usage, power
consumption, and thermal conditions.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `gpu_memory` | The amount of GPU memory | `state={free,used}` |
| `gpu_pcie_bandwidth` | The PCIe bandwidth | `direction=receive` |
| `gpu_pcie_throughput` | The current PCIe throughput | `direction={receive,transmit}` |
| `gpu_power_usage` | The current power usage in milliwatts | |
| `gpu_energy_consumption` | The energy consumption in milliJoules | |
| `gpu_temperature` | The current temperature in degrees Celsius | |
| `gpu_clock` | The current clock speed for different GPU domains | `clock={compute,graphics,memory,video}` |
| `gpu_utilization` | The running average percentage of time the GPU was executing one or more kernels (0-100) | |
| `gpu_memory_utilization` | The running average percentage of time that GPU memory was being read from or written to (0-100) | |

### gpu_intel_pmu

Produces Intel GPU metrics from the i915 PMU via `perf_event_open`, the same
data source `intel_gpu_top` reads. Covers both integrated GPUs and discrete Arc
cards: each GPU registers its own PMU, which the sampler discovers from sysfs
along with the engine set that GPU actually exposes (a discrete card has a
compute engine; an integrated GPU typically does not). All events for a GPU are
opened as one perf group, so a single read returns every counter with no skew
between engines.

Requires `CAP_PERFMON` (or `kernel.perf_event_paranoid` <= 2).

The PMU values are **cumulative counters**, so the interesting quantities are
rates:

- `rate(gpu_engine_busy_time) * 100` — engine utilization, in percent.
- `rate(gpu_frequency_sample)` — average frequency in MHz over the interval.
  The driver accumulates one MHz sample per tick while the GT is awake, so this
  is a running sum rather than an instantaneous gauge; a single reading carries
  no meaning without its predecessor.

VRAM is the exception: it is a gauge in bytes, from the DRM query ioctl rather
than the PMU.

| Metric | Type | Description | Metadata |
|--------|------|-------------|----------|
| `gpu_engine_busy_time` | counter | Nanoseconds an engine spent executing work | `id`, `device`, `type`, `engine`, `engine_class={render,copy,video,video-enhance,compute}` |
| `gpu_frequency_sample` | counter | Cumulative sum of frequency samples in MHz; `rate()` yields average MHz | `id`, `device`, `type`, `frequency={actual,requested}` |
| `gpu_memory` | gauge | The amount of GPU device memory (VRAM), in bytes | `id`, `device`, `type`, `state={used,free}` |

The `device` label is the GPU's PCI address (e.g. `0000:04:00.0`) for a discrete
card, or `integrated` for an integrated GPU; prefer it over `id` when joining
across restarts. `type` is `discrete` or `integrated`, which is the quickest way
to separate an Arc card from the iGPU on a host that has both.

`gpu_memory` uses the same name and units as the AMD and NVIDIA samplers, so
cross-vendor dashboards work unchanged. It is **discrete-only**: an integrated
GPU has no device-local memory region, so it publishes no VRAM series rather
than passing host RAM off as VRAM. VRAM comes from the DRM
`QUERY_MEMORY_REGIONS` ioctl and needs `CAP_PERFMON` — without it the kernel
reports all memory as unallocated, so the sampler suppresses the series rather
than publishing a misleading "0 used".

Pair busy time with frequency when interpreting load. The PMU reports engine
**occupancy, not efficiency**: an engine reading 100% busy had work queued,
which does not mean the EUs were saturated. Frequency separates "busy but
downclocked" from genuinely saturated. EU-level detail needs the separate i915
perf/OA interface, which this sampler does not use.

Deliberately not collected, though the PMU exposes them: `<engine>-wait` and
`<engine>-sema` (why an engine stalled — a debugging question rather than a load
one), and `rc6-residency`, `software-gt-awake-time` and `interrupts` (power-state
and IRQ accounting). GPU temperature and energy are available from the i915
hwmon node but are not collected here either. `gpu_memory_utilization` and
`gpu_pcie_throughput` are not available on Intel at all — there is no
memory-controller or PCIe counter in this PMU.

### Cadence

This sampler reads on its own interval (`[samplers.gpu_intel_pmu] interval`,
default 1s) rather than on the scrape cycle, serving cached values in between.
A grouped `read(2)` on an i915 perf fd is not an mmap load — it takes a driver
lock and samples each event, plus ~5.4 us for the VRAM ioctl — so driving it at
the snapshot-TTL rate would burn CPU for values that move on the order of
seconds. Measured end to end on a host with two Intel GPUs (an Arc A770 and an
integrated GPU, 11 engines and 4 frequency counters between them): 40-66 us per
sweep in a release build. Reads are dispatched via `spawn_blocking`; the
on-cycle cost is a time comparison.

The read cadence admits a scrape arriving up to 50ms early. Without that
tolerance, a consumer scraping at the same period as `interval` — the common
case, since both default to 1s — has roughly every other scrape rejected by
timer jitter, and the exported cumulative counters then alternate between an
unchanged value and one that jumped two intervals' worth. That differentiates
to double the true rate with no gap to indicate a dropped sample: an A770 held
at a steady 2400 MHz recorded as 4800. Measured before the fix, two thirds of
samples in a 155-second capture were stale repeats.

The sweep is bracketed by two acquisition groups rather than one
(`gpu_intel_pmu_engines` and `gpu_intel_pmu_devices`). It is a single read
section, but its metrics live in two index spaces — per-engine entries are
indexed by a GPU-major engine index, per-device entries by plain GPU id — and a
group's member set applies to every metric tagged with it. Sharing one group
made engine indices look like GPU ids and published phantom all-zero
`gpu_frequency_sample{id="2"}` series on a two-GPU host. Both groups are stamped
from the same bracket, so the two windows are identical.

Note that this PMU reports engine **occupancy, not efficiency** — a busy engine
had work queued, which does not mean its execution units were saturated. Pair
busy time with frequency to distinguish "busy but downclocked" from genuinely
saturated. EU-level detail requires the separate i915 perf/OA interface, and
there is no VRAM bandwidth counter in this PMU.

## Memory

Metrics related to system memory usage.

### memory_meminfo

Memory utilization from /proc/meminfo. These metrics provide a view of system
memory usage, including total memory, free memory, and memory used for various
purposes (buffers, cache). They can be useful for monitoring memory pressure,
identifying potential memory leaks, and understanding how memory is being
utilized across the system.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `memory_total` | The total amount of system memory | |
| `memory_free` | The amount of system memory that is currently free | |
| `memory_available` | The amount of system memory that is available for allocation | |
| `memory_buffers` | The amount of system memory used for buffers | |
| `memory_cached` | The amount of system memory used by the page cache | |
| `memory_dirty` | Page-cache memory dirtied and not yet written back (`Dirty`); what the writeback throttle acts on | |
| `memory_writeback` | Page-cache memory currently being written back (`Writeback`) | |
| `memory_active` | Memory on the active LRU list, reclaimed last (`Active(file)`, `Active(anon)`) | `kind={file,anon}` |
| `memory_inactive` | Memory on the inactive LRU list, reclaimed first (`Inactive(file)`, `Inactive(anon)`) | `kind={file,anon}` |
| `memory_unevictable` | Memory the kernel cannot reclaim (`Unevictable`) | |
| `memory_mlocked` | Memory locked with mlock (`Mlocked`) | |
| `memory_shmem` | tmpfs, shm and shared anonymous memory (`Shmem`); counted inside Cached | |
| `memory_mapped` | Page cache mapped into process address spaces (`Mapped`) | |
| `memory_anon` | Anonymous process memory (`AnonPages`) | |
| `memory_slab` | Slab allocator memory, reclaimable (`SReclaimable`) or not (`SUnreclaim`) | `kind={reclaimable,unreclaimable}` |
| `memory_kernel_reclaimable` | Kernel allocations reclaimable under pressure (`KReclaimable`) | |
| `memory_kernel_stack` | Kernel stacks (`KernelStack`) | |
| `memory_page_tables` | Process page tables (`PageTables`) | |
| `memory_percpu` | Per-CPU allocator memory (`Percpu`) | |
| `memory_swap_total` | Total swap (`SwapTotal`) | |
| `memory_swap_free` | Unused swap (`SwapFree`) | |
| `memory_swap_cached` | Swapped-out memory that is back in RAM with its swap slot still held (`SwapCached`) | |
| `memory_commit_limit` | Memory the overcommit policy allows to be committed (`CommitLimit`); enforced only with `vm.overcommit_memory=2` | |
| `memory_committed` | Memory committed to processes whether touched or not (`Committed_AS`) | |
| `memory_hugepages_anon` | Anonymous memory in transparent huge pages (`AnonHugePages`) | |
| `memory_hugepages_shmem` | Shared memory in transparent huge pages (`ShmemHugePages`) | |
| `memory_hugepages_file` | Page cache in transparent huge pages (`FileHugePages`) | |
| `memory_hugetlb` | Memory reserved for hugetlbfs pages of every size (`Hugetlb`); not in MemAvailable | |
| `memory_hugetlb_pages` | hugetlbfs pages of the default size (`HugePages_Total/Free/Rsvd/Surp`), in pages | `state={total,free,reserved,surplus}` |
| `memory_hardware_corrupted` | Memory retired after a hardware error (`HardwareCorrupted`); non-zero is a failing DIMM | |

A line the running kernel does not print (`HardwareCorrupted` without
`CONFIG_MEMORY_FAILURE`, the huge-page lines without the corresponding
config) leaves its gauge absent from the snapshot rather than at 0.

On macOS the sampler reads the `vm.page_*` and `vm.compressor` sysctls and
`hw.memsize` and `vm.swapusage`, unprivileged. It sets
`memory_total`, `memory_free`, `memory_available`, `memory_cached`,
`memory_anon`, `memory_swap_total` and `memory_swap_free`, and the macOS-only
gauges below; the rest are absent. `memory_free` excludes speculative pages,
as `vm_stat`'s "Pages free" does. `memory_cached` is the file-backed pages.
`memory_anon` omits anonymous pages on the per-CPU local queues, so
`vm_stat`'s "Anonymous pages" can read slightly higher.
`memory_available` has the Linux meaning, memory that can be handed out
without compressing or swapping anonymous memory: free, file-backed and
purgeable pages. `memory_active` and `memory_inactive` are absent because
macOS does not split those lists into file and anonymous pages.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `memory_purgeable` | Unwired anonymous memory in objects marked volatile, which the kernel may discard without saving; counted in `memory_anon` and `memory_available` | |
| `memory_speculative` | File pages read ahead and not yet used; counted in `memory_cached` and `memory_available` | |
| `memory_compressed` | Compressed data the compressor holds in memory, in bytes; excludes segments swapped out and unused space within segments | |
| `memory_compressed_stored` | Uncompressed size of every page the compressor holds, including pages in segments swapped out (`vm_stat`'s "Pages stored in compressor") | |

The two compressor gauges cover different pages, so their ratio is not the
compression ratio. Each is set only when its sysctl is readable.

### memory_pagecache

BPF sampler on the page cache, per filesystem and optionally per cgroup:
buffered read calls and the bytes they ask for, pages filled by what the
filling task was doing, pages evicted, and mmap faults. Reads are one
`fentry` on `filemap_read` (`generic_file_buffered_read` before 5.12): no
`fexit`, no task storage, so a read pays one probe crossing. Fills are the
`mm_filemap_add_to_page_cache` tracepoint at the rate pages come in from the
device, each classified from the task's saved syscall number (a read
syscall, a write syscall, a page fault, or other; every fill is `other` on
kernels before 5.15, which lack `bpf_task_pt_regs`). Evictions are the
delete tracepoint; faults are `fentry` on `filemap_fault`. On aarch64 without
ftrace direct calls (before 6.4) fentry cannot attach: reads, read bytes and
faults read 0, so the hit ratio below is undefined there, and fills and
evictions are recorded. `rezolus status` then shows the sampler unsupported
and names the hooks left out.

**The hit ratio.** `pagecache_pages_added{reason="read"} × 4096 /
pagecache_read_bytes` is the fraction of bytes read that came from the
device, readahead included, so `1 −` that is the page-level hit ratio.
There is no per-call hit or miss and no latency by outcome: the design that
had them bracketed every read with an `fexit` and task storage
(`docs/journal/2026-09-29-pagecache-hit-ratio.md`), and this one keeps the
read path to a single `fentry`.

**Opt-in.** The read hook runs on every buffered read call. It is one of the
samplers the `[defaults]` section never turns on: enable it with
`[samplers.memory_pagecache] enabled = true`. `cgroup_attribution = true`
adds the per-cgroup series at the cost measured for `ext4_ops`.

Per filesystem means the slot registry's mounts (ext4, ext3, ext2, ocfs2,
xfs): every other filesystem, and the block devices' own page cache (inode
tables and directory blocks read through the buffer cache), lands in
`mount="other"`. A large folio counts as its pages on every kernel with
folios, wherever that kernel keeps the order. Read bytes are what each call
could return, clamped at end of file, so a `cat` of a 4 KiB file counts 4
KiB, not its 128 KiB buffer. The syscall table that classifies fills is
written after the programs attach, so fills in the agent's first instant are
`other`.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `pagecache_reads` | Buffered read calls into the page cache, hits and misses alike | `mount`, `fstype`, `devnum`, `block_device` |
| `pagecache_read_bytes` | Bytes those calls could return: requested, clamped at end of file | `mount`, ... |
| `pagecache_pages_added` | Pages added, by the adding task's context: `read` (misses and their readahead), `write` (buffered writes of uncached pages), `fault` (mmap), `other` | `reason={read,write,fault,other}`, `mount`, ... |
| `pagecache_pages_evicted` | Pages removed: reclaim, truncation, invalidation | `mount`, ... |
| `pagecache_faults` | mmap faults served by the page cache, resident or not | `mount`, ... |
| `cgroup_pagecache_reads` | Read calls by the reading task's cgroup (`cgroup_attribution = true`) | `name` |
| `cgroup_pagecache_read_bytes` | Bytes a cgroup's reads could return | `name` |
| `cgroup_pagecache_pages_added` | Pages a cgroup's tasks filled, every reason | `name` |

### memory_slabinfo

Sizes of slab caches from `/proc/slabinfo`, read at most once per `interval`
(60 s by default) off the scrape cycle, since the file is root-only, lists every
cache on the machine and is generated under the slab lock.

`memory_slab_cache_bytes` is published for every cache the file lists, labeled
by `cache`. When `memory_slab` (meminfo's `SReclaimable` and `SUnreclaim`)
grows, this says which cache grew: a `kmalloc-*` size class, a ZFS, KVM or
VFIO cache, or any other. A cache that appears when a module loads gets its
series then, and one that goes away when its module unloads reads absent. A
cache that comes back can take a different slot, so its `id` label can change.
`caches = "fixed"` in `[samplers.memory_slabinfo]` publishes bytes for the
followed caches below only. At most 1024 caches are published; over that, the
followed caches come first and the rest are taken largest first, among the
slots free at that sweep: a slot once held is kept until its cache goes, so a
followed cache that first appears while every slot is held is not published.
A name listed twice is published once, at its larger line. The first
cache left out is logged as a warning, changes in the count at debug, and
fitting again at info. A kernel lists a few hundred caches, so the cap only
bounds what an unusual kernel could produce.

A cache's bytes are its slabs times pages per slab times the page size, free
objects included. The file gives pages per slab at the cache's preferred
order; under memory pressure SLUB allocates slabs at a smaller fallback order,
and those hold fewer pages, so the bytes are an upper bound per cache, exact
while no slab of the cache used the fallback.

`memory_slabinfo` also answers whether the filesystem metadata caches fit: a
filesystem with tens of millions of files whose inode cache is evicted pays a
synchronous inode-table read on `stat` and on every atime update
(`ext4_inode_loads`), and `ext4_inode_cache` here is the cause on the same
axis. `vm.vfs_cache_pressure` trades this memory against the page cache.

Caches whose object counts are followed: `dentry`, `inode_cache`,
`ext4_inode_cache`, `ext4_extent_status`, `jbd2_journal_head`, `buffer_head`,
`xfs_inode`, `radix_tree_node` (the page-cache index). Their object counts read
absent while the file does not list them, as their bytes do. A cache the
running kernel does not have has no series, and neither does one SLUB has
merged into a same-sized pool, which the file lists under one name only with
the pool's totals: caches without a constructor or a usercopy region are
candidates, and `ext4_extent_status` is merged on the Debian 13 6.12 kernel.
The inode and dentry caches have constructors and are never merged. A merged
pool's member names are the entries in `/sys/kernel/slab/` that link to one
directory. Booting with `slab_nomerge` lists every cache under its own name.

Not every slab allocation is in a cache. On SLUB, and on SLAB from 6.1, a
`kmalloc` larger than two pages (`kmalloc-8k` with 4 KiB pages) goes to the
page allocator, and since Linux 5.4 those pages are counted in `SUnreclaim`.
A `CONFIG_SLAB` kernel before 6.1 keeps kmalloc caches up to 4 MiB with 4 KiB
pages (2^(MAX_ORDER + PAGE_SHIFT − 1), at most 32 MiB), so there the
difference below is about zero. With every cache published, `memory_slab`
minus the sum of `memory_slab_cache_bytes` approximates the memory in large
`kmalloc` allocations; since the cache bytes are an upper bound, the difference can
understate it. OpenZFS's SPL creates caches of objects up to
`spl_kmem_cache_slab_limit` (`SPL_MAX_KMEM_ORDER_NR_PAGES` pages: 4 under
SLUB, so 16 KiB with 4 KiB pages) as Linux slab caches, which are listed here
under their ZFS names. Caches of larger objects, such as the larger
`zio_buf_*`, `zio_data_buf_*` and `zio_buf_comb_*` (one cache for data and
metadata when the two would be identical), are SPL's own on `vmalloc`, are in
neither this file nor `memory_slab`, and only `/proc/spl/kmem/slab` lists
them. ARC data in scatter ABDs, the default, comes from the page allocator and
is in neither file.

Measured on a 56-vCPU guest with a 196-line `/proc/slabinfo`, when only the
fixed caches were published: the sweep reads the file in 366–409 µs and parses
it in 51–63 µs, once per interval on the blocking pool; the scrape-path
`refresh()` is 0 µs except for the dispatch. Publishing every cache, measured on
a 16-vCPU arm64 VM (Docker Desktop, kernel 7.0.14) with 216 caches in a release
build: read 106–108 µs, parse and publish 79–87 µs per sweep, the mean of 200
sweeps in each of three runs (`slabinfo::tests::sweep_phase_timing`). On a
32-core x86 host (kernel 6.12.90) with 267 caches, from the sweep's debug line
at a 10 s interval over two minutes: read 1525–1559 µs (28884 bytes), parse
and publish 294–306 µs. The read's cost does not depend on `caches`; parse
and publish does.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `memory_slab_cache_bytes` | Memory the cache holds: slabs times pages per slab times the page size, free objects in those slabs included; an upper bound (see above). Every cache by default; the followed caches only with `caches = "fixed"` | `cache`, `id` |
| `memory_slab_cache_objects` | Objects in a followed cache: `active` are in use, `total` are allocated to the cache's slabs whether in use or free | `cache`, `state={active,total}`, `id` |

### memory_vmstat

Memory NUMA metrics from /proc/vmstat. NUMA (Non-Uniform Memory Access) metrics
can be particularly relevant for multi-socket systems where memory access times
vary depending on which CPU is accessing which memory. These metrics may help
identify inefficient NUMA access patterns that can impact performance on NUMA
systems.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `memory_numa_hit` | The number of allocations made on the intended node: the memory policy's choice, or the node the caller asked for (`numa_hit`) | |
| `memory_numa_miss` | The number of allocations made on a node other than the intended one (`numa_miss`). Summed over all nodes, as `/proc/vmstat` reports it, it equals `memory_numa_foreign` | |
| `memory_numa_foreign` | The number of allocations intended for a node that were made on another node, counted against the intended node (`numa_foreign`). Summed over all nodes, as `/proc/vmstat` reports it, it equals `memory_numa_miss` | |
| `memory_numa_interleave` | The number of interleave-policy allocations made on their intended node (`numa_interleave`) | |
| `memory_numa_local` | The number of allocations made on the node the allocating task was running on (`numa_local`) | |
| `memory_numa_other` | The number of allocations made on a node other than the one the allocating task was running on (`numa_other`) | |
| `memory_pages_dirtied` | Page-cache pages dirtied by writes (`nr_dirtied`); a page dirtied again before writeback counts once | |
| `memory_pages_written` | Page-cache pages written back by any path (`nr_written`): flushers, reclaim and integrity syncs alike. The complete count; `writeback_pages_written` below is only what the flusher's own accounting saw | |
| `memory_dirty_threshold` | The dirty-page limits the writeback throttle compares `Dirty` against, in bytes: `hard` throttles writers, `background` starts the flusher (`nr_dirty_threshold`, `nr_dirty_background_threshold`) | `kind={hard,background}` |
| `memory_reclaim_scanned` | Pages scanned by kswapd or by direct reclaim on the allocating thread (`pgscan_kswapd`, `pgscan_direct`) | `kind={kswapd,direct}` |
| `memory_reclaim_reclaimed` | Pages reclaimed by kswapd or by direct reclaim (`pgsteal_kswapd`, `pgsteal_direct`) | `kind={kswapd,direct}` |
| `memory_allocation_stalls` | Allocations that stalled to run direct reclaim, summed over zones (`allocstall_*`) | |
| `memory_page_faults` | Page faults of every kind (`pgfault`) | |
| `memory_major_page_faults` | Page faults that read the page from storage (`pgmajfault`); a synchronous I/O each | |
| `memory_swap_in` | Pages read back from swap (`pswpin`) | |
| `memory_swap_out` | Pages written to swap (`pswpout`) | |
| `memory_workingset_refaults` | Pages evicted and then needed again while their shadow entry survived (`workingset_refault_file`, `_anon`): a working set that does not fit. Kernel 5.9+ | `kind={file,anon}` |
| `memory_workingset_activations` | Refaulted pages placed straight on the active list (`workingset_activate_file`, `_anon`). Kernel 5.9+ | `kind={file,anon}` |
| `memory_workingset_restores` | Refaulted pages restored to the active list they were evicted from (`workingset_restore_file`, `_anon`). Kernel 5.9+ | `kind={file,anon}` |
| `memory_oom_kills` | Processes killed by the OOM killer (`oom_kill`) | |
| `memory_thp_faults` | Faults served with a transparent huge page, or that fell back to small pages because none could be allocated (`thp_fault_alloc`, `thp_fault_fallback`) | `outcome={allocated,fallback}` |
| `memory_thp_collapses` | Ranges khugepaged collapsed into a huge page (`thp_collapse_alloc`) | |
| `memory_thp_splits` | Huge pages split back into small pages (`thp_split_page`) | |
| `memory_compaction_stalls` | Allocations that stalled to compact memory on the allocating thread (`compact_stall`) | |
| `memory_compactions` | Direct compaction runs by outcome (`compact_success`, `compact_fail`) | `outcome={success,fail}` |
| `memory_numa_balancing_pte_updates` | Page-table entries the NUMA balancer marked to sample locality (`numa_pte_updates`) | |
| `memory_numa_balancing_hint_faults` | NUMA hinting faults on those entries (`numa_hint_faults`) | |
| `memory_numa_balancing_pages_migrated` | Pages the NUMA balancer moved to the accessing node (`numa_pages_migrated`) | |

Counters are in pages or events; the two thresholds are gauges in bytes. A
line the running kernel does not print (NUMA counters without NUMA, THP
counters without `CONFIG_TRANSPARENT_HUGEPAGE`, the working-set split before
5.9) leaves its metric absent rather than 0.

On macOS the sampler reads `host_statistics64` (the call `vm_stat` makes),
unprivileged, for the values with no sysctl in Apple's published kernel
source. It sets `memory_page_faults`, `memory_swap_in` and `memory_swap_out`,
and the macOS-only metrics below; the rest are absent.
Swap on macOS holds compressed memory, so the swap counters are kernel pages
(16 KiB on Apple silicon) of compressed data moved to and from the swap files,
not pages of memory as on Linux. There is no major-fault count;
`memory_pageins` is file pages only.

`host_statistics64` is rate-limited for every process that is not an Apple
platform binary: together they get 2 to 10 calls a second, and a call over
that returns the last result any of them fetched. A read is published only if
it has more faults than the sampler's last published read and none of its
counters is lower; otherwise that interval has no point, rather than a rate
of zero or a counter going backwards. A newer copy fetched by another process
cannot be detected; it is from the current one-second window.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `memory_wired` | Memory wired down: it cannot be paged out or compressed (`wire_count`, which on macOS also counts throttled pages and free low-memory pages); a gauge, in bytes | |
| `memory_compressions` | Pages compressed into the compressor | |
| `memory_decompressions` | Pages decompressed out of the compressor | |
| `memory_pageins` | Pages read in from files by the pager | |
| `memory_pageouts` | Pages written out to files by the pager | |

### memory_writeback

BPF sampler on the kernel's page-cache writeback tracepoints. Reports the
sleeps the dirty-page throttle imposes on writers, the flusher work items by
the reason they ran, and the pages written back. Filesystem-agnostic: these
are the `mm` layer's tracepoints, and `memory_dirty` / `memory_writeback` in
`memory_meminfo` are the gauges these rates act on.

The throttle tracepoint (`balance_dirty_pages`) fires only once dirty pages are
over the free-run ceiling, midway between the background and hard limits, so
`writeback_throttle_checks` is zero while dirty pages stay comfortably under
the limit and rises only when the throttle is being considered;
`writeback_throttle_events` counts the evaluations that made the writer sleep.
The sleep is a raw argument in jiffies, converted with the measured tick, so
the histogram has one-jiffy resolution. That tracepoint has had
two argument lists across kernel versions, and the sampler picks the program
written for the one BTF reports; a kernel with neither, or without BTF, runs
without the throttle metrics and reports the sampler unsupported, naming
writeback throttling, rather than reading the wrong argument.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `writeback_throttle_latency` | Distribution of the time a writer was made to sleep by the dirty-page throttle, in nanoseconds at one-jiffy resolution; sleeps only | |
| `writeback_throttle_checks` | Throttle evaluations of tasks whose dirty pages were over the free-run ceiling | |
| `writeback_throttle_events` | Evaluations that made the writer sleep | |
| `writeback_throttled_time` | Total writer sleep in the throttle, in nanoseconds | |
| `writeback_runs` | Flusher passes (one work item covering many inodes makes several), by why the work ran: `background` (dirty pages over the background threshold), `periodic` (the `dirty_writeback_centisecs` flusher), `sync`, `vmscan` (memory reclaim), `laptop_timer`, `fs_free_space`, `forker_thread`, `foreign_flush` (cgroup writeback) | `reason={background,vmscan,sync,periodic,laptop_timer,fs_free_space,forker_thread,foreign_flush}` |
| `writeback_pages_written` | Pages the flushers reported written back, summed over flusher wakeups | |

## Network

Metrics related to network performance. These metrics provide insights into
network traffic, error rates, packet processing, and overall network health.

### network_ethtool

NIC driver statistics read via ethtool ioctls, the same mechanism as
`ethtool -S`. On AWS EC2 instances with ENA interfaces these expose the
allowance-exceeded counters that indicate instance-level rate limiting. On
hosts with no ENA interface the sampler reports as unsupported and produces
nothing.

Every metric is **per interface**: each carries an `id` (the interface's slot)
and an `interface` label (its name, e.g. `eth0`). A host with several ENIs
reports each separately — which ENI is being throttled is the question these
counters exist to answer.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `network_ena_bandwidth_allowance_exceeded` | Packets queued or dropped due to the bandwidth allowance being exceeded | `direction={receive,transmit}`, `id`, `interface` |
| `network_ena_pps_allowance_exceeded` | Packets queued or dropped due to the PPS allowance being exceeded | `id`, `interface` |
| `network_ena_conntrack_allowance_exceeded` | Packets dropped due to the connection tracking allowance being exceeded | `id`, `interface` |
| `network_ena_linklocal_allowance_exceeded` | Packets dropped due to the link-local PPS allowance being exceeded | `id`, `interface` |

`id` is assigned from a name-sorted enumeration of interfaces, so it is stable
across agent restarts for an unchanged set of interfaces. Adding or removing a
NIC renumbers the ones after it — use `interface` to follow a series across
that.

### network_interfaces

Produces network interface statistics from /sys/class/net for TX/RX errors.
These metrics can help monitor network interface health.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `network_carrier_changes` | The number of times the link has changes between the UP and DOWN states | |
| `network_receive_errors_crc` | The number of packets received which had CRC errors | |
| `network_receive_dropped` | The number of packets received but not processed. Usually due to lack of resources or unsupported protocol. Does not include hardware interface buffer exhaustion. | |
| `network_receive_errors_missed` | The number of packets missed due to buffer exhaustion | |
| `network_transmit_dropped` | The number of packets dropped on the transmit path. Usually due to lack of resources. | |

### network_traffic

Basic network traffic statistics.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `network_bytes` | Bytes transferred, counted once per interface traversed — **reads high on stacked interfaces**, see below | `direction={receive,transmit}` |
| `network_packets` | Packets transferred, counted once per interface traversed — **reads high on stacked interfaces**, see below | `direction={receive,transmit}` |
| `network_host_bytes` | Bytes crossing this host's network boundary, counted once at the interface bound to a device driver | `direction={receive,transmit}` |
| `network_host_packets` | Packets crossing this host's network boundary, counted once at the interface bound to a device driver | `direction={receive,transmit}` |

#### Which pair to use

The two pairs count at different layers, and neither is a drop-in replacement
for the other.

`network_bytes` / `network_packets` count **every netdev the packet is handed
to**. The kernel fires `net_dev_start_xmit` once per `net_device`, not once per
packet leaving the host, so on a stacked netdev each layer is counted:

```
bond0  -> xmit_one() -> trace_net_dev_start_xmit(skb, bond0)   # counted
  eth0 -> xmit_one() -> trace_net_dev_start_xmit(skb, eth0)    # counted again
```

A bonded host therefore reports roughly **2x** its real egress on these, and
VLAN-over-bond reports 3x. The receive side is not symmetric —
`trace_netif_receive_skb()` sits above the `another_round:` label in
`__netif_receive_skb_core()`, and bonding, bridging and VLAN untagging all
re-enter below it — so RX is counted once while TX is inflated. Use these when
you want stack activity, not a traffic total.

`network_host_bytes` / `network_host_packets` count the same traffic **once**,
at the interface bound to a device driver. This is the number to put on a
"network throughput" dashboard and the one to compare against a NIC's own
counters.

#### What "host boundary" means, and does not

The predicate is the presence of a bus parent (`SET_NETDEV_DEV()` — the same
thing `/sys/class/net/<if>/device` reflects). Verified against Linux v6.12:
bond, VLAN, bridge, veth, tun/tap, macvlan, ipvlan, vxlan, dummy and loopback
have none; `virtio_net` has one.

**This is not a claim that the frame reached a wire.** On bare metal the two
coincide. In a VM guest the boundary is the hypervisor: the guest counts its
own egress at `virtio_net`, but the hypervisor may hairpin that frame to
another guest on the same box without it ever touching a link. The guest cannot
distinguish the two cases, so the metric claims only what it can support.

Known scope limits:

- **Container east-west traffic is not counted.** A pod-to-pod flow that stays
  on the node (veth -> bridge -> veth) touches no bus-parented device. It
  appears in `network_bytes` (several times over) and not at all in
  `network_host_bytes`. If that traffic matters to you, `network_bytes` is the
  only thing that currently sees it.
- **VM east-west is not a gap in the same way.** Each guest counts its own side
  at `virtio_net`, so a fleet running the agent inside guests still sees it;
  the hypervisor correctly reports zero, because nothing left the box.
- **SR-IOV VF traffic is invisible to the hypervisor.** With a VF passed
  through to a guest, the host has no netdev for it at all, so no tracepoint
  fires — the host is blind to it both before and after this metric existed.
  Guests count their own VF (it has a PCI parent), but VF-to-VF traffic
  hairpinned by the NIC's embedded switch is counted by the guests without
  consuming link bandwidth. Seeing real link utilization under SR-IOV requires
  PF-side hardware counters, which Rezolus does not yet collect.

#### network_traffic on macOS

On macOS the sampler reads each interface's counters, unprivileged, with the
`IFMIB_IFDATA` sysctl (`net.link.generic.ifdata`).
`network_bytes` and `network_packets` sum every interface: loopback, bridges,
`utun` tunnels and the `vmenet` interfaces of virtual machines included, so
a packet that crosses a bridge and its member interface is counted on each.
`network_host_bytes` and `network_host_packets` sum two kinds of interface:

- the `IONetworkInterface` entries in the I/O registry that have no
  `IOResources` ancestor: wifi, Ethernet on PCIe, Thunderbolt or USB, and
  Thunderbolt networking;
- the interfaces with no delegate interface whose functional type is wifi
  or cellular, which adds `awdl0` (Apple Wireless Direct Link, used by
  AirDrop), `llw0` (low-latency WLAN) and `ap1`, all on the wifi radio.

They exclude `lo0`, bridges and `utun` tunnels, which have no
`IONetworkInterface`, and `vmenet` interfaces, which are software devices
under `IOResources`. A tunnel delegated to a wifi interface reports wifi as
its functional type, and is excluded because it has a delegate.

The counters start at zero when the sampler starts and add each interface's
increase since the previous read. An interface first listed after the
sampler started adds its whole count. When an interface goes away, its
traffic after the last read before that is not counted.

## Scheduler

Metrics related to the Linux kernel scheduler. The scheduler is responsible for
allocating CPU time to processes, and its behavior directly impacts application
responsiveness, throughput, and overall system performance. These metrics
provide insights into how efficiently the scheduler is managing processes and
CPU resources.

### scheduler

Instruments scheduler events, with one program on each scheduler hook, and
measures runqueue latency, process running time, context switches and CPU
migrations. It replaced the `scheduler_runqueue` and `cpu_migrations`
samplers, which each had a program on `sched_switch` (see
docs/journal/2026-10-03-one-program-per-hook.md); a config that still names
them is read as `[samplers.scheduler]`, with a warning. Its two parts are the
options `runqueue` and `migrations`, both on by default, and
`cgroup_attribution` applies to both.

The runqueue part measures runqueue latency, process running time, and
context switch information. These metrics help understand how long
processes wait before getting CPU time, how long they run once scheduled, and
how frequently they're switched out. High runqueue latencies can indicate CPU
contention or scheduling inefficiencies that directly impact application
performance and responsiveness.

On kernels whose BTF shows `sched_switch` with a `prev_state` argument
(mainline 5.18 and later, built with BTF), whether a switch was voluntary and
whether the task went into uninterruptible sleep come from that argument,
the state `__schedule()` read from the task before deciding whether to block
it. A sleep cancelled by a pending signal counts as involuntary, since the
task stays queued. Otherwise the state is read from the task, where a wakeup that lands
during the switch may already have replaced it. There
`scheduler_uninterruptible` and the `*_uninterruptible_wait` counters can
undercount short uninterruptible sleeps by an amount that depends on the
workload.

On every kernel, a switch that preempts the task counts as involuntary
whatever the task's state, as the kernel counts it: a preempted task stays on
the runqueue, and its runqueue latency is measured from the preemption.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `scheduler_runqueue_latency` | Distribution of the amount of time tasks were waiting in the runqueue | |
| `scheduler_running` | Distribution of the amount of time tasks were on-CPU | |
| `scheduler_offcpu` | Distribution of the amount of time tasks were off-CPU | |
| `scheduler_uninterruptible` | Distribution of the amount of time tasks were off-CPU in uninterruptible sleep (D state), a subset of `scheduler_offcpu` | |
| `scheduler_uninterruptible_wait` | Time tasks spent in uninterruptible sleep, added in full when a task next runs, on the CPU it runs on. Over windows much longer than the sleeps, the rate summed across CPUs is the mean number of tasks in D state; a long sleep shows as a spike when it ends | `id`: the CPU |
| `scheduler_context_switch` | The number of involuntary context switches | `kind=involuntary` |

The migrations part tracks when tasks move from one CPU to another, per CPU
and per cgroup, to show which containers might be experiencing high rates of
CPU migration.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_migrations` | The number of CPU migrations | `direction={from,to}` |
| `cgroup_cpu_migrations` | The number of CPU migrations on a per-cgroup basis | `name`: the name of the cgroup |

## Hardware Sensors

The Linux `hw_sensors` sampler monitors hardware health: temperature, power,
voltage, current, fans, and cooling state. It reads thermal zones, hwmon
channels, and thermal cooling devices. It follows `[defaults]` like other
samplers; to set its read interval or turn it off:

```toml
[samplers.hw_sensors]
interval = "5s"
# enabled = false
```

Reads run in a nonoverlapping blocking task, dispatched by consumer activity
at most once per interval (5 seconds by default). Discovery runs every 60
seconds during sampling; descriptive metadata is cached between passes.
Failed, disabled, faulted, removed or unreadable measurements are absent from
the live endpoint rather than zero. If every channel in a family fails, its
last successful acquisition window remains unchanged; a recorder that
deduplicates windows can retain the previous observation without an outage
marker. A sysfs read can invoke hardware access, so a slow driver costs the
background worker time on every sweep, not the scrape path. `nvme` and
`drivetemp` hwmon chips and ACPI `Processor` cooling devices (one per logical
CPU) are not read. Each drive hwmon read sends the drive a command, and
`drivehealth` reads each drive's temperature at its own interval; for NVMe that
is the Composite temperature, and the per-sensor temperatures the `nvme` hwmon
chip also exposes are not collected. `drivehealth` needs root to open the drive
devices, so an agent that does not run as root has no drive temperatures. A
family holds at most 256 sensor identities; a host with more temperature
channels than that, such as `coretemp` on a 256-core part, logs a warning and
omits the rest.

Measured with rezolus 5.28.0 on 2026-10-21, with only `hw_sensors` and
`rezolus_rusage` enabled, at a 5 second interval, one scrape per second for
150 seconds, against the same run with it off
(docs/journal/2026-09-17-sensors.md has the full data):

| Host | Values | Sweep p50 / max | Extra agent CPU |
| --- | ---: | --- | --- |
| EPYC 4564P, `bnxt_en` NIC (incl. 32 `Processor` cooling devices) | 41 | 21.9 / 45.7 ms | 120 ms (0.08% of a core) |
| Threadripper 1950X, 2 NVMe (incl. NVMe hwmon and 32 `Processor` cooling devices) | 44 | 26.6 / 33.4 ms | 40 ms (0.03%) |
| Threadripper 3970X, ASUS ROG Zenith II Extreme (incl. NVMe hwmon and 64 `Processor` cooling devices) | 93 | 323 / 354 ms | 83 ms (0.06%) |
| Raspberry Pi 4B (×2) | 4 | 0.17–0.18 / 0.36–11.8 ms | at most 34 ms (0.02%); the controls differed by up to 22 ms |

Most of the EPYC sweep is one `bnxt_en` port's temperature, at 20.9 ms per
read (median of 100); the other port takes 0.7 ms. The 1950X run
predates the `nvme` skip: its NVMe hwmon channels read at 7.6 and 14.4 ms
(median of 100, 10 ms apart), and its sweeps include them. Nearly all of the
3970X sweep is the `asusec` embedded-controller driver: one read per refresh
takes about 300 ms, and the rest about 30 µs; its NIC's MAC temperature adds
about 18 ms. About 2.8 ms of each sweep is CPU; the
rest is the worker waiting.

`asusec` reports -40 °C for a temperature header with nothing connected.
Such a channel gets no slot and is not read on each sweep; discovery reads it
every 60 seconds and adds it once it reports another value.

Each family identifies a native channel with `sensor`, `source`, `chip`,
`channel`, and a native `label` when supplied by the kernel. `device` is the
last component of the device path: a PCI address such as `0000:0b:00.1`, a
platform or I2C device, `thermal_zoneN`, `cooling_deviceN`, or `hwmonN` for an
hwmon with no parent device (that one can renumber across reboots). `name` is a readable name, unique
within the family while the devices' paths differ in their last component, `<chip> <label or channel> <device>`, for example
`k10temp Tctl 0000:00:18.3`; when a driver repeats a label across channels,
the channel is added, `<chip> <label> (<channel>) <device>`. The Hardware
Sensors charts group by `name` when the recording has it and by `sensor`
otherwise; a recording that mixes both (it spans an agent upgrade, or combines
hosts on different versions) sums the series without `name` into one line. `board_model` and
`soc_compatible` preserve platform identity when available; `scope` records
verified board-specific interpretation. Derived power carries
`derived=voltage_x_current`.
Unknown boards retain native readings without inferred CPU/GPU associations.
Sensor slots are never reused for a different identity during the process;
each family supports at most 256 lifetime identities, reporting overflow.

| Metric | Type | Stored unit / interpretation |
| --- | --- | --- |
| `sensor_temperature` | Gauge | Millidegrees Celsius per temperature channel; signed |
| `sensor_power` | Gauge | Microwatts per channel; direct hwmon reading or checked INA3221 voltage × current |
| `sensor_voltage` | Gauge | Millivolts per bus-voltage channel; excludes INA3221 shunt-voltage channels |
| `sensor_current` | Gauge | Milliamps per current channel |
| `sensor_fan_speed` | Gauge | Measured revolutions per minute, including NVIDIA `pwm_tach/rpm` |
| `sensor_fan_pwm` | Gauge | Fan command on a 0–255 scale, not measured rotation |
| `sensor_cooling_state` | Gauge | Driver-defined cooling-state index, not a throttling percentage |

The Hardware Sensors dashboard plots every family separately by sensor, converting
temperature to Celsius, electrical measurements to W/V/A, and PWM to a fraction.
Native sensor sources can overlap thermal zones or existing GPU/drive readings;
these are distinct source series, not additive measurements. Power rails may
also overlap. Derived INA3221 power uses sequential voltage/current reads and
does not claim an atomic electrical sample.

The captured Thor fixture exposes GPU, CPU, two SoC thermal zones, and a junction
zone; INA3221 rail measurements; INA238 input power; fan PWM; and cooling states.
Thor INA238 input includes module and carrier, unlike Orin NX/Nano's module-only
VDD_IN. Orin layouts have synthetic test coverage, not hardware validation.
No NIC temperature is inferred from unlabeled external sensors.

This first sampler does not record configurable limits, trip points, fan modes,
or overcurrent event histories. A zero cooling state does not establish the
absence of every form of hardware throttling. See [hardware validation and
inventory commands](sensors-validation.md) for the remaining validation work.

## Syscall

Metrics related to system calls. System calls are the interface between user
applications and the kernel. These metrics provide visibility into how
applications are interacting with the operating system, helping identify
inefficient patterns, excessive system call usage, or system call latency issues
that can impact performance.

### syscall

Instruments syscall enter and exit, with one program on each, to gather
syscall counts and syscall latency distributions. It replaced the
`syscall_counts` and `syscall_latency` samplers, which each had a program on
`sys_enter` (see docs/journal/2026-10-03-one-program-per-hook.md); a config
that still names them is read as `[samplers.syscall]`, with a warning. Its
two parts are the options `counts` and `latency`, both on by default, and
`cgroup_attribution` applies to the counts.

The counts help to identify excessive system calls or unexpected patterns of
system call usage.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `syscall` | The number of syscalls by operation type on a per-CPU basis | `op={other,read,write,poll,lock,time,sleep,socket,yield,filesystem,memory,process,query,ipc,timer,event,sync}`, `id`: the CPU the syscall entered on |
| `cgroup_syscall` | The number of syscalls by operation type on a per-cgroup basis | `op={other,read,write,poll,lock,time,sleep,socket,yield,filesystem,memory,process,query,ipc,timer,event,sync}`, `name`: the name of the cgroup | |

The latency distributions track how long system calls take to complete,
which can reveal performance issues in the kernel or resource contention.
High system call latencies may indicate system-level bottlenecks.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `syscall_latency` | Distribution of syscall latency | `op={other,read,write,poll,lock,time,sleep,socket,yield,filesystem,memory,process,query,ipc,timer,event,sync}` |

The `sync` class is `fsync`, `fdatasync`, `sync`, `syncfs` and `msync`: the
calls that block until data reaches the device. They are kept apart from
`filesystem` (metadata calls that usually complete from cache) and `memory`
so that their latency, a device round trip, is visible on its own. The class
table is `syscall_lut()` in `src/agent/samplers/syscall/linux/mod.rs`.

## TCP

Metrics related to TCP connections and performance.

### tcp_connect_latency

Measures the latency for establishing TCP connections. High connect latencies
may indicate network congestion, DNS resolution problems, or overloaded servers.
These metrics are particularly valuable for monitoring client-side connection
performance.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `tcp_connect_latency` | Distribution of latency for establishing outbound connections (active open) | |

### tcp_mib

TCP counters the kernel keeps as SNMP MIBs, read from the `TcpExt` section of
`/proc/net/netstat` and the `Tcp` section of `/proc/net/snmp` once per refresh.
These are counted by the kernel at each event site and are exposed only through
those two files, so this sampler reads procfs rather than attaching probes.
Each section is printed as a line of column names and a line of values; the
columns vary by kernel version and are matched by name. A column the running
kernel does not print leaves its metric absent rather than 0. The counts cover
the agent's network namespace and include IPv4 and IPv6 together.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `tcp_delayed_acks` | Delayed-ACK timer firings that sent an ACK (`DelayedACKs`), including firings first deferred because the socket was owned by the application | |
| `tcp_delayed_ack_deferrals` | Delayed-ACK timer firings that found the socket owned by the application and deferred the ACK until it was released (`DelayedACKLocked`) | |
| `tcp_duplicate_data_segments` | Duplicate data segments received (`DelayedACKLost`): ending at or below the next expected sequence number, or starting below it and failing the sequence or PAWS check. On each one the kernel sends its next ACKs immediately, up to 16 (quick-ACK mode). An accepted partial overlap is not counted; it gets a D-SACK (`tcp_dsacks_sent{kind="old"}`) |  |
| `tcp_rto_expirations` | Retransmission timer expirations plus SYN-ACK timeouts on pending connection requests (`TCPTimeouts`); not counted while the peer advertises a zero window | |
| `tcp_loss_probes` | Tail loss probes sent (`TCPLossProbes`) | |
| `tcp_loss_probe_recoveries` | Losses repaired by a tail loss probe that retransmitted the last segment, instead of an RTO (`TCPLossProbeRecovery`); probes that sent new data are not counted |  |
| `tcp_loss_retransmits` | Segments marked lost and retransmitted from the retransmit queue: `fast` during fast recovery, `slow_start` in the loss state entered on an RTO or a path MTU reduction (`TCPFastRetrans`, `TCPSlowStartRetrans`) | `kind={fast,slow_start}` |
| `tcp_retransmits_lost` | Retransmitted segments detected lost again (`TCPLostRetransmit`) | |
| `tcp_spurious_rtos` | RTOs that F-RTO found spurious and undid (`TCPSpuriousRTOs`) | |
| `tcp_fast_recoveries` | Entries into fast recovery, by whether the connection uses SACK (`TCPSackRecovery`, `TCPRenoRecovery`) | `kind={sack,reno}` |
| `tcp_dsacks_received` | ACKs received whose first SACK block is a D-SACK: below the cumulative ACK (`old`), or above it inside the second SACK block (`out_of_order`). The peer received a segment twice (`TCPDSACKRecv`, `TCPDSACKOfoRecv`) | `kind={old,out_of_order}` |
| `tcp_dsacks_sent` | D-SACKs sent for duplicate data below the next expected sequence number, or already held out of order (`TCPDSACKOldSent`, `TCPDSACKOfoSent`) | `kind={old,out_of_order}` |
| `tcp_zero_window_transitions` | Segments sent advertising a zero window after a non-zero one (`closed`), or a non-zero window after a zero one (`reopened`) (`TCPToZeroWindowAdv`, `TCPFromZeroWindowAdv`). A zero window advertised because the socket could not be charged memory for received data is not counted | `kind={closed,reopened}` |
| `tcp_zero_window_withheld` | Window selections that computed zero but advertised the current window, because an offered window is not shrunk (`TCPWantZeroWindowAdv`) | |
| `tcp_zero_window_drops` | Data segments dropped because the receive window was zero (`TCPZeroWindowDrop`) | |
| `tcp_prune_attempts` | Runs of the receive-queue pruner on a socket that could not be charged memory for an incoming segment (`PruneCalled`) | |
| `tcp_prune_failures` | Prune runs that could not bring receive memory under the buffer limit (`RcvPruned`). Usually followed by a drop counted in `tcp_rcvbuf_drops`; an in-order segment arriving at an empty receive queue is queued anyway |  |
| `tcp_out_of_order_prunes` | Times out-of-order data was discarded to free receive memory, once per pass that removed something, including passes from the retry loop after a prune (`OfoPruned`); can exceed `tcp_prune_attempts` |  |
| `tcp_collapsed_buffers` | Socket buffers freed by copying queued data into fewer, fuller buffers (`TCPRcvCollapsed`) | |
| `tcp_backlog_drops` | Segments dropped because the socket was owned by the application and its backlog plus receive memory exceeded twice the receive buffer plus half the send buffer plus 64 KiB, or the segment came from pfmemalloc reserves (`TCPBacklogDrop`) |  |
| `tcp_rcvbuf_drops` | Segments dropped because the socket could not be charged the memory to queue them: in order with a non-empty receive queue, or out of order (`TCPRcvQDrop`, `TCPOFODrop`) | `kind={in_order,out_of_order}` |
| `tcp_out_of_order_queued` | Segments placed in the out-of-order queue, including segments then discarded because the queue already held their data (`TCPOFOQueue`) |  |
| `tcp_memory_pressure_entries` | Times TCP entered memory pressure: total memory over the `tcp_mem` pressure threshold, or an skb or page allocation for sending failed (`TCPMemoryPressures`). The state is host-wide; the count lands in the namespace of the socket that triggered it |  |
| `tcp_autocorks` | Times a send held back a small segment while earlier data from the socket was still in the qdisc or device (`TCPAutoCorking`) | |
| `tcp_listen_overflows` | SYNs and handshake-completing ACKs dropped because the accept queue was full (`ListenOverflows`) | |
| `tcp_listen_drops` | Connection requests and handshake-completing ACKs a listener dropped for any reason, most overflows included (`ListenDrops`) | |
| `tcp_aborts` | Connections aborted, by reason: data after close, a zero-timeout `SO_LINGER` close, or an orphan with negative `TCP_LINGER2` having its FIN acknowledged (`data`); retries, probes or `TCP_USER_TIMEOUT` running out (`timeout`); orphans killed over `tcp_max_orphans` or the `tcp_mem` hard limit (`memory`); close with unread data (`close`); close in FIN-WAIT-2 with negative `TCP_LINGER2` (`linger`) (`TCPAbortOnData`, `TCPAbortOnTimeout`, `TCPAbortOnMemory`, `TCPAbortOnClose`, `TCPAbortOnLinger`) | `reason={data,timeout,memory,close,linger}` |
| `tcp_open_failures` | Connection attempts that failed: SYN-SENT or SYN-RECV to CLOSED, and pending passive opens reset (`AttemptFails`) | |
| `tcp_established_resets` | Connections moved to CLOSED directly from ESTABLISHED or CLOSE-WAIT (`EstabResets`) | |
| `tcp_input_errors` | Segments received in error (`InErrs`): a bad checksum, a data offset below the 20-byte minimum, a segment shorter than its header on the established fast path, or an unexpected SYN on an established connection |  |
| `tcp_checksum_errors` | Segments received with a bad checksum, also in `tcp_input_errors` (`InCsumErrors`) | |
| `tcp_resets_sent` | Segments sent with RST (`OutRsts`) | |
| `tcp_segments_retransmitted` | Segments retransmitted, in TSO/GSO segments, including tail loss probes and SYN-ACK retransmissions (`RetransSegs`) | |
| `tcp_established_connections` | Connections in ESTABLISHED or CLOSE-WAIT, a gauge (`CurrEstab`) | |

On macOS, see [TCP on macOS](#tcp-on-macos).

`tcp_segments_retransmitted` and `tcp_retransmit` (below) count different
things. The kernel adds to `RetransSegs` the segments of each retransmission
it hands to the transmit path, from every path including tail loss probes and
SYN-ACKs. `tcp_retransmit` adds, on entry to `tcp_retransmit_skb`, the
caller's upper bound on the segments to retransmit in that call, which can
exceed the number retransmitted and is counted even when the call returns
before retransmitting; tail loss probes and SYN-ACKs do not go through that
function. The probe also counts every network namespace on the host, while
`RetransSegs` covers only the agent's.

### tcp_packet_latency

Measures latency from a packet being received until application reads from the
socket. This metric captures how quickly applications respond to incoming data,
which can reveal application processing bottlenecks or inefficient socket read
patterns. High latencies here often indicate that applications aren't reading
from sockets promptly, which can lead to increased memory usage and network
bottlenecks.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `tcp_packet_latency` | Distribution of latency from a socket becoming readable until a userspace read | |

### tcp_receive

Measures jitter and smoothed round trip time for TCP connections. These metrics
provide insights into network stability and latency. Jitter (variation in
latency) can severely impact real-time applications like video conferencing or
online gaming, while SRTT (Smoothed Round Trip Time) helps understand overall
network latency conditions that affect all TCP-based communications.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `tcp_jitter` | Distribution of TCP latency jitter | |
| `tcp_srtt` | Distribution of TCP smoothed round-trip time | |

### tcp_retransmit

Counts TCP packet retransmissions. High retransmission rates may indicate
network congestion, packet loss, or connectivity issues that degrade network
performance and efficiency.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `tcp_retransmit` | The number of TCP packets that were re-transmitted | |

On macOS, see [TCP on macOS](#tcp-on-macos).

### tcp_traffic

Samples TCP traffic to get metrics for TX/RX bytes and packets. These metrics
track the volume of TCP traffic, providing visibility into how much data is
being transferred over TCP connections. They help monitor application network
usage patterns, identify unexpected traffic spikes, and correlate application
behavior with network activity.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `tcp_bytes` | The number of bytes transferred over TCP | `direction={receive,transmit}` |
| `tcp_packets` | The number of packets transferred over TCP | `direction={receive,transmit}` |
| `tcp_size` | Distribution of the size of TCP packets | `direction={receive,transmit}` |

On macOS, see [TCP on macOS](#tcp-on-macos).

#### TCP on macOS

On macOS, `tcp_mib`, `tcp_retransmit` and `tcp_traffic` read the kernel's
TCP statistics, the `net.inet.tcp.stats` sysctl that `netstat -s -p tcp`
prints, on each refresh and, while `tcp_traffic` runs, every 250 ms. The
kernel returns zeros to a process not running as root while
`net.inet.tcp.disable_access_to_stats` is set, which is the default, so the
agent needs root for them; without it the three samplers report themselves
unsupported. The counters cover IPv4 and IPv6 together.

| Metric | XNU counter |
|--------|-------------|
| `tcp_retransmit`, `tcp_segments_retransmitted` | `tcps_sndrexmitpack`: retransmissions of data sent |
| `tcp_delayed_acks` | `tcps_delack`: delayed-ACK timer firings that sent an ACK |
| `tcp_rto_expirations` | `tcps_rexmttimeo`: retransmission timer expirations |
| `tcp_checksum_errors` | `tcps_rcvbadsum`: segments received with a bad checksum |
| `tcp_input_errors` | `tcps_rcvbadsum`, `tcps_rcvbadoff` and `tcps_rcvshort`: a bad checksum, a bad data offset, or a segment shorter than its header or not pulled up for lack of mbufs |
| `tcp_bytes{direction="transmit"}` | `tcps_sndbyte`: data bytes sent for the first time |
| `tcp_bytes{direction="receive"}` | `tcps_rcvbyte` and `tcps_rcvoobyte`: data bytes of segments received in order with nothing queued for reassembly, and of segments received out of order |

The other metrics of these samplers are not set.

- `tcps_sndrexmitpack` counts a retransmission sent with TSO once however
  many segments it spans, so after an RTO it can be lower than the segments
  sent again, and it counts data only, so a retransmitted SYN or SYN-ACK is
  not included.
- `tcp_bytes{direction="receive"}` leaves out a segment that starts at the
  next expected byte while later data is queued for reassembly, mostly a
  retransmission from the peer or a reordered original, so it undercounts
  while the connection is losing or reordering packets. An
  out-of-order segment is counted in full before the kernel trims its
  overlap with segments already queued after it, so overlapping bytes are
  counted twice.
- `tcp_bytes` counts bytes when segments are sent and received; Linux counts
  them at the socket calls. `tcp_packets` and `tcp_size` count socket calls
  on Linux, which the macOS kernel does not count, and are not set.

The kernel's counters are 32 bits wide, and it updates them without
atomics, so a read can be higher than the next one. The agent keeps one
64-bit total per counter, shared by the three samplers, which starts at
zero at the first read and adds the increase since the previous read. A
fall of less than 256 MiB (2^28) is taken as a read lower than the previous
one and adds nothing, so a total is exact while its counter advances by less
than 2^32 - 2^28 between two reads. A larger advance loses a multiple of
2^32, and when it ends within 2^28 short of a multiple of 2^32 the total
holds until the counter advances by up to 256 MiB more. Reads happen when
the agent is scraped or streamed, and, while `tcp_traffic` runs, every 250
ms on a thread of the agent's own, which keeps the totals exact below 128
Gbit/s, less when the thread is delayed. Without `tcp_traffic`, the
counters `tcp_mib` and `tcp_retransmit` read count events, which stay far
below 2^32 between reads at any regular scrape or stream interval; an agent
left unread for days can lose a multiple of 2^32 on a busy counter.

## Rezolus

Metrics about Rezolus itself. These metrics provide visibility into Rezolus's
own resource consumption and performance. They're useful for monitoring the
overhead of Rezolus itself.

### rezolus_rusage

Samples resource utilization for Rezolus itself. This sampler tracks Rezolus's
CPU usage, memory consumption, and I/O operations.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `rezolus_cpu_usage` | The amount of CPU time Rezolus was executing | `state={user,system}` |
| `rezolus_memory_usage_resident_set_size` | The total amount of memory allocated by Rezolus | |
| `rezolus_memory_page_reclaims` | The number of page faults which were serviced by reclaiming a page | |
| `rezolus_memory_page_faults` | The number of page faults which required an I/O operation | |
| `rezolus_blockio_operations` | The number of filesystem operations | `op={read,write}` |
| `rezolus_context_switch` | The number of context switches | `kind={voluntary,involuntary}` |
