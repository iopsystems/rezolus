# Rezolus Metrics Documentation

Rezolus is a Linux performance telemetry agent that provides detailed insights
into system behavior through efficient, low-overhead instrumentation.

This guide walks you through all the available metrics, organized by category.

## Table of Contents

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
- [Filesystem](#filesystem)
  - [filesystem](#filesystem-1)
- [GPU](#gpu)
  - [gpu_nvidia](#gpu_nvidia)
  - [gpu_intel_pmu](#gpu_intel_pmu)
- [Memory](#memory)
  - [memory_meminfo](#memory_meminfo)
  - [memory_vmstat](#memory_vmstat)
- [Network](#network)
  - [network_interfaces](#network_interfaces)
  - [network_traffic](#network_traffic)
- [Scheduler](#scheduler)
  - [scheduler_runqueue](#scheduler_runqueue)
- [Sensors](#sensors)
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
| `cgroup_cpu_instructions` | The number of elapsed CPU cycles on a per-cgroup basis | name: the name of the cgroup |

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

## Sensors

The Linux `sensors` sampler reads thermal zones, hwmon channels and thermal
cooling devices. It is **opt-in**, including when `[defaults] enabled = true`:

```toml
[samplers.sensors]
enabled = true
interval = "5s"
```

Reads run in a nonoverlapping blocking task, dispatched by consumer activity
at most once per interval (5 seconds by default). Discovery runs every 60
seconds during sampling; descriptive metadata is cached between passes.
Failed, disabled, faulted, removed or unreadable measurements are absent rather
than zero. A sysfs read can invoke hardware access, so this sampler's cost must
be measured on the target before fleet-wide enablement.

Each family identifies a native channel with `sensor`, `source`, `chip`,
`channel`, and a native `label` when supplied by the kernel. Platform identity
and verified board-specific scope provide additional context when available.
Unknown boards retain native readings without inferred CPU/GPU associations.
Sensor slots are never reused for a different identity during the process;
each family supports at most 256 lifetime identities, reporting overflow.

| Metric | Type | Stored unit / interpretation |
| --- | --- | --- |
| `sensor_temperature` | Gauge | Millidegrees Celsius per temperature channel; signed |
| `sensor_power` | Gauge | Microwatts per rail; direct hwmon reading or checked INA3221 voltage × current |
| `sensor_voltage` | Gauge | Millivolts per bus-voltage channel; excludes INA3221 shunt-voltage channels |
| `sensor_current` | Gauge | Milliamps per current channel |
| `sensor_fan_speed` | Gauge | Measured revolutions per minute, including NVIDIA `pwm_tach/rpm` |
| `sensor_fan_pwm` | Gauge | Fan command on a 0–255 scale, not measured rotation |
| `sensor_cooling_state` | Gauge | Driver-defined cooling-state index, not a throttling percentage |

The Sensors dashboard plots every family separately by sensor, converting
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
| `syscall` | The number of syscalls by operation type | `op={other,read,write,poll,lock,time,sleep,socket,yield,filesystem,memory,process,query,ipc,timer,event}` |
| `cgroup_syscall` | The number of syscalls by operation type on a per-cgroup basis | `op={other,read,write,poll,lock,time,sleep,socket,yield,filesystem,memory,process,query,ipc,timer,event}`, `name`: the name of the cgroup | |

### syscall_latency

Instruments syscall enter and exit to gather syscall latency distributions.
These metrics track how long system calls take to complete, which can reveal
performance issues in the kernel or resource contention. High system call
latencies may indicate system-level bottlenecks.

| Metric | Description | Metadata |
|--------|-------------|----------|
| `syscall_latency` | Distribution of syscall latency | `op={other,read,write,poll,lock,time,sleep,socket,yield}` |

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
