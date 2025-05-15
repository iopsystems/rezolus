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
  - [cpu_tlb_flush](#cpu_tlb_flush)
  - [cpu_usage](#cpu_usage)
- [GPU](#gpu)
  - [gpu_nvidia](#gpu_nvidia)
- [Memory](#memory)
  - [memory_meminfo](#memory_meminfo)
  - [memory_vmstat](#memory_vmstat)
- [Network](#network)
  - [network_interfaces](#network_interfaces)
  - [network_traffic](#network_traffic)
- [Scheduler](#scheduler)
  - [scheduler_runqueue](#scheduler_runqueue)
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
| `blockio_latency` | Distribution of blockio operation latency in nanoseconds | `op={read,write,flush,discard}` |

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
| `network_bytes` | The number of bytes transferred over the network | `direction={receive,transmit}` |
| `network_packets` | The number of packets transferred over the network | `direction={receive,transmit}` |

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
