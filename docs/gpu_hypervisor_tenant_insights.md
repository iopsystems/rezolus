# Rezolus on GPU Servers: What an Operator and Tenants Can Learn From Each Other

This note sketches the two-way value of running **Rezolus inside the guest OS**
on a GPU server fleet. The operator owns the hypervisor and the physical hosts;
tenants run their workloads (training, fine-tuning, inference, data prep) inside
guest VMs. Rezolus is a low-overhead, high-resolution telemetry agent that uses
eBPF for in-kernel aggregation, so it is cheap enough to leave running on
production guests fleet-wide.

The key idea: the operator already sees the *host* side. Putting Rezolus on the
guest closes the loop by exposing the *workload* side at high resolution — and
that benefits both parties.

---

## What the operator can offer tenants as insight

These are things a tenant usually struggles to see on their own, but which
become straightforward once Rezolus is collecting guest-side telemetry.

### GPU efficiency and utilization quality
- **Real GPU utilization vs. "allocation."** `gpu_utilization` and
  `gpu_memory_utilization` show how much of a paid-for GPU is actually doing
  work. A GPU pinned at 100% allocation but 30% utilization is money on the
  floor — the operator can show tenants exactly when and where.
- **Power and thermal headroom.** `gpu_power_usage`, `gpu_energy_consumption`,
  `gpu_temperature`, and `gpu_clock` reveal whether kernels are power- or
  thermally throttled, or running below their clock ceiling.
- **PCIe pressure.** `gpu_pcie_throughput` / `gpu_pcie_bandwidth` highlight
  host-to-device transfer bottlenecks — a classic cause of "the GPU is idle but
  my job is slow."

### Where the GPU is *waiting* (the host-side context the tenant can't see)
- **Data-loading and I/O stalls.** `blockio_latency`, `blockio_requests`, and
  `blockio_size` expose storage as the reason GPUs sit idle between steps.
- **CPU starvation feeding the GPU.** `cpu_usage`, `scheduler_runqueue` (runqueue
  latency), and `cpu_migrations` show when the data pipeline (augmentation,
  tokenization, collation) is CPU-bound and can't keep the accelerator fed.
- **Network bottlenecks for distributed jobs.** `tcp_traffic`,
  `tcp_retransmit`, `tcp_connect_latency`, and `tcp_packet_latency` surface
  collective-communication (all-reduce) stalls, NIC saturation, and retransmits
  that throttle multi-node training.

### Efficiency and right-sizing guidance
- **"You are paying for a GPU box but bottlenecked on CPU/IO/network."** The
  combination above lets the operator give concrete, evidence-backed advice:
  add data-loader workers, move to faster storage, change instance shape, batch
  differently.
- **Syscall and scheduler behavior.** `syscall_counts` / `syscall_latency` and
  runqueue depth can point at pathological behavior (excessive `futex`, lock
  contention, oversubscribed vCPUs) the tenant would never attribute correctly.

### Incident and regression support
- **High-resolution post-incident analysis.** With Rezolus Hindsight's rolling
  ring buffer, the operator can hand a tenant a second-by-second picture of what
  the guest was doing during a slowdown, OOM, or throttling event.
- **Before/after comparisons.** The viewer's A/B mode lets a tenant compare two
  runs (e.g., before vs. after a code change or instance migration) on identical
  axes.

---

## What the operator can learn from tenants

Guest-side telemetry also makes the *fleet* more legible to the operator — for
capacity planning, oversubscription decisions, and fairness.

### True demand vs. provisioned capacity
- **Real utilization distributions across the fleet.** Aggregated
  `gpu_utilization` / `gpu_memory_utilization` tell the operator how much
  GPU capacity is genuinely consumed vs. reserved — the basis for
  oversubscription and bin-packing policy.
- **Workload shapes.** Block IO sizes, network traffic patterns, and CPU/GPU
  ratios let the operator classify tenants (IO-bound prep, compute-bound
  training, latency-bound inference) and place them on suitable hardware.

### Noisy-neighbor and contention detection
- **Cross-tenant interference.** Correlating guest-side `scheduler_runqueue`
  latency, `cpu_migrations`, `blockio_latency`, and `tcp_retransmit` with host
  occupancy reveals when one tenant's behavior degrades a co-located tenant —
  something invisible from host counters alone.
- **vCPU oversubscription health.** Runqueue latency on the guest is a direct
  signal that the host is overcommitted relative to what tenants actually need.

### Capacity planning and hardware ROI
- **Bottleneck inventory across the fleet.** If many guests are PCIe- or
  storage-bound rather than compute-bound, that argues for faster NVMe, more
  host CPU, or better NIC provisioning — not more GPUs.
- **Power and thermal envelopes.** Fleet-wide `gpu_power_usage` and
  `gpu_temperature` inform datacenter power budgeting and cooling, and flag
  hosts that throttle under load.

### Anomaly detection at fleet scale
- Rezolus recordings (parquet) plus the MCP analysis tools
  (`detect-anomalies`, `analyze-correlation`, PromQL `query`) let the operator
  run automated regression and anomaly sweeps across many tenants without
  manual dashboard-watching.

---

## Why guest-side Rezolus specifically

- **Low overhead, always-on.** eBPF in-kernel aggregation read via mmap means it
  is cheap enough to run continuously on production guests, not just during
  debugging.
- **High resolution.** Sub-second visibility catches micro-stalls (step-to-step
  GPU idle, all-reduce hiccups) that minute-resolution monitoring averages away.
- **Workload-side truth.** The host sees physical counters; the guest sees what
  the *application kernel* actually experiences (runqueue latency, syscall
  latency, TCP latency). Together they tell a complete story neither side has
  alone.

---

## A reasonable privacy/trust boundary

Rezolus collects *systems performance* signals (utilization, latencies,
counters), not application payloads or model data. That makes it a comfortable
shared layer: tenants get expert efficiency insight and faster incident
resolution, and the operator gets the demand signal needed to run a denser,
fairer, better-provisioned fleet — without either side exposing proprietary
workload contents.
