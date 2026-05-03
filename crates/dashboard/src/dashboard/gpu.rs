use crate::Tsdb;
use crate::plot::*;

/// True iff the recording has more than one GPU. Per-device charts are
/// suppressed when this is false because they degenerate to the aggregate.
fn has_multiple_gpus(data: &Tsdb) -> bool {
    [
        "gpu_utilization",
        "gpu_memory",
        "gpu_temperature",
        "gpu_power_usage",
        "gpu_clock",
        "gpu_memory_utilization",
        "gpu_dram_bandwidth_utilization",
    ]
    .iter()
    .any(|m| metric_unique_label_count(data, m, "id") > 1)
}

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let multi_gpu = has_multiple_gpus(data);

    /*
     * Utilization
     */

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
                .percentage_range(),
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
            .percentage_range(),
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
            .percentage_range(),
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
            .percentage_range(),
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
            .percentage_range(),
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

    /*
     * Memory
     */

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
            PlotOpts::gauge("Used (Per-GPU)", "mem-used-per-gpu", Unit::Bytes),
            "sum by (id) (gpu_memory{state=\"used\"})".to_string(),
        );
        per_device.plot_promql(
            PlotOpts::gauge("Free (Per-GPU)", "mem-free-per-gpu", Unit::Bytes),
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
            .percentage_range(),
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

    /*
     * PCIe
     */

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

    /*
     * Power
     */

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
                .with_axis_label("Watts"),
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

    /*
     * Temperature
     */

    let mut thermal = Group::new("Temperature", "temperature");

    let temps = thermal.subgroup("Temperatures");
    temps.describe("Per-device temperatures and the hottest GPU across the system.");
    if multi_gpu {
        temps.plot_promql(
            PlotOpts::gauge("Temperature (Per-GPU)", "temp-per-gpu", Unit::Count)
                .with_axis_label("°C"),
            "sum by (id) (gpu_temperature)".to_string(),
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

    /*
     * Clocks
     */

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

    view
}
