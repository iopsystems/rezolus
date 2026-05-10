# Desired Future Capabilities

Feature requests raised during the Exceptions-dashboard work. Each entry describes *what* would be instrumented and *why* it matters operationally — implementation choices are intentionally out of scope and will be decided per item.

## Hardirq instrumentation

Per-CPU rate of hardware interrupt delivery, broken down by source (per-device IRQs, inter-processor interrupts, LAPIC timer, etc.). Today rezolus tracks softirq cost per CPU but not hardirq.

**Why it matters.** On hosts using CPU isolation for QoS, any non-zero hardirq rate on isolated CPUs is a misconfiguration. On cloud VMs, IPI traffic pays a multiplied VMEXIT cost — surfacing the rate makes the cost visible. The LAPIC timer rate also tells you whether `nohz_full` is actually quieting the tick.

## Per-CPU block IO completion distribution

Today `blockio_operations` aggregates across CPUs. A per-CPU breakdown would show how completions are distributed across the cores they're delivered to.

**Why it matters.** Lopsided distribution (one CPU draining most completions) signals IRQ-affinity misconfiguration on multi-queue devices, the same way per-CPU `softirq{kind="net_rx"}` reveals RPS/RFS misconfiguration on the network side. Without the per-CPU view, a block-IO bottleneck pinned to a single CPU is invisible until tail latency spikes.

## IO submitter→completer CPU correlation

The block layer's `rq_affinity` setting controls whether completions are routed back to the CPU that submitted the request. There's no metric today that directly verifies it's working.

**Why it matters.** Cross-CPU completion routing causes cache and NUMA traffic on every IO. A direct measure of "what fraction of IOs complete on a different CPU than they were submitted from" would let operators tune `rq_affinity` and IRQ topology with feedback rather than guesswork.

## Protocol-level IO error breakdown

`blockio_errors` buckets `blk_status_t` into seven coarse classes. Going one level deeper into protocol-specific status codes (NVMe SCT/SC, SCSI sense keys) would distinguish, for example, *Media Error* from *Aborted by Host* from *Capacity Exceeded*.

**Why it matters.** The coarse classes answer "is the storage misbehaving"; the protocol codes answer "in what specific way." Useful for fleets that need to triage storage incidents without manual `dmesg` archaeology.

## Per-cgroup off-CPU latency distribution

The system-wide `scheduler_offcpu` is a histogram; the per-cgroup `cgroup_scheduler_offcpu` is a counter (total ns blocked). A per-cgroup distribution would distinguish a cgroup taking many short blocks from one taking a few long ones.

**Why it matters.** Two co-located cgroups with the same total off-CPU time can have very different application-visible latencies. The shape of the distribution is the diagnostic — long-tailed off-CPU time often indicates lock contention or IO stalls; short and many indicates scheduler interleaving.

## System configuration visibility

Surface boot-time and runtime configuration that affects performance posture: CPU isolation (`isolcpus`, `nohz_full`, cgroup `cpuset`), block device tuning (IO scheduler, completion affinity, NVMe queue mode), and IRQ affinity.

**Why it matters.** A host can be misconfigured against its intended QoS posture in ways that are silent until traffic hits a corner case. Pulling configuration into the metrics stream lets dashboards flag drift (e.g., "completion just landed on a CPU listed in `isolcpus`") and lets fleets compare intent against reality at scale.
