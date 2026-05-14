# Diagnostic Patterns

Rules of thumb for using rezolus metrics to identify recurring failure modes. Each pattern lists the metric shape that distinguishes the cause, plus the operational implication.

## Block IO error mix

The composition of `blockio_errors` is more diagnostic than absolute counts. The error label buckets `blk_status_t` into seven coarse classes; the *shape* of the mix points at the cause.

| Pattern | Likely cause | Where to look next |
|---|---|---|
| `error="timeout"` rising, `error="io"` flat | Transport or controller blackhole — NVMe controller hung, NVMe-oF transport silent, iSCSI target unreachable, hypervisor not reattaching an EBS volume | `dmesg`, `/proc/interrupts` for IRQ delivery, `nvme list` |
| `error="io"` and `error="target"` together | Real device failure — failing media, namespace going read-only, SCSI EH exhausted retries | SMART data, `dmesg`, kernel log for `blk_update_request` errors |
| `error="nospc"` non-zero | Thin-provisioned storage out of physical capacity (dm-thin, VMDK datastore, RBD pool past `full_ratio`) — *not* a filesystem-full condition | The abstraction layer above the disk; FS metrics will show plenty of room |
| `error="protection"` non-zero (any rate) | T10 PI / DIF/DIX or NVMe end-to-end protection check failed. Treat as a data-integrity alarm even at very low rates | Hardware logs, ECC counters, controller PI configuration |
| `error="unsupported"` increasing | Configuration drift — feature toggled off, firmware update dropped a capability, kernel upgrade changed the discard/write-zeroes path | Recent infra changes, `nvme id-ctrl`, queue feature attributes |
| `blockio_requeues` rising, `blockio_errors` flat | Multipath or transport recovery absorbing faults cleanly — system is healthy, path is flaky | Multipath status, NVMe-oF connection counts, `dmesg` for path events |
| `blockio_requeues` and `blockio_errors` both rising | Recovery exhausted — paths failed for real | Same as above, plus prepare for failover |

PromQL pattern:

```
sum by (error) (irate(blockio_errors[5m]))      # mix shape
sum (irate(blockio_requeues[5m]))               # recovery rate
```

## IRQ pinning misconfiguration

Block IO completions and network packet processing run in softirq context on whichever CPU received the hardware IRQ. Lopsided per-CPU softirq distribution is the canonical signal that interrupt affinity isn't doing what the topology wants.

| Pattern | Likely cause |
|---|---|
| `softirq{kind="block"}` concentrated on one or two CPUs | Single-queue HBA driver, `smp_affinity` mask too narrow, or `irqbalance` failed/disabled. blk-mq devices should spread across many vCPUs naturally |
| `softirq{kind="net_rx"}` concentrated on one CPU | RPS/RFS misconfigured, RSS not enabled on the NIC, or single-queue NIC driver |
| Non-zero `softirq` on a CPU listed in `isolcpus` | IRQ leakage onto an isolated CPU — check `/proc/irq/*/smp_affinity_list` |
| `cpu_tlb_flush{reason="remote_send_ipi"}` non-zero on an isolated CPU | Cross-CPU `mm_struct` sharing — another CPU's `munmap`/`mprotect` is shooting down TLB entries on the isolated CPU |

PromQL pattern:

```
# Per-CPU softirq distribution — heatmap shape, look for hotspots
sum by (id) (irate(cpu_usage{state="softirq"}[5m]))

# Block-specific
sum by (id) (irate(softirq{kind="block"}[5m]))

# IPI leakage onto isolated CPUs
sum by (id) (irate(cpu_tlb_flush{reason="remote_send_ipi"}[5m]))
```

## Cloud VM contention vs self-induced overload

Two metrics decompose "my workload is queueing":

- `cpu_usage{state="steal"}` — vCPU was descheduled by the hypervisor (host-side cause).
- `scheduler_runqueue_wait` — your task wanted to run; something on your runqueue ran instead.

| Pattern | Interpretation |
|---|---|
| High runqueue wait, near-zero steal | Self-induced — too many runnable threads for the vCPUs you have. Scale up or fix concurrency |
| High runqueue wait, high steal | Host oversubscription — noisy neighbor or the host is saturated. Move to a dedicated tier or larger SKU |
| Low runqueue wait, high steal | Hypervisor preempting you for housekeeping (live migration window, host scheduler). Often transient |
| Steal spikes correlate with IO latency spikes | vCPU was preempted during interrupt-heavy moments. Common on shared-tenancy hosts under load — cloud IRQ tax |

`cpu_l3_miss` per CPU adds one more axis: uniformly elevated across all your vCPUs (rather than concentrated on whichever ones your workload runs on) suggests external cache pressure from co-tenants — visible *without* steal time rising. Cache contention is the noisy-neighbor failure mode that doesn't show up in scheduling.

## CPU throttling vs scheduling pressure

`cgroup_cpu_throttled` and `cgroup_scheduler_runqueue_wait` answer different questions for a single cgroup:

| Pattern | Interpretation |
|---|---|
| `cgroup_cpu_throttled` non-zero, `cgroup_cpu_bandwidth_throttled_time` rising | CFS quota is the bottleneck — workload is ready to run but blocked by `cpu.max`. Raise the quota or remove it |
| `cgroup_scheduler_runqueue_wait` rising, throttling near zero | Cgroup has quota to spare, but is competing with co-located cgroups for actual CPU. Check sibling cgroups' usage |
| Both rising together | Both: quota is too tight *and* co-located workloads are competing |

These are often confused. Throttling is *policy* — quota enforcement. Runqueue wait is *contention* — multiple runnable threads sharing a CPU.

## ENA allowance limits (AWS)

`network_ena_*_allowance_exceeded` counters increment when the Nitro hypervisor drops or queues packets to keep the instance under its per-instance allowance. Sustained non-zero rates mean the instance is undersized for its traffic shape — *not* that the network is unhealthy.

| Counter | Limit hit |
|---|---|
| `network_ena_bandwidth_allowance_exceeded{direction="receive"}` | Inbound bandwidth cap |
| `network_ena_bandwidth_allowance_exceeded{direction="transmit"}` | Outbound bandwidth cap |
| `network_ena_pps_allowance_exceeded` | Packets-per-second cap (independent of bandwidth) |
| `network_ena_conntrack_allowance_exceeded` | Connection-tracking table full |
| `network_ena_linklocal_allowance_exceeded` | Per-second cap on metadata-service / IMDS access |

Bursty bandwidth usage typically rides on burst credits without surfacing here; sustained overage is what shows up. The `linklocal` one is especially common — applications that hammer the IMDS for credentials in tight loops will trip it.

## TCP retransmits as a path-quality signal

`tcp_retransmit` counts segments retransmitted because the peer didn't ack in time. The *shape* against load matters:

- **Constant low rate, scales with traffic** — normal background loss. Don't chase.
- **Spike independent of traffic load** — path event (route change, neighbor outage, transient saturation upstream).
- **Sustained elevation under load** — congestion at a fixed bottleneck or a specific peer. Pair with ENA allowance counters; a same-time spike in `bw_in_allowance_exceeded` rules out the network and points at instance sizing.

## Live-migration window detection (cloud)

A burst of `cpu_usage{state="steal"}` lasting tens of seconds, with `scheduler_offcpu` rising for *all* tasks at once and IO latency spiking briefly, usually corresponds to a hypervisor-driven live migration. Distinct from sustained noisy-neighbor steal because it ends abruptly. Worth correlating with cloud-provider maintenance windows / instance lifecycle events.

## Off-CPU time as a workload health signal

Per-cgroup `cgroup_scheduler_offcpu` counts nanoseconds spent off-CPU. Convert to a wall-time fraction:

```
sum by (id) (irate(cgroup_scheduler_offcpu[5m])) / 1e9
```

Yields 0..N where N is the number of CPUs the cgroup had threads on — total wall-time-equivalent seconds of blocking per second.

| Pattern | Interpretation |
|---|---|
| High off-CPU, low CPU usage | Workload is IO- or lock-bound. Look at `blockio_latency`, `tcp_packet_latency`, syscall latency by class |
| High off-CPU, paired with `cgroup_cpu_throttled` rising | Quota throttling is the off-CPU cause |
| High off-CPU, paired with high steal | Host preemption is part of the off-CPU cause |
| High off-CPU, none of the above | Self-induced blocking — application logic, lock contention. Profiling territory |
