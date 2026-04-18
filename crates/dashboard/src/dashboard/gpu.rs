use crate::Tsdb;
use crate::plot::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Utilization
     */

    let mut utilization = Group::new("Utilization", "utilization");

    // GPU compute utilization (average across all GPUs)
    // NVML returns 0-100, divide by 100 for 0-1 ratio expected by Unit::Percentage
    utilization.plot_promql(
        PlotOpts::gauge("GPU %", "gpu-pct", Unit::Percentage).percentage_range(),
        "avg(gpu_utilization) / 100".to_string(),
    );

    // Per-GPU utilization heatmap
    utilization.plot_promql(
        PlotOpts::gauge("GPU % (Per-GPU)", "gpu-pct-per-gpu", Unit::Percentage).percentage_range(),
        "sum by (id) (gpu_utilization) / 100".to_string(),
    );

    // Memory controller utilization (average)
    utilization.plot_promql(
        PlotOpts::gauge("Memory Controller %", "mem-ctrl-pct", Unit::Percentage).percentage_range(),
        "avg(gpu_memory_utilization) / 100".to_string(),
    );

    // Per-GPU memory controller utilization
    utilization.plot_promql(
        PlotOpts::gauge(
            "Memory Controller % (Per-GPU)",
            "mem-ctrl-pct-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_memory_utilization) / 100".to_string(),
    );

    view.group(utilization);

    let mut activity = Group::new("Activity", "activity");

    // GPU tensor activity
    activity.plot_promql(
        PlotOpts::gauge("GPU Tensor Activity %", "gpu-tensor-act", Unit::Percentage)
            .percentage_range(),
        "avg(gpu_tensor_utilization) / 100".to_string(),
    );

    // Per-GPU GPU tensor activity
    activity.plot_promql(
        PlotOpts::gauge(
            "GPU Tensor Activity % (Per-GPU)",
            "gpu-tensor-act-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_tensor_utilization) / 100".to_string(),
    );

    // GPU DRAM activity
    activity.plot_promql(
        PlotOpts::gauge("GPU DRAM Activity %", "gpu-dram-act", Unit::Percentage).percentage_range(),
        "avg(gpu_dram_bandwidth_utilization) / 100".to_string(),
    );

    // Per-GPU GPU DRAM activity
    activity.plot_promql(
        PlotOpts::gauge(
            "GPU DRAM Activity % (Per-GPU)",
            "gpu-dram-act-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_dram_bandwidth_utilization) / 100".to_string(),
    );

    // GPU SM activity
    activity.plot_promql(
        PlotOpts::gauge("GPU SM Activity %", "gpu-sm-act", Unit::Percentage).percentage_range(),
        "avg(gpu_sm_utilization) / 100".to_string(),
    );

    // Per-GPU GPU SM activity
    activity.plot_promql(
        PlotOpts::gauge(
            "GPU SM Activity % (Per-GPU)",
            "gpu-sm-act-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_sm_utilization) / 100".to_string(),
    );

    // GPU SM occupancy (active warps / max warps, average across all GPUs)
    activity.plot_promql(
        PlotOpts::gauge("GPU SM Occupancy %", "gpu-sm-ocp", Unit::Percentage).percentage_range(),
        "avg(gpu_sm_occupancy) / 100".to_string(),
    );

    // Per-GPU GPU SM occupancy
    activity.plot_promql(
        PlotOpts::gauge(
            "GPU SM Occupancy % (Per-GPU)",
            "gpu-sm-ocp-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_sm_occupancy) / 100".to_string(),
    );

    view.group(activity);

    /*
     * Memory
     */

    let mut memory = Group::new("Memory", "memory");

    // Total memory used (sum across all GPUs)
    memory.plot_promql(
        PlotOpts::gauge("Used", "mem-used", Unit::Bytes),
        "sum(gpu_memory{state=\"used\"})".to_string(),
    );

    // Per-GPU memory used
    memory.plot_promql(
        PlotOpts::gauge("Used (Per-GPU)", "mem-used-per-gpu", Unit::Bytes),
        "sum by (id) (gpu_memory{state=\"used\"})".to_string(),
    );

    // Total memory free (sum across all GPUs)
    memory.plot_promql(
        PlotOpts::gauge("Free", "mem-free", Unit::Bytes),
        "sum(gpu_memory{state=\"free\"})".to_string(),
    );

    // Per-GPU memory free
    memory.plot_promql(
        PlotOpts::gauge("Free (Per-GPU)", "mem-free-per-gpu", Unit::Bytes),
        "sum by (id) (gpu_memory{state=\"free\"})".to_string(),
    );

    // Memory utilization percentage (calculated)
    memory.plot_promql(
        PlotOpts::gauge("Utilization %", "mem-util-pct", Unit::Percentage).percentage_range(),
        "sum(gpu_memory{state=\"used\"}) / (sum(gpu_memory{state=\"used\"}) + sum(gpu_memory{state=\"free\"}))".to_string(),
    );

    view.group(memory);

    /*
     * Power
     */

    let mut power = Group::new("Power", "power");

    // Total power usage (sum across all GPUs, convert mW to W)
    power.plot_promql(
        PlotOpts::gauge("Power (W)", "power-watts", Unit::Count).with_axis_label("Watts"),
        "sum(gpu_power_usage) / 1000".to_string(),
    );

    // Per-GPU power usage (in Watts)
    power.plot_promql(
        PlotOpts::gauge("Power (Per-GPU)", "power-watts-per-gpu", Unit::Count)
            .with_axis_label("Watts"),
        "sum by (id) (gpu_power_usage) / 1000".to_string(),
    );

    // Energy consumption rate (convert mJ counter to Watts via rate)
    power.plot_promql(
        PlotOpts::counter("Energy Rate (W)", "energy-rate", Unit::Count).with_axis_label("Watts"),
        "sum(rate(gpu_energy_consumption[5m])) / 1000".to_string(),
    );

    view.group(power);

    /*
     * Temperature
     */

    let mut thermal = Group::new("Temperature", "temperature");

    // Average temperature across all GPUs
    thermal.plot_promql(
        PlotOpts::gauge("Average (°C)", "temp-avg", Unit::Count).with_axis_label("°C"),
        "avg(gpu_temperature)".to_string(),
    );

    // Max temperature across all GPUs
    thermal.plot_promql(
        PlotOpts::gauge("Max (°C)", "temp-max", Unit::Count).with_axis_label("°C"),
        "max(gpu_temperature)".to_string(),
    );

    // Per-GPU temperature
    thermal.plot_promql(
        PlotOpts::gauge("Temperature (Per-GPU)", "temp-per-gpu", Unit::Count).with_axis_label("°C"),
        "sum by (id) (gpu_temperature)".to_string(),
    );

    view.group(thermal);

    /*
     * Clocks
     */

    let mut clocks = Group::new("Clocks", "clocks");

    // Graphics clock (average)
    clocks.plot_promql(
        PlotOpts::gauge("Graphics", "clock-graphics", Unit::Frequency),
        "avg(gpu_clock{clock=\"graphics\"})".to_string(),
    );

    // Per-GPU graphics clock
    clocks.plot_promql(
        PlotOpts::gauge(
            "Graphics (Per-GPU)",
            "clock-graphics-per-gpu",
            Unit::Frequency,
        ),
        "sum by (id) (gpu_clock{clock=\"graphics\"})".to_string(),
    );

    // Memory clock (average)
    clocks.plot_promql(
        PlotOpts::gauge("Memory", "clock-memory", Unit::Frequency),
        "avg(gpu_clock{clock=\"memory\"})".to_string(),
    );

    // Per-GPU memory clock
    clocks.plot_promql(
        PlotOpts::gauge("Memory (Per-GPU)", "clock-memory-per-gpu", Unit::Frequency),
        "sum by (id) (gpu_clock{clock=\"memory\"})".to_string(),
    );

    // Compute clock (average)
    clocks.plot_promql(
        PlotOpts::gauge("Compute", "clock-compute", Unit::Frequency),
        "avg(gpu_clock{clock=\"compute\"})".to_string(),
    );

    // Video clock (average)
    clocks.plot_promql(
        PlotOpts::gauge("Video", "clock-video", Unit::Frequency),
        "avg(gpu_clock{clock=\"video\"})".to_string(),
    );

    view.group(clocks);

    /*
     * PCIe
     */

    let mut pcie = Group::new("PCIe", "pcie");

    // PCIe bandwidth (max theoretical)
    pcie.plot_promql(
        PlotOpts::gauge("Bandwidth", "pcie-bandwidth", Unit::Datarate),
        "sum(gpu_pcie_bandwidth)".to_string(),
    );

    // PCIe receive throughput (total)
    pcie.plot_promql(
        PlotOpts::gauge("Receive", "pcie-rx", Unit::Datarate),
        "sum(gpu_pcie_throughput{direction=\"receive\"})".to_string(),
    );

    // Per-GPU receive throughput
    pcie.plot_promql(
        PlotOpts::gauge("Receive (Per-GPU)", "pcie-rx-per-gpu", Unit::Datarate),
        "sum by (id) (gpu_pcie_throughput{direction=\"receive\"})".to_string(),
    );

    // PCIe transmit throughput (total)
    pcie.plot_promql(
        PlotOpts::gauge("Transmit", "pcie-tx", Unit::Datarate),
        "sum(gpu_pcie_throughput{direction=\"transmit\"})".to_string(),
    );

    // Per-GPU transmit throughput
    pcie.plot_promql(
        PlotOpts::gauge("Transmit (Per-GPU)", "pcie-tx-per-gpu", Unit::Datarate),
        "sum by (id) (gpu_pcie_throughput{direction=\"transmit\"})".to_string(),
    );

    view.group(pcie);

    view
}
