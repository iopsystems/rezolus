# Suggested Additions to Rezolus

Items raised during the Exceptions-dashboard work that weren't in scope for that PR but would close visibility gaps in adjacent areas.

## Samplers

### Hardirq accounting

Today rezolus tracks per-CPU `softirq` time and counts but not hardirq. `/proc/interrupts` exposes a per-CPU count for every IRQ vector (`LOC`, `RES`, `CAL`, `TLB`, `IWI`, `MCE`, plus per-device IRQs). A new procfs sampler that diffs these counts per scrape interval would directly show:

- IRQ leakage onto `isolcpus` / `nohz_full` cores (any non-zero rate is a misconfiguration).
- Cross-CPU IPI rate by class (`RES` for reschedule, `CAL` for `smp_call_function`, `TLB` for shootdowns) — these are the IPIs that pay double the VMEXIT tax on cloud VMs.
- LAPIC timer (`LOC`) rate — direct measure of whether `nohz_full` is actually quieting the tick.

No eBPF, no special privileges beyond reading procfs. Estimated cost: small. Same shape as the existing softirq accounting in `cpu/linux/usage`.

### `block:block_rq_error` tracepoint optimization

The new `blockio_errors` sampler attaches to `block_rq_complete` and early-returns on `BLK_STS_OK`. On kernel ≥5.18 the dedicated `block:block_rq_error` tracepoint exists and fires *only* on error, eliminating the OK-path branch entirely. Worth feature-detecting at load time and preferring the dedicated tracepoint when available; fall back to the filtered `block_rq_complete` on older kernels.

### Per-CPU breakdown of `blockio_operations`

Currently exposed system-wide. Per-CPU would let the dashboard show "CPU 12 retired 80% of completions" at a glance — the same pattern that makes per-CPU `softirq{kind="net_rx"}` heatmaps so useful for diagnosing RPS misconfig. Pairs naturally with the hardirq sampler above.

### Submitter-vs-completer CPU mismatch counter

Capture CPU at `block_rq_issue` (in a BPF map keyed by request pointer), compare at `block_rq_complete`, increment a per-(submit_cpu, complete_cpu) bucket. Directly answers "is `rq_affinity=2` actually working?" — the metric should pile up on the diagonal if the block layer is routing completions back to the submitter.

Heavier than the other ideas (full BPF program with a hashmap), but the only way to verify completion-side affinity from inside the kernel.

### Protocol-level error tracepoints

The current `blockio_errors` bucketing collapses `blk_status_t` into seven coarse classes. For deeper diagnosis:

- **`nvme:nvme_complete_rq`** — exposes the NVMe status code (Status Code Type + Status Code), distinguishing *Media Error* / *Capacity Exceeded* / *Aborted by Host* / *Internal Error* / etc.
- **`scsi:scsi_dispatch_cmd_error`** + sense data — distinguishes UNIT ATTENTION / NOT READY / ABORTED COMMAND / ILLEGAL REQUEST.

Probably one new sampler each. Useful for storage-heavy fleets where the generic class breakdown isn't enough.

### Per-cgroup off-CPU latency distribution

`cgroup_scheduler_offcpu` is a counter (total ns blocked) — sufficient for "fraction of wall time blocked" but doesn't distinguish a cgroup taking many short blocks vs a few long ones. The system-wide `scheduler_offcpu` is a histogram that does. A per-cgroup histogram is memory-intensive (`MAX_CGROUPS × buckets`) but valuable for tail-latency triage of co-located workloads.

## Topology / configuration gauges

These don't require eBPF — they're cheap procfs/sysfs reads that surface configuration in the metrics so the dashboard can highlight misconfigurations without operators having to SSH and `cat`.

| Gauge | Source | Purpose |
|---|---|---|
| `topology_isolcpus` | `/sys/devices/system/cpu/isolated` | Bitmap of CPUs marked `isolcpus=` at boot. |
| `topology_nohz_full` | `/sys/devices/system/cpu/nohz_full` | Bitmap of CPUs in adaptive-tickless mode. |
| `topology_cpuset_*` | per-cgroup cpuset.cpus | Which CPUs each cgroup is permitted to use. |
| `blockio_rq_affinity` | `/sys/block/<dev>/queue/rq_affinity` | Per-device completion routing setting (0/1/2). |
| `blockio_nvme_poll_queues` | `/sys/class/nvme/nvme*/poll_queues` | NVMe poll-mode queue count per controller. |
| `blockio_scheduler` | `/sys/block/<dev>/queue/scheduler` | Active IO scheduler per device. |

The dashboard can then flag conditions like "IRQ landed on a CPU listed in `isolcpus`" or "`rq_affinity=0` on a NUMA host."

## Viewer

### Heatmap A/B view "Full → diff" reproduction

Reported behavior: clicking "Full" then "diff" on certain metrics returns a percentile scatter instead of a diff heatmap; only some metrics. Tracing through the code:

- The "diff" toggle is gated to `style === 'heatmap'` in `chart_controls.js:18` (gauge/counter with `id` label) — histograms shouldn't see it at all.
- `extractExperimentCapture` for `histogram_heatmap` style explicitly returns empty arrays with a TODO at `viewer_core.js:122-128` ("Full support is follow-up work").
- The section-level "SHOW HEATMAPS" toggle is hidden in compare mode (`app.js:590`) precisely because of the gap above.

Two threads to follow up on:

1. Reproduce against a specific metric (operator action), then trace the actual code path. The gap I found doesn't predict "scatter plot of percentiles" as the fallback — predicts an empty chart.
2. Either complete the histogram_heatmap experiment-side data path (close the TODO at `viewer_core.js:122-128`) or document the limitation in the dashboard so the section-level heatmap toggle could be re-enabled for compare mode.
