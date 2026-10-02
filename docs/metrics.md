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
| `cpu_migrations` | on | `cgroup_cpu_migrations` |
| `cpu_perf` | on | `cgroup_cpu_cycles`, `cgroup_cpu_instructions` (off also drops the `sched_switch` program) |
| `cpu_tlb_flush` | on | `cgroup_cpu_tlb_flush` |
| `scheduler_runqueue` | on | `cgroup_scheduler_runqueue_wait`, `cgroup_scheduler_offcpu`, `cgroup_scheduler_context_switch` |
| `syscall_counts` | on | `cgroup_syscall` |
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
| `syscall_counts`, `sys_enter` | 33–39 ns | 24–27 ns | | |
| `cpu_tlb_flush`, `tlb_flush` | 180–197 ns | 36–39 ns | 293–352 ns | 72–76 ns |
| `scheduler_runqueue`, `sched_switch` | 491–575 ns | 213–256 ns | 629–775 ns | 240–349 ns |
| `cpu_usage`, `cpuacct_account_field` | 502–599 ns | 285–351 ns | 416–776 ns | 238–518 ns |
| `cpu_migrations`, `sched_switch` | 77–98 ns | 77–90 ns | 106–126 ns | 107–132 ns |
| `ext4_ops`, fsync and write end hooks | | | 531–537 ns | 266–271 ns |

`syscall_counts` reads the task group once, as direct loads from a BTF task
pointer, from a `tp_btf` program. Before that change its row read 176–188 ns
on and 39–42 ns off on bare metal, and 250–291 ns and 62–75 ns in the guest.
The other samplers in the table still read the task group through
`bpf_probe_read_kernel()` calls.
`cpu_migrations`'s cgroup path runs only on a migration, so it costs nothing
per switch. On bare metal, `perf bench syscall basic` ran at 715 K ops/s with
the five samplers' cgroup paths off, the same as without these samplers
(711 K), and at 564–582 K with them on.

A task is attributed to its CPU controller's task group. A service whose
cgroup has no CPU controller of its own is counted under the nearest
ancestor that has one, usually its slice.

`task_attribution` is the per-task counterpart, for `cpu_usage` only, off by
default: see [cpu_usage](#cpu_usage).

## Table of Contents

- [Per-cgroup and per-task series](#per-cgroup-and-per-task-series)
- [Block I/O](#block-io)
  - [blockio_latency](#blockio_latency)
  - [blockio_requests](#blockio_requests)
- [CPU](#cpu)
  - [cpu_bandwidth](#cpu_bandwidth)
  - [cpu_cores](#cpu_cores)
  - [cpu_frequency](#cpu_frequency)
  - [cpu_l3](#cpu_l3)
  - [cpu_migrations](#cpu_migrations)
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
  - [scheduler_runqueue](#scheduler_runqueue)
- [Hardware Sensors](#hardware-sensors)
- [Syscall](#syscall)
  - [syscall_counts](#syscall_counts)
  - [syscall_latency](#syscall_latency)
- [TCP](#tcp)
  - [tcp_connect_latency](#tcp_connect_latency)
  - [tcp_packet_latency](#tcp_packet_latency)
  - [tcp_receive](#tcp_receive)
  - [tcp_retransmit](#tcp_retransmit)
  - [tcp_traffic](#tcp_traffic)
- [Rezolus](#rezolus)
  - [rezolus_rusage](#rezolus_rusage)

## Block I/O

Samplers for measuring how disk and storage devices are performing.

### blockio_latency

This sampler instruments the block I/O request queue to measure request latency
distribution.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `blockio_device_latency` | Distribution of block IO device service latency in nanoseconds, from the moment the device began servicing the request until it completed. Recordings made before this metric was renamed carry the same measurement as `blockio_latency` | `op={read,write,flush,discard}` |
| `blockio_queue_latency` | Distribution of time requests spent queued before the device began servicing them, in nanoseconds. This is the component that grows under saturation, where device latency alone stays flat. A request that goes straight to the driver has no queue phase and records no sample, so on such a device the histogram is present but empty | `op={read,write,flush,discard}` |
| `blockio_total_latency` | Distribution of end-to-end latency in nanoseconds, from the request entering the queue until it completed — queue and device together. Measured directly rather than summed, because two histograms cannot be added | `op={read,write,flush,discard}` |

### blockio_requests

This sampler instruments the block I/O request queue to get counts of requests,
number of bytes by request type, and size distribution. These metrics help
monitor I/O throughput and understand the characteristics of disk access
patterns. This information is useful for storage system tuning, application
optimization, and capacity planning.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `blockio_size` | Distribution of blockio operation sizes | `op={read,write,flush,discard}` |
| `blockio_operations` | The number of completed operations for block devices | `op={read,write,flush,discard}` |
| `blockio_bytes` | The number of bytes transferred for block devices | `op={read,write,flush,discard}` |

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

### cpu_l3

Tracks L3 cache misses and accesses. A high miss rate might mean inefficient
memory access patterns or programs competing for cache space.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_l3_access` | The number of L3 cache access | |
| `cpu_l3_miss` | The number of L3 cache miss | |

### cpu_migrations

Tracks when tasks move from one CPU to another. This is measured per-CPU with
conditionality to track system dynamics and per-cgroup to understand which
containers might be experiencing high rates of CPU migration.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `cpu_migration` | The number of CPU migrations | `direction={from,to}` |
| `cgroup_cpu_migration` | The number of CPU migrations on a per-cgroup basis | `name`: the name of the cgroup |

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
| `cpu_usage_exited_tasks` | CPU time of tasks that have exited, per CPU the exit ran on | `id` |
| `cgroup_cpu_usage_exited_tasks` | CPU time of a cgroup's tasks that have exited | `name` |
| `task_cpu_usage` | CPU time (user and system) per thread; only with `task_attribution = true` | `pid`, `tgid`, `comm`, `cgroup` |

The sampler accounts CPU per task in every configuration: the per-CPU and
per-cgroup deltas are computed from each thread's `utime`/`stime` whether
or not per-task series are exported, so turning the export off does not
change the host or cgroup totals. Exporting that per-task accounting as
`task_cpu_usage` is the option `task_attribution`, **off by default**. On, it
costs a task-metadata event per new thread (comm and three cgroup names),
an exit event per thread, a walk of the per-pid map's populated slots each
time a snapshot is served, and one series per thread in every recording,
which on a host with thread
churn is most of the recording. Set `task_attribution = true` in
`[samplers.cpu_usage]` (or `[defaults]`) to get it.

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

Because each read is a device command (measured ~7.6 ms/drive) and temperature
moves on the order of seconds, reads are throttled and offloaded from the
scrape/TTL sample cycle: at most once per `interval` the sampler dispatches the
reads (all drives in parallel) to a blocking thread pool and returns
immediately, so the sample cycle stays ~microseconds; the gauge retains its last
value between reads. The cadence defaults to 60s and is configurable via
`interval` in `[samplers.drivehealth]`.

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

jbd2 reports commit and checkpoint phases in **jiffies**, so those histograms
have one-jiffy resolution (1–10 ms depending on `CONFIG_HZ`); the sampler
measures the tick with `clock_getres(CLOCK_MONOTONIC_COARSE)` and converts to
nanoseconds. Lock-buffer stalls are reported by jbd2 in whole milliseconds.

Reading the jbd2 statistics needs their types in BTF: vmlinux when ext4 is
built in, module BTF (`/sys/kernel/btf/jbd2`, kernels 5.11+) when it is a
module. A 5.8–5.10 kernel with `CONFIG_EXT4_FS=m` has neither, and the sampler
reports failed there. `ext4_errors` needs the `ext4_error` tracepoint, which
is younger than the 5.8 floor; on a kernel without it the sampler reports
degraded and the rest of the metrics are unaffected.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `ext4_journal_commit_latency` | Distribution of each phase of a journal commit, in nanoseconds at one-jiffy resolution. `wait`: longest a handle waited to join the transaction; `request_delay`: commit requested to commit started; `running`: how long the transaction was open (bounded by the commit interval, cut short by fsync); `locked`: waiting for outstanding handles; `flushing`: data blocks to disk (ordered mode, where fsync waits on the device); `logging`: metadata and commit record to the journal | `phase={wait,request_delay,running,locked,flushing,logging}` |
| `ext4_journal_commits` | The number of journal transaction commits | |
| `ext4_journal_commit_handles` | Handles (metadata operations) committed, summed over commits | |
| `ext4_journal_commit_blocks` | Blocks per commit, summed: `dirtied` is metadata blocks in the transaction, `logged` is blocks written to the journal including descriptor and commit blocks | `kind={dirtied,logged}` |
| `ext4_journal_checkpoint_latency` | Distribution of the time each checkpoint took, in nanoseconds at one-jiffy resolution | |
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
`[defaults]` section never turns on (with `gpu_amd_pmu` and `hw_sensors`):
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
BTF lacks `ext4_file_write_iter` or `ext4_rename2`, that operation's
histogram stays empty and its counters read 0 while the rest run.

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
pseudo-filesystems (`tmpfs`, `overlay`, `proc`, `sysfs`, squashfs images, ...)
are never touched. Neither is a local filesystem that another mount covers — an
NFS share mounted over `/data`, or over a directory above it — since its path
now leads to the mount on top — nor one whose path passes through a network,
FUSE or autofs mount, since looking the path up walks that mount. An agent in a
chroot whose mount table omits the mount holding its root samples nothing, since
that mount's type is unknown. Each skipped filesystem is logged as a warning,
with the reason, when the reasons change. A `statvfs` on a `hard` network mount
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
(with `ext4_ops`, `gpu_amd_pmu` and `hw_sensors`): enable it with
`[samplers.xfs_log] enabled = true`.

Kernel support: task local storage became usable from tracing programs in
5.12, and attaching to XFS's tracepoints and functions needs XFS's BTF (vmlinux
if built in, module BTF from 5.11 if a module), so the floor is 5.12 with XFS
loaded. The fsync path's force is `xfs_log_force_seq` from 5.13 and
`xfs_log_force_lsn` before; the sampler loads whichever the kernel's BTF has,
and on a kernel with neither the fsync-path forces are not timed. On an older
kernel, or a host with no XFS, `rezolus status` shows the sampler unsupported
rather than failed. A kernel without `xfs_log_cil_wait` in BTF reads 0 for the
CIL count.

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
delete tracepoint; faults are `fentry` on `filemap_fault`.

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

Sizes of a fixed set of slab caches from `/proc/slabinfo`, read at most once
per `interval` (60 s by default) off the scrape cycle, since the file is
root-only, lists every cache on the machine and is generated under the slab
lock. The question it answers is whether the metadata caches fit: a
filesystem with tens of millions of files whose inode cache is evicted pays a
synchronous inode-table read on `stat` and on every atime update
(`ext4_inode_loads`), and `ext4_inode_cache` here is the cause on the same
axis. `vm.vfs_cache_pressure` trades this memory against the page cache.

Caches followed: `dentry`, `inode_cache`, `ext4_inode_cache`,
`ext4_extent_status`, `jbd2_journal_head`, `buffer_head`, `xfs_inode`,
`radix_tree_node` (the page-cache index). A cache the running kernel does not
have leaves its gauges absent, and so does one SLUB has merged into a
same-sized pool, which the file lists under one name only: caches without a
constructor are candidates, and `ext4_extent_status` is merged on the Debian 13
6.12 kernel. The inode and dentry caches have constructors and are never
merged. Booting with `slab_nomerge` lists every cache under its own name.

Measured on a 56-vCPU guest with a 196-line `/proc/slabinfo`: the sweep reads
the file in 366–409 µs and parses it in 51–63 µs, once per interval on the
blocking pool; the scrape-path `refresh()` is 0 µs except for the dispatch.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `memory_slab_cache_objects` | Objects in the cache: `active` are in use, `total` are allocated to the cache's slabs whether in use or free | `cache={...}`, `state={active,total}` |
| `memory_slab_cache_bytes` | Memory the cache holds: slabs times pages per slab times the page size | `cache={...}` |

### memory_vmstat

Memory NUMA metrics from /proc/vmstat. NUMA (Non-Uniform Memory Access) metrics
can be particularly relevant for multi-socket systems where memory access times
vary depending on which CPU is accessing which memory. These metrics may help
identify inefficient NUMA access patterns that can impact performance on NUMA
systems.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `memory_numa_hit` | The number of allocations that succeeded on the intended node | |
| `memory_numa_miss` | The number of allocations that did not succeed on the intended node | |
| `memory_numa_foreign` | The number of allocations that were not intended for a node that were serviced by this node | |
| `memory_numa_interleave` | The number of interleave policy allocations that succeeded on the intended node | |
| `memory_numa_local` | The number of allocations that succeeded on the local node | |
| `memory_numa_other` | The number of allocations that on this node that were allocated by a process on another node | |
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
without the throttle metrics and reports degraded rather than reading the
wrong argument.

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

## Scheduler

Metrics related to the Linux kernel scheduler. The scheduler is responsible for
allocating CPU time to processes, and its behavior directly impacts application
responsiveness, throughput, and overall system performance. These metrics
provide insights into how efficiently the scheduler is managing processes and
CPU resources.

### scheduler_runqueue

Instruments scheduler events and measures runqueue latency, process running
time, and context switch information. These metrics help understand how long
processes wait before getting CPU time, how long they run once scheduled, and
how frequently they're switched out. High runqueue latencies can indicate CPU
contention or scheduling inefficiencies that directly impact application
performance and responsiveness.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `scheduler_runqueue_latency` | Distribution of the amount of time tasks were waiting in the runqueue | |
| `scheduler_running` | Distribution of the amount of time tasks were on-CPU | |
| `scheduler_offcpu` | Distribution of the amount of time tasks were off-CPU | |
| `scheduler_context_switch` | The number of involuntary context switches | `kind=involuntary` |

## Hardware Sensors

The Linux `hw_sensors` sampler monitors hardware health: temperature, power,
voltage, current, fans, and cooling state. It reads thermal zones, hwmon
channels, and thermal cooling devices. It is **opt-in**, including when `[defaults] enabled = true`:

```toml
[samplers.hw_sensors]
enabled = true
interval = "5s"
```

Reads run in a nonoverlapping blocking task, dispatched by consumer activity
at most once per interval (5 seconds by default). Discovery runs every 60
seconds during sampling; descriptive metadata is cached between passes.
Failed, disabled, faulted, removed or unreadable measurements are absent from
the live endpoint rather than zero. If every channel in a family fails, its
last successful acquisition window remains unchanged; a recorder that
deduplicates windows can retain the previous observation without an outage
marker. A sysfs read can invoke hardware access, so this sampler's cost must
be measured on the target before fleet-wide enablement.

Each family identifies a native channel with `sensor`, `source`, `chip`,
`channel`, and a native `label` when supplied by the kernel. `board_model` and
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

### syscall_counts

Instruments syscall enter to gather syscall counts. This helps to identify
excessive system calls or unexpected patterns of system call usage.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `syscall` | The number of syscalls by operation type on a per-CPU basis | `op={other,read,write,poll,lock,time,sleep,socket,yield,filesystem,memory,process,query,ipc,timer,event,sync}`, `id`: the CPU the syscall entered on |
| `cgroup_syscall` | The number of syscalls by operation type on a per-cgroup basis | `op={other,read,write,poll,lock,time,sleep,socket,yield,filesystem,memory,process,query,ipc,timer,event,sync}`, `name`: the name of the cgroup | |

### syscall_latency

Instruments syscall enter and exit to gather syscall latency distributions.
These metrics track how long system calls take to complete, which can reveal
performance issues in the kernel or resource contention. High system call
latencies may indicate system-level bottlenecks.

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
