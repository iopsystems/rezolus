use crate::MetricsSource;
use crate::plot::*;

/// The metrics we probe for GPU presence / id enumeration.
///
/// `gpu_engine_busy_time` and `gpu_frequency_sample` are Intel's — an Intel host
/// publishes neither `gpu_utilization` nor (on an integrated GPU) `gpu_memory`,
/// so without them such a host would enumerate no GPUs at all and the per-device
/// charts and selector would never appear.
const GPU_PROBE_METRICS: &[&str] = &[
    "gpu_utilization",
    "gpu_memory",
    "gpu_temperature",
    "gpu_power_usage",
    "gpu_clock",
    "gpu_memory_utilization",
    "gpmu_clock",
    "gpu_engine_busy_time",
    "gpu_frequency_sample",
];

/// True iff the recording has more than one GPU. Per-device charts are
/// suppressed when this is false because they degenerate to the aggregate.
///
/// Counts distinct `(vendor, id)` pairs, not distinct ids: a host with an
/// NVIDIA card and an Intel iGPU has two GPUs that are both `id="0"`, and
/// counting ids alone would call that a single-GPU host and hide the
/// per-device charts that separate them.
fn has_multiple_gpus(data: &dyn MetricsSource) -> bool {
    gpu_entries(data).len() > 1
}

/// One GPU present in the recording, as the selector needs to address it.
///
/// `id` alone does not identify a GPU. Every vendor's sampler numbers its own
/// devices from 0 (NVML's device index, ROCm's, and for Intel the PMU discovery
/// order), so on a host with an NVIDIA card and an Intel iGPU there are two
/// distinct GPUs both labelled `id="0"`. Selecting on `id` alone matches both.
struct GpuEntry {
    id: i64,
    /// The `vendor` label, absent on samplers that do not set one (the Apple
    /// GPU sampler declares no vendor). `None` means "match on id alone",
    /// which is the pre-existing behaviour and correct when nothing else
    /// disambiguates.
    vendor: Option<String>,
}

/// The distinct GPUs present in the recording, sorted by vendor then id so the
/// selector's order is stable across runs.
fn gpu_entries(data: &dyn MetricsSource) -> Vec<GpuEntry> {
    let mut seen: std::collections::BTreeSet<(Option<String>, i64)> =
        std::collections::BTreeSet::new();

    for metric in GPU_PROBE_METRICS {
        // Read whole label sets rather than one key at a time: pairing a
        // vendor with an id requires seeing them on the same series.
        for labels in data
            .counter_labels(metric)
            .into_iter()
            .chain(data.gauge_labels(metric))
        {
            let Some(id) = labels.get("id").and_then(|v| v.parse::<i64>().ok()) else {
                continue;
            };
            seen.insert((labels.get("vendor").cloned(), id));
        }
    }

    seen.into_iter()
        .map(|(vendor, id)| GpuEntry { id, vendor })
        .collect()
}

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let multi_gpu = has_multiple_gpus(data);

    // Tell the frontend which GPUs exist so it can render the GPU selector
    // (a picker that filters the non-per-GPU charts to a subset, or the
    // aggregate). Only meaningful with more than one GPU.
    //
    // `gpus` carries the (vendor, id) pairs the selector must filter on; `ids`
    // is kept alongside it for older frontends, which read only that field.
    let entries = gpu_entries(data);
    if entries.len() > 1 {
        let mut ids: Vec<i64> = entries.iter().map(|g| g.id).collect();
        ids.sort_unstable();
        ids.dedup();

        let gpus: Vec<serde_json::Value> = entries
            .iter()
            .map(|g| match &g.vendor {
                Some(v) => serde_json::json!({ "id": g.id, "vendor": v }),
                None => serde_json::json!({ "id": g.id }),
            })
            .collect();

        view.metadata.insert(
            "gpu_selector".to_string(),
            serde_json::json!({ "enabled": true, "ids": ids, "gpus": gpus }),
        );
    }

    let mut utilization = Group::new("Utilization", "utilization");

    let gpu = utilization.subgroup("GPU Utilization");
    gpu.describe("Fraction of time the GPU has work scheduled, averaged and per-device.");
    if multi_gpu {
        gpu.plot_promql(
            PlotOpts::gauge("GPU %", "gpu-pct", Unit::Percentage).percentage_range(),
            "avg(gpu_utilization) / 100".to_string(),
        );
        gpu.plot_promql(
            PlotOpts::gauge("GPU % (Per-GPU)", "gpu-pct-per-gpu", Unit::Percentage)
                .percentage_range()
                .with_row_label("GPU"),
            "sum by (id) (gpu_utilization) / 100".to_string(),
        );
    } else {
        gpu.plot_promql_full(
            PlotOpts::gauge("GPU %", "gpu-pct", Unit::Percentage).percentage_range(),
            "avg(gpu_utilization) / 100".to_string(),
        );
    }

    let mem_ctrl = utilization.subgroup("Memory Controller");
    mem_ctrl.describe("Fraction of time the memory controller is servicing requests.");
    if multi_gpu {
        mem_ctrl.plot_promql(
            PlotOpts::gauge("Memory Controller %", "mem-ctrl-pct", Unit::Percentage)
                .percentage_range(),
            "avg(gpu_memory_utilization) / 100".to_string(),
        );
        mem_ctrl.plot_promql(
            PlotOpts::gauge(
                "Memory Controller % (Per-GPU)",
                "mem-ctrl-pct-per-gpu",
                Unit::Percentage,
            )
            .percentage_range()
            .with_row_label("GPU"),
            "sum by (id) (gpu_memory_utilization) / 100".to_string(),
        );
    } else {
        mem_ctrl.plot_promql_full(
            PlotOpts::gauge("Memory Controller %", "mem-ctrl-pct", Unit::Percentage)
                .percentage_range(),
            "avg(gpu_memory_utilization) / 100".to_string(),
        );
    }

    view.group(utilization);

    let mut activity = Group::new("Parallel Compute Activity", "activity");

    let tensor = activity.subgroup("Tensor Activity");
    tensor.describe("Tensor core utilization — how busy the matrix-math units are.");
    if multi_gpu {
        tensor.plot_promql(
            PlotOpts::gauge("GPU Tensor Activity %", "gpu-tensor-act", Unit::Percentage)
                .percentage_range(),
            "avg(gpu_tensor_utilization) / 100".to_string(),
        );
        tensor.plot_promql(
            PlotOpts::gauge(
                "GPU Tensor Activity % (Per-GPU)",
                "gpu-tensor-act-per-gpu",
                Unit::Percentage,
            )
            .percentage_range()
            .with_row_label("GPU"),
            "sum by (id) (gpu_tensor_utilization) / 100".to_string(),
        );
    } else {
        tensor.plot_promql_full(
            PlotOpts::gauge("GPU Tensor Activity %", "gpu-tensor-act", Unit::Percentage)
                .percentage_range(),
            "avg(gpu_tensor_utilization) / 100".to_string(),
        );
    }

    let sm = activity.subgroup("SM Activity & Occupancy");
    sm.describe("Streaming multiprocessor active time and warp occupancy — core indicators of compute efficiency.");
    if multi_gpu {
        sm.plot_promql(
            PlotOpts::gauge("GPU SM Activity %", "gpu-sm-act", Unit::Percentage).percentage_range(),
            "avg(gpu_sm_utilization) / 100".to_string(),
        );
        sm.plot_promql(
            PlotOpts::gauge(
                "GPU SM Activity % (Per-GPU)",
                "gpu-sm-act-per-gpu",
                Unit::Percentage,
            )
            .percentage_range()
            .with_row_label("GPU"),
            "sum by (id) (gpu_sm_utilization) / 100".to_string(),
        );
        sm.plot_promql(
            PlotOpts::gauge("GPU SM Occupancy %", "gpu-sm-ocp", Unit::Percentage)
                .percentage_range(),
            "avg(gpu_sm_occupancy) / 100".to_string(),
        );
        sm.plot_promql(
            PlotOpts::gauge(
                "GPU SM Occupancy % (Per-GPU)",
                "gpu-sm-ocp-per-gpu",
                Unit::Percentage,
            )
            .percentage_range()
            .with_row_label("GPU"),
            "sum by (id) (gpu_sm_occupancy) / 100".to_string(),
        );
    } else {
        sm.plot_promql_full(
            PlotOpts::gauge("GPU SM Activity %", "gpu-sm-act", Unit::Percentage).percentage_range(),
            "avg(gpu_sm_utilization) / 100".to_string(),
        );
        sm.plot_promql_full(
            PlotOpts::gauge("GPU SM Occupancy %", "gpu-sm-ocp", Unit::Percentage)
                .percentage_range(),
            "avg(gpu_sm_occupancy) / 100".to_string(),
        );
    }

    view.group(activity);

    let mut memory = Group::new("Memory", "memory");

    let capacity = memory.subgroup("Capacity");
    capacity
        .describe("Total GPU memory used and free across all devices, plus overall utilization.");
    capacity.plot_promql(
        PlotOpts::gauge("Used", "mem-used", Unit::Bytes),
        "sum(gpu_memory{state=\"used\"})".to_string(),
    );
    capacity.plot_promql(
        PlotOpts::gauge("Free", "mem-free", Unit::Bytes),
        "sum(gpu_memory{state=\"free\"})".to_string(),
    );
    capacity.plot_promql_full(
        PlotOpts::gauge("Memory Utilization %", "mem-util-pct", Unit::Percentage).percentage_range(),
        "sum(gpu_memory{state=\"used\"}) / (sum(gpu_memory{state=\"used\"}) + sum(gpu_memory{state=\"free\"}))".to_string(),
    );

    if multi_gpu {
        let per_device = memory.subgroup("Per-Device Capacity");
        per_device.describe("Memory used and free broken out by GPU id.");
        per_device.plot_promql(
            PlotOpts::gauge("Used (Per-GPU)", "mem-used-per-gpu", Unit::Bytes)
                .with_row_label("GPU"),
            "sum by (id) (gpu_memory{state=\"used\"})".to_string(),
        );
        per_device.plot_promql(
            PlotOpts::gauge("Free (Per-GPU)", "mem-free-per-gpu", Unit::Bytes)
                .with_row_label("GPU"),
            "sum by (id) (gpu_memory{state=\"free\"})".to_string(),
        );
    }

    let dram_bw = memory.subgroup("DRAM Bandwidth");
    dram_bw.describe("Fraction of peak memory bandwidth in use.");
    if multi_gpu {
        dram_bw.plot_promql(
            PlotOpts::gauge(
                "DRAM Bandwidth Utilization %",
                "gpu-dram-act",
                Unit::Percentage,
            )
            .percentage_range(),
            "avg(gpu_dram_bandwidth_utilization) / 100".to_string(),
        );
        dram_bw.plot_promql(
            PlotOpts::gauge(
                "DRAM Bandwidth % (Per-GPU)",
                "gpu-dram-act-per-gpu",
                Unit::Percentage,
            )
            .percentage_range()
            .with_row_label("GPU"),
            "sum by (id) (gpu_dram_bandwidth_utilization) / 100".to_string(),
        );
    } else {
        dram_bw.plot_promql_full(
            PlotOpts::gauge(
                "DRAM Bandwidth Utilization %",
                "gpu-dram-act",
                Unit::Percentage,
            )
            .percentage_range(),
            "avg(gpu_dram_bandwidth_utilization) / 100".to_string(),
        );
    }

    view.group(memory);

    let mut pcie = Group::new("PCIe", "pcie");

    let rx = pcie.subgroup("Receive");
    rx.describe("Host-to-GPU traffic over PCIe.");
    rx.plot_promql(
        PlotOpts::gauge("Total Receive Rate", "pcie-rx-per-gpu", Unit::Datarate),
        "sum(gpu_pcie_throughput{direction=\"receive\"})".to_string(),
    );
    rx.plot_promql(
        PlotOpts::gauge(
            "Receive Bandwidth Utilization %",
            "pcie-rx-util",
            Unit::Percentage,
        )
        .percentage_range(),
        "gpu_pcie_throughput{direction=\"receive\"} / ignoring(direction) gpu_pcie_bandwidth"
            .to_string(),
    );

    let tx = pcie.subgroup("Transmit");
    tx.describe("GPU-to-host traffic over PCIe.");
    tx.plot_promql(
        PlotOpts::gauge("Total Transmit Rate", "pcie-tx-per-gpu", Unit::Datarate),
        "sum(gpu_pcie_throughput{direction=\"transmit\"})".to_string(),
    );
    tx.plot_promql(
        PlotOpts::gauge(
            "Transmit Bandwidth Utilization %",
            "pcie-tx-util",
            Unit::Percentage,
        )
        .percentage_range(),
        "gpu_pcie_throughput{direction=\"transmit\"} / ignoring(direction) gpu_pcie_bandwidth"
            .to_string(),
    );

    let capacity = pcie.subgroup("Link Capacity");
    capacity.describe("Aggregate theoretical PCIe bandwidth available to all GPUs.");
    capacity.plot_promql_full(
        PlotOpts::gauge("Bandwidth", "pcie-bandwidth", Unit::Datarate),
        "sum(gpu_pcie_bandwidth)".to_string(),
    );

    view.group(pcie);

    let mut power = Group::new("Power", "power");

    let draw = power.subgroup("Power Draw");
    draw.describe("Instantaneous power consumption, total and per-GPU.");
    if multi_gpu {
        draw.plot_promql(
            PlotOpts::gauge("Power (W)", "power-watts", Unit::Count).with_axis_label("Watts"),
            "sum(gpu_power_usage) / 1000".to_string(),
        );
        draw.plot_promql(
            PlotOpts::gauge("Power (Per-GPU)", "power-watts-per-gpu", Unit::Count)
                .with_axis_label("Watts")
                .with_row_label("GPU"),
            "sum by (id) (gpu_power_usage) / 1000".to_string(),
        );
    } else {
        draw.plot_promql_full(
            PlotOpts::gauge("Power (W)", "power-watts", Unit::Count).with_axis_label("Watts"),
            "sum(gpu_power_usage) / 1000".to_string(),
        );
    }

    let energy = power.subgroup("Energy");
    energy.describe("Energy consumption rate derived from the accumulating GPU energy counter.");
    energy.plot_promql_full(
        PlotOpts::counter("Energy Rate (W)", "energy-rate", Unit::Count).with_axis_label("Watts"),
        "sum(rate(gpu_energy_consumption[5m])) / 1000".to_string(),
    );

    view.group(power);

    let mut thermal = Group::new("Temperature", "temperature");

    let temps = thermal.subgroup("Temperatures");
    temps.describe("Per-device temperatures and the hottest GPU across the system.");
    // AMD reports multiple temperature sensors per GPU (edge/junction/memory).
    // Use max (not sum) when aggregating so each value is the hottest sensor
    // reading rather than a meaningless sum across sensors. The "Max" chart is
    // the single hottest sensor across all (selected) GPUs.
    if multi_gpu {
        temps.plot_promql(
            PlotOpts::gauge("Temperature (Per-GPU)", "temp-per-gpu", Unit::Count)
                .with_axis_label("°C")
                .with_row_label("GPU"),
            "max by (id) (gpu_temperature)".to_string(),
        );
        temps.plot_promql(
            PlotOpts::gauge("Max (°C)", "temp-max", Unit::Count).with_axis_label("°C"),
            "max(gpu_temperature)".to_string(),
        );
    } else {
        temps.plot_promql_full(
            PlotOpts::gauge("Max (°C)", "temp-max", Unit::Count).with_axis_label("°C"),
            "max(gpu_temperature)".to_string(),
        );
    }

    view.group(thermal);

    let mut clocks = Group::new("Clocks", "clocks");

    let freqs = clocks.subgroup("Clock Frequencies");
    freqs.describe("Per-device clock speeds for graphics, memory, compute, and video engines.");
    freqs.plot_promql(
        PlotOpts::gauge("Graphics", "clock-graphics", Unit::Frequency),
        "sum by (id) (gpu_clock{clock=\"graphics\"})".to_string(),
    );
    freqs.plot_promql(
        PlotOpts::gauge("Memory", "clock-memory", Unit::Frequency),
        "sum by (id) (gpu_clock{clock=\"memory\"})".to_string(),
    );
    freqs.plot_promql(
        PlotOpts::gauge("Compute", "clock-compute", Unit::Frequency),
        "sum by (id) (gpu_clock{clock=\"compute\"})".to_string(),
    );
    freqs.plot_promql(
        PlotOpts::gauge("Video", "clock-video", Unit::Frequency),
        "sum by (id) (gpu_clock{clock=\"video\"})".to_string(),
    );

    view.group(clocks);

    amd_pmu(&mut view);
    intel_pmu(&mut view, multi_gpu);

    view
}

/// AMD GPU hardware performance counters (the `gpu_amd_pmu` sampler) and the
/// metrics derived from them. These raw counters are monotonic, so rates are
/// taken with `rate()`. Only populated on AMD hosts; the charts are empty
/// otherwise. See `docs/amd_gpu_pmu_events.md`.
fn amd_pmu(view: &mut View) {
    let mut pmu = Group::new("AMD GPU Performance Counters", "amd-pmu");

    // ----- GPU busy / compute -----
    let busy = pmu.subgroup("Compute Activity");
    busy.describe(
        "GPU busy fraction and vector-ALU throughput, derived from GRBM and SQ hardware counters.",
    );
    // Average number of WGPs actively executing waves = rate(SQ_BUSY_CYCLES) /
    // rate(GRBM_COUNT). SQ_BUSY_CYCLES is summed per-WGP, so dividing by total
    // clocks gives the mean count of busy WGPs.
    busy.plot_promql(
        PlotOpts::gauge("Active Workgroup Processors", "amd-active-wgp", Unit::Count)
            .with_axis_label("WGPs"),
        "sum(rate(gpmu_busy_cycles[5m])) / sum(rate(gpmu_clock[5m]))".to_string(),
    );
    busy.plot_promql(
        PlotOpts::counter("VALU Instructions/s", "amd-valu-rate", Unit::Count)
            .with_axis_label("instr/s"),
        "sum(rate(gpmu_valu_instructions[5m]))".to_string(),
    );
    busy.plot_promql(
        PlotOpts::counter("SALU Instructions/s", "amd-salu-rate", Unit::Count)
            .with_axis_label("instr/s"),
        "sum(rate(gpmu_salu_instructions[5m]))".to_string(),
    );
    busy.plot_promql(
        PlotOpts::counter("LDS Instructions/s", "amd-lds-rate", Unit::Count)
            .with_axis_label("instr/s"),
        "sum(rate(gpmu_lds_instructions[5m]))".to_string(),
    );
    // Estimated total instructions/s = VALU + SALU + LDS issue rates.
    busy.plot_promql(
        PlotOpts::counter(
            "Estimated Total Instructions (VALU + LDS + SALU)",
            "amd-total-insts",
            Unit::Count,
        )
        .with_axis_label("instr/s"),
        "sum(rate(gpmu_valu_instructions[5m])) + sum(rate(gpmu_salu_instructions[5m])) \
         + sum(rate(gpmu_lds_instructions[5m]))"
            .to_string(),
    );
    busy.plot_promql(
        PlotOpts::counter("Total Busy Cycles/s", "amd-total-busy-cycles", Unit::Count)
            .with_axis_label("cycles/s"),
        "sum(rate(gpmu_busy_cycles[5m]))".to_string(),
    );
    // Estimated IPC = estimated total instructions / total busy cycles.
    busy.plot_promql(
        PlotOpts::gauge(
            "Estimated IPC (Estimated Total Instructions / Total Busy Cycles)",
            "amd-est-ipc",
            Unit::Count,
        )
        .with_axis_label("instr/cycle"),
        "(sum(rate(gpmu_valu_instructions[5m])) + sum(rate(gpmu_salu_instructions[5m])) \
         + sum(rate(gpmu_lds_instructions[5m]))) / sum(rate(gpmu_busy_cycles[5m]))"
            .to_string(),
    );
    busy.plot_promql(
        PlotOpts::counter("Waves/s", "amd-waves-rate", Unit::Count).with_axis_label("waves/s"),
        "sum(rate(gpmu_waves[5m]))".to_string(),
    );
    // Estimated instructions per wave = estimated total instructions / waves.
    busy.plot_promql(
        PlotOpts::gauge(
            "Estimated Instructions Per Wave (Estimated Total Instructions / Waves)",
            "amd-insts-per-wave",
            Unit::Count,
        )
        .with_axis_label("instr/wave"),
        "(sum(rate(gpmu_valu_instructions[5m])) + sum(rate(gpmu_salu_instructions[5m])) \
         + sum(rate(gpmu_lds_instructions[5m]))) / sum(rate(gpmu_waves[5m]))"
            .to_string(),
    );
    busy.plot_promql(
        PlotOpts::gauge("Cycles Per Wave", "amd-cycles-per-wave", Unit::Count)
            .with_axis_label("cycles/wave"),
        "sum(rate(gpmu_busy_cycles[5m])) / sum(rate(gpmu_waves[5m]))".to_string(),
    );
    // Average resident waves per active WGP cycle = rate(SQ_WAVE_CYCLES) /
    // rate(SQ_BUSY_CYCLES). Both are per-WGP wave-cycle counters, so the ratio
    // is the mean number of waves in flight while a WGP is busy. Computed from
    // rates (not cumulative totals) so it stays correct as long as the
    // per-interval delta doesn't saturate the per-WGP 32-bit accumulator.
    busy.plot_promql(
        PlotOpts::gauge(
            "Average Waves Per Workgroup Processors",
            "amd-waves-per-wgp",
            Unit::Count,
        )
        .with_axis_label("waves/active cycle"),
        "sum(rate(gpmu_wave_cycles[5m])) / sum(rate(gpmu_busy_cycles[5m]))".to_string(),
    );
    // Average resident waves per GPU = rate(SQ_WAVE_CYCLES) /
    // rate(GRBM_GUI_ACTIVE). Normalizes wave-cycles by GPU-active cycles instead
    // of WGP-busy cycles, giving the mean waves in flight across the whole GPU.
    busy.plot_promql(
        PlotOpts::gauge("Average Waves Per GPU", "amd-waves-per-gpu", Unit::Count)
            .with_axis_label("waves/active cycle"),
        "sum(rate(gpmu_wave_cycles[5m])) / sum(rate(gpmu_active_clock[5m]))".to_string(),
    );

    // ----- Caches -----
    let caches = pmu.subgroup("Caches");
    caches.describe(
        "Instruction-cache and L2 cache hit rates, derived from SQC and GL2C hardware counters.",
    );
    caches.plot_promql(
        PlotOpts::gauge(
            "Instruction Cache Hit %",
            "amd-icache-hit",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum(rate(gpmu_icache_hits[5m])) / sum(rate(gpmu_icache_requests[5m]))".to_string(),
    );
    caches.plot_promql(
        PlotOpts::gauge("L2 Cache Hit %", "amd-l2-hit", Unit::Percentage).percentage_range(),
        "sum(rate(gpmu_l2_hits[5m])) / (sum(rate(gpmu_l2_hits[5m])) + sum(rate(gpmu_l2_misses[5m])))"
            .to_string(),
    );

    // ----- Memory bandwidth -----
    // GL2C<->VRAM requests, weighted by transaction size. RDNA reads are
    // predominantly 128-byte, writes predominantly 64-byte cache-line bursts.
    let bw = pmu.subgroup("Memory Bandwidth");
    bw.describe(
        "L2<->VRAM bandwidth, derived from GL2C read/write request counters \
         (reads weighted 128B, writes 64B).",
    );
    bw.plot_promql(
        PlotOpts::counter(
            "Estimated Read Bandwidth (VRAM Read Requests x 128B)",
            "amd-mem-read-bw",
            Unit::Datarate,
        ),
        "sum(rate(gpmu_vram_read_requests[5m])) * 128".to_string(),
    );
    bw.plot_promql(
        PlotOpts::counter(
            "Estimated Write Bandwidth (VRAM Write Requests x 64B)",
            "amd-mem-write-bw",
            Unit::Datarate,
        ),
        "sum(rate(gpmu_vram_write_requests[5m])) * 64".to_string(),
    );
    bw.plot_promql(
        PlotOpts::counter("VRAM Read Requests", "amd-vram-read-req", Unit::Count)
            .with_axis_label("requests/s"),
        "sum(rate(gpmu_vram_read_requests[5m]))".to_string(),
    );
    bw.plot_promql(
        PlotOpts::counter("VRAM Write Requests", "amd-vram-write-req", Unit::Count)
            .with_axis_label("requests/s"),
        "sum(rate(gpmu_vram_write_requests[5m]))".to_string(),
    );

    view.group(pmu);
}

/// Intel GPU metrics from the i915/xe PMU (the `gpu_intel_pmu` sampler).
///
/// Only populated on Intel hosts; the charts are empty otherwise. See
/// `docs/metrics.md#gpu_intel_pmu`.
///
/// Both PMU families are **cumulative counters**, so everything here is a
/// `rate()`. Two consequences shape the queries below:
///
/// - Engine busy is cumulative nanoseconds, so `rate()` yields nanoseconds of
///   busy time per second — dividing by 1e9 gives the busy fraction, which is
///   what the percentage charts plot.
/// - The frequency events are a running sum of one MHz sample per driver tick,
///   accumulated only while the GT is awake. `rate()` recovers average MHz over
///   the interval; a raw reading carries no meaning without its predecessor.
///   The GT parking while idle is why these drop to zero rather than to an idle
///   clock.
///
/// `gpu_memory` is deliberately absent here: the Intel sampler publishes it
/// under the same vendor-neutral name and labels the AMD and NVIDIA samplers
/// use, so it already appears in the shared Memory group above. VRAM is
/// discrete-only — an integrated GPU has no device-local memory region and
/// publishes no such series.
fn intel_pmu(view: &mut View, multi_gpu: bool) {
    let mut pmu = Group::new("Intel GPU Performance Counters", "intel-pmu");

    // ----- Engine occupancy -----
    let engines = pmu.subgroup("Engine Busy");
    engines.describe(
        "Fraction of wall time each GPU engine spent executing work, from the i915/xe PMU. \
         This is occupancy, not efficiency: an engine at 100% had work queued, which does not \
         mean the execution units were saturated — read it alongside the frequency charts.",
    );

    // Aggregate across engines of a class. Summing the busy time of several
    // engines can exceed one device-second (they run concurrently), which is
    // correct and why this is not clamped to 100%.
    engines.plot_promql(
        PlotOpts::gauge(
            "Busy % by Engine Class",
            "intel-engine-class-pct",
            Unit::Percentage,
        )
        .with_row_label("Class"),
        "sum by (engine_class) (rate(gpu_engine_busy_time[5m])) / 1000000000".to_string(),
    );
    engines.plot_promql(
        PlotOpts::gauge("Busy % by Engine", "intel-engine-pct", Unit::Percentage)
            .with_row_label("Engine"),
        "sum by (engine) (rate(gpu_engine_busy_time[5m])) / 1000000000".to_string(),
    );

    // The engine that carries the compute workload, called out on its own
    // because it is the one to watch on a GPGPU host. A discrete Arc exposes a
    // compute engine; an integrated GPU generally does not, and this chart is
    // then empty while the render one below carries the load.
    engines.plot_promql(
        PlotOpts::gauge(
            "Compute Engine Busy %",
            "intel-compute-pct",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum(rate(gpu_engine_busy_time{engine_class=\"compute\"}[5m])) / 1000000000".to_string(),
    );
    engines.plot_promql(
        PlotOpts::gauge("Render Engine Busy %", "intel-render-pct", Unit::Percentage)
            .percentage_range(),
        "sum(rate(gpu_engine_busy_time{engine_class=\"render\"}[5m])) / 1000000000".to_string(),
    );

    if multi_gpu {
        let per_device = pmu.subgroup("Per-Device Engine Busy");
        per_device.describe(
            "Engine busy time broken out by GPU id — on a host with both an integrated GPU and \
             a discrete Arc card, this is what separates them.",
        );
        per_device.plot_promql(
            PlotOpts::gauge(
                "Busy % (Per-GPU)",
                "intel-engine-pct-per-gpu",
                Unit::Percentage,
            )
            .with_row_label("GPU"),
            "sum by (id) (rate(gpu_engine_busy_time[5m])) / 1000000000".to_string(),
        );
    }

    // ----- Frequency -----
    let freq = pmu.subgroup("Frequency");
    freq.describe(
        "Average GPU frequency over each interval, recovered from the PMU's cumulative sum of \
         per-tick MHz samples. Requested is what the driver asked for; actual is what the \
         hardware delivered — a persistent gap between them means the GPU is being held back \
         (thermal, power or voltage limits). Both fall to zero when the GT parks, because the \
         driver stops sampling rather than because the clock reached zero.",
    );
    freq.plot_promql(
        PlotOpts::gauge("Actual", "intel-freq-actual", Unit::Frequency),
        "sum by (id) (rate(gpu_frequency_sample{frequency=\"actual\"}[5m])) * 1000000".to_string(),
    );
    freq.plot_promql(
        PlotOpts::gauge("Requested", "intel-freq-requested", Unit::Frequency),
        "sum by (id) (rate(gpu_frequency_sample{frequency=\"requested\"}[5m])) * 1000000"
            .to_string(),
    );

    view.group(pmu);
}
