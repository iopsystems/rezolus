use crate::MetricsSource;
use crate::plot::*;

/// The metrics we probe for GPU presence / id enumeration.
const GPU_PROBE_METRICS: &[&str] = &[
    "gpu_utilization",
    "gpu_memory",
    "gpu_temperature",
    "gpu_power_usage",
    "gpu_clock",
    "gpu_memory_utilization",
    "gpmu_clock",
];

/// True iff the recording has more than one GPU. Per-device charts are
/// suppressed when this is false because they degenerate to the aggregate.
fn has_multiple_gpus(data: &dyn MetricsSource) -> bool {
    GPU_PROBE_METRICS
        .iter()
        .any(|m| metric_unique_label_count(data, m, "id") > 1)
}

/// The distinct GPU `id` values present in the recording, sorted numerically.
fn gpu_ids(data: &dyn MetricsSource) -> Vec<i64> {
    let mut ids: Vec<i64> = GPU_PROBE_METRICS
        .iter()
        .flat_map(|m| data.label_values(m, "id"))
        .filter_map(|v| v.parse::<i64>().ok())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let multi_gpu = has_multiple_gpus(data);

    // Tell the frontend which GPU ids exist so it can render the GPU selector
    // (a dropdown to view the non-per-GPU charts for a single GPU or the
    // aggregate). Only meaningful with more than one GPU.
    let ids = gpu_ids(data);
    if ids.len() > 1 {
        view.metadata.insert(
            "gpu_selector".to_string(),
            serde_json::json!({ "enabled": true, "ids": ids }),
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
                .percentage_range().with_row_label("GPU"),
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
            .percentage_range().with_row_label("GPU"),
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
            .percentage_range().with_row_label("GPU"),
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
            .percentage_range().with_row_label("GPU"),
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
            .percentage_range().with_row_label("GPU"),
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
            PlotOpts::gauge("Used (Per-GPU)", "mem-used-per-gpu", Unit::Bytes).with_row_label("GPU"),
            "sum by (id) (gpu_memory{state=\"used\"})".to_string(),
        );
        per_device.plot_promql(
            PlotOpts::gauge("Free (Per-GPU)", "mem-free-per-gpu", Unit::Bytes).with_row_label("GPU"),
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
            .percentage_range().with_row_label("GPU"),
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
                .with_axis_label("Watts").with_row_label("GPU"),
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
                .with_axis_label("°C").with_row_label("GPU"),
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
        PlotOpts::gauge("Instruction Cache Hit %", "amd-icache-hit", Unit::Percentage)
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
