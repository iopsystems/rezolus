use crate::data::DashboardData;
use crate::plot::*;

// Helper: gauge avg across columns matching a regex, divided by `divisor`.
// Used for the many `avg(gpu_X) / 100` percentages.
fn gauge_avg_div(re: &str, divisor: f64) -> String {
    format!(
        r#"SELECT timestamp::DOUBLE/1e9 AS t,
                  list_avg([*COLUMNS('{re}')]::BIGINT[]) / {divisor:.1} AS v
           FROM _src"#
    )
}

// Helper: per-id fan-out via UNPIVOT, returning gauge value / divisor.
// Used for `sum by (id) (gpu_X) / 100` percentages and per-GPU values.
fn gauge_by_id_div(re: &str, divisor: f64) -> String {
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src)
                  ON COLUMNS('{re}')
                  INTO NAME col VALUE v
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  regexp_extract(col, '/([0-9]+)$', 1) AS id,
                  v::DOUBLE / {divisor:.1} AS v
           FROM unp"#
    )
}

// Helper: gauge sum across columns matching a regex.
fn gauge_sum(re: &str) -> String {
    format!(
        r#"SELECT timestamp::DOUBLE/1e9 AS t,
                  list_sum([*COLUMNS('{re}')]::BIGINT[])::DOUBLE AS v
           FROM _src"#
    )
}

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Utilization
     */

    let mut utilization = Group::new("Utilization", "utilization");

    let gpu = utilization.subgroup("GPU Utilization");
    gpu.describe("Fraction of time the GPU has work scheduled, averaged and per-device.");
    gpu.plot_promql_with_sql(
        PlotOpts::gauge("GPU %", "gpu-pct", Unit::Percentage).percentage_range(),
        "avg(gpu_utilization) / 100".to_string(),
        gauge_avg_div("^gpu_utilization/[0-9]+$", 100.0),
    );
    gpu.plot_promql_with_sql(
        PlotOpts::gauge("GPU % (Per-GPU)", "gpu-pct-per-gpu", Unit::Percentage).percentage_range(),
        "sum by (id) (gpu_utilization) / 100".to_string(),
        gauge_by_id_div("^gpu_utilization/[0-9]+$", 100.0),
    );

    let mem_ctrl = utilization.subgroup("Memory Controller");
    mem_ctrl.describe("Fraction of time the memory controller is servicing requests.");
    mem_ctrl.plot_promql_with_sql(
        PlotOpts::gauge("Memory Controller %", "mem-ctrl-pct", Unit::Percentage).percentage_range(),
        "avg(gpu_memory_utilization) / 100".to_string(),
        gauge_avg_div("^gpu_memory_utilization/[0-9]+$", 100.0),
    );
    mem_ctrl.plot_promql_with_sql(
        PlotOpts::gauge(
            "Memory Controller % (Per-GPU)",
            "mem-ctrl-pct-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_memory_utilization) / 100".to_string(),
        gauge_by_id_div("^gpu_memory_utilization/[0-9]+$", 100.0),
    );

    view.group(utilization);

    let mut activity = Group::new("Parallel Compute Activity", "activity");

    let tensor = activity.subgroup("Tensor Activity");
    tensor.describe("Tensor core utilization — how busy the matrix-math units are.");
    tensor.plot_promql_with_sql(
        PlotOpts::gauge("GPU Tensor Activity %", "gpu-tensor-act", Unit::Percentage)
            .percentage_range(),
        "avg(gpu_tensor_utilization) / 100".to_string(),
        gauge_avg_div("^gpu_tensor_utilization/[0-9]+$", 100.0),
    );
    tensor.plot_promql_with_sql(
        PlotOpts::gauge(
            "GPU Tensor Activity % (Per-GPU)",
            "gpu-tensor-act-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_tensor_utilization) / 100".to_string(),
        gauge_by_id_div("^gpu_tensor_utilization/[0-9]+$", 100.0),
    );

    let sm = activity.subgroup("SM Activity & Occupancy");
    sm.describe("Streaming multiprocessor active time and warp occupancy — core indicators of compute efficiency.");
    sm.plot_promql_with_sql(
        PlotOpts::gauge("GPU SM Activity %", "gpu-sm-act", Unit::Percentage).percentage_range(),
        "avg(gpu_sm_utilization) / 100".to_string(),
        gauge_avg_div("^gpu_sm_utilization/[0-9]+$", 100.0),
    );
    sm.plot_promql_with_sql(
        PlotOpts::gauge(
            "GPU SM Activity % (Per-GPU)",
            "gpu-sm-act-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_sm_utilization) / 100".to_string(),
        gauge_by_id_div("^gpu_sm_utilization/[0-9]+$", 100.0),
    );
    sm.plot_promql_with_sql(
        PlotOpts::gauge("GPU SM Occupancy %", "gpu-sm-ocp", Unit::Percentage).percentage_range(),
        "avg(gpu_sm_occupancy) / 100".to_string(),
        gauge_avg_div("^gpu_sm_occupancy/[0-9]+$", 100.0),
    );
    sm.plot_promql_with_sql(
        PlotOpts::gauge(
            "GPU SM Occupancy % (Per-GPU)",
            "gpu-sm-ocp-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_sm_occupancy) / 100".to_string(),
        gauge_by_id_div("^gpu_sm_occupancy/[0-9]+$", 100.0),
    );

    view.group(activity);

    /*
     * Memory
     */

    let mut memory = Group::new("Memory", "memory");

    let capacity = memory.subgroup("Capacity");
    capacity.describe("Total GPU memory used and free across all devices, plus overall utilization.");
    capacity.plot_promql_with_sql(
        PlotOpts::gauge("Used", "mem-used", Unit::Bytes),
        "sum(gpu_memory{state=\"used\"})".to_string(),
        gauge_sum("^gpu_memory/used/[0-9]+$"),
    );
    capacity.plot_promql_with_sql(
        PlotOpts::gauge("Free", "mem-free", Unit::Bytes),
        "sum(gpu_memory{state=\"free\"})".to_string(),
        gauge_sum("^gpu_memory/free/[0-9]+$"),
    );
    capacity.plot_promql_with_sql_full(
        PlotOpts::gauge("Memory Utilization %", "mem-util-pct", Unit::Percentage).percentage_range(),
        "sum(gpu_memory{state=\"used\"}) / (sum(gpu_memory{state=\"used\"}) + sum(gpu_memory{state=\"free\"}))".to_string(),
        // Multiple STAR/COLUMNS in one expression are rejected — split
        // each list_sum into its own CTE projection (see duckdb.md).
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^gpu_memory/used/[0-9]+$')]::BIGINT[])::DOUBLE AS used,
                     list_sum([*COLUMNS('^gpu_memory/free/[0-9]+$')]::BIGINT[])::DOUBLE AS free
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  used / NULLIF(used + free, 0) AS v
           FROM agg"#.to_string(),
    );

    let per_device = memory.subgroup("Per-Device Capacity");
    per_device.describe("Memory used and free broken out by GPU id.");
    per_device.plot_promql_with_sql(
        PlotOpts::gauge("Used (Per-GPU)", "mem-used-per-gpu", Unit::Bytes),
        "sum by (id) (gpu_memory{state=\"used\"})".to_string(),
        gauge_by_id_div("^gpu_memory/used/[0-9]+$", 1.0),
    );
    per_device.plot_promql_with_sql(
        PlotOpts::gauge("Free (Per-GPU)", "mem-free-per-gpu", Unit::Bytes),
        "sum by (id) (gpu_memory{state=\"free\"})".to_string(),
        gauge_by_id_div("^gpu_memory/free/[0-9]+$", 1.0),
    );

    let dram_bw = memory.subgroup("DRAM Bandwidth");
    dram_bw.describe("Fraction of peak memory bandwidth in use.");
    dram_bw.plot_promql_with_sql(
        PlotOpts::gauge(
            "DRAM Bandwidth Utilization %",
            "gpu-dram-act",
            Unit::Percentage,
        )
        .percentage_range(),
        "avg(gpu_dram_bandwidth_utilization) / 100".to_string(),
        gauge_avg_div("^gpu_dram_bandwidth_utilization/[0-9]+$", 100.0),
    );
    dram_bw.plot_promql_with_sql(
        PlotOpts::gauge(
            "DRAM Bandwidth % (Per-GPU)",
            "gpu-dram-act-per-gpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (gpu_dram_bandwidth_utilization) / 100".to_string(),
        gauge_by_id_div("^gpu_dram_bandwidth_utilization/[0-9]+$", 100.0),
    );

    view.group(memory);

    /*
     * PCIe
     */

    let mut pcie = Group::new("PCIe", "pcie");

    let rx = pcie.subgroup("Receive");
    rx.describe("Host-to-GPU traffic over PCIe.");
    rx.plot_promql_with_sql(
        PlotOpts::gauge("Total Receive Rate", "pcie-rx-per-gpu", Unit::Datarate),
        "sum(gpu_pcie_throughput{direction=\"receive\"})".to_string(),
        gauge_sum("^gpu_pcie_throughput/receive/[0-9]+$"),
    );
    rx.plot_promql_with_sql(
        PlotOpts::gauge(
            "Receive Bandwidth Utilization %",
            "pcie-rx-util",
            Unit::Percentage,
        )
        .percentage_range(),
        "gpu_pcie_throughput{direction=\"receive\"} / ignoring(direction) gpu_pcie_bandwidth"
            .to_string(),
        // PromQL `ignoring(direction)` matches throughput (which has the
        // direction label) against bandwidth (which doesn't). In SQL, the
        // bandwidth column has no direction in its name, so just sum each
        // and divide.
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^gpu_pcie_throughput/receive/[0-9]+$')]::BIGINT[])::DOUBLE AS rx,
                     list_sum([*COLUMNS('^gpu_pcie_bandwidth/[0-9]+$')]::BIGINT[])::DOUBLE AS bw
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, rx / NULLIF(bw, 0) AS v FROM agg"#.to_string(),
    );

    let tx = pcie.subgroup("Transmit");
    tx.describe("GPU-to-host traffic over PCIe.");
    tx.plot_promql_with_sql(
        PlotOpts::gauge("Total Transmit Rate", "pcie-tx-per-gpu", Unit::Datarate),
        "sum(gpu_pcie_throughput{direction=\"transmit\"})".to_string(),
        gauge_sum("^gpu_pcie_throughput/transmit/[0-9]+$"),
    );
    tx.plot_promql_with_sql(
        PlotOpts::gauge(
            "Transmit Bandwidth Utilization %",
            "pcie-tx-util",
            Unit::Percentage,
        )
        .percentage_range(),
        "gpu_pcie_throughput{direction=\"transmit\"} / ignoring(direction) gpu_pcie_bandwidth"
            .to_string(),
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^gpu_pcie_throughput/transmit/[0-9]+$')]::BIGINT[])::DOUBLE AS tx,
                     list_sum([*COLUMNS('^gpu_pcie_bandwidth/[0-9]+$')]::BIGINT[])::DOUBLE AS bw
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, tx / NULLIF(bw, 0) AS v FROM agg"#.to_string(),
    );

    let capacity = pcie.subgroup("Link Capacity");
    capacity.describe("Aggregate theoretical PCIe bandwidth available to all GPUs.");
    capacity.plot_promql_with_sql_full(
        PlotOpts::gauge("Bandwidth", "pcie-bandwidth", Unit::Datarate),
        "sum(gpu_pcie_bandwidth)".to_string(),
        gauge_sum("^gpu_pcie_bandwidth/[0-9]+$"),
    );

    view.group(pcie);

    /*
     * Power
     */

    let mut power = Group::new("Power", "power");

    let draw = power.subgroup("Power Draw");
    draw.describe("Instantaneous power consumption, total and per-GPU.");
    draw.plot_promql_with_sql(
        PlotOpts::gauge("Power (W)", "power-watts", Unit::Count).with_axis_label("Watts"),
        "sum(gpu_power_usage) / 1000".to_string(),
        // Native gpu_power_usage is in milliwatts; divide to get watts.
        r#"SELECT timestamp::DOUBLE/1e9 AS t,
                  list_sum([*COLUMNS('^gpu_power_usage/[0-9]+$')]::BIGINT[])::DOUBLE / 1000.0 AS v
           FROM _src"#.to_string(),
    );
    draw.plot_promql_with_sql(
        PlotOpts::gauge("Power (Per-GPU)", "power-watts-per-gpu", Unit::Count)
            .with_axis_label("Watts"),
        "sum by (id) (gpu_power_usage) / 1000".to_string(),
        gauge_by_id_div("^gpu_power_usage/[0-9]+$", 1000.0),
    );

    let energy = power.subgroup("Energy");
    energy.describe("Energy consumption rate derived from the accumulating GPU energy counter.");
    energy.plot_promql_with_sql_full(
        PlotOpts::counter("Energy Rate (W)", "energy-rate", Unit::Count).with_axis_label("Watts"),
        "sum(rate(gpu_energy_consumption[5m])) / 1000".to_string(),
        // Counter rate (5m windowed) of the energy accumulator across GPUs,
        // converted from mJ/s to W (= J/s).
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^gpu_energy_consumption(/.+)?$')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  rate_5m(s, timestamp) / 1000.0 AS v
           FROM agg"#.to_string(),
    );

    view.group(power);

    /*
     * Temperature
     */

    let mut thermal = Group::new("Temperature", "temperature");

    let temps = thermal.subgroup("Temperatures");
    temps.describe("Per-device temperatures and the hottest GPU across the system.");
    temps.plot_promql_with_sql(
        PlotOpts::gauge("Temperature (Per-GPU)", "temp-per-gpu", Unit::Count).with_axis_label("°C"),
        "sum by (id) (gpu_temperature)".to_string(),
        gauge_by_id_div("^gpu_temperature/[0-9]+$", 1.0),
    );
    temps.plot_promql_with_sql(
        PlotOpts::gauge("Max (°C)", "temp-max", Unit::Count).with_axis_label("°C"),
        "max(gpu_temperature)".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t,
                  list_max([*COLUMNS('^gpu_temperature/[0-9]+$')]::BIGINT[])::DOUBLE AS v
           FROM _src"#.to_string(),
    );

    view.group(thermal);

    /*
     * Clocks
     */

    let mut clocks = Group::new("Clocks", "clocks");

    let freqs = clocks.subgroup("Clock Frequencies");
    freqs.describe("Per-device clock speeds for graphics, memory, compute, and video engines.");
    for (label, id, clock) in &[
        ("Graphics", "clock-graphics", "graphics"),
        ("Memory", "clock-memory", "memory"),
        ("Compute", "clock-compute", "compute"),
        ("Video", "clock-video", "video"),
    ] {
        freqs.plot_promql_with_sql(
            PlotOpts::gauge(*label, *id, Unit::Frequency),
            format!("sum by (id) (gpu_clock{{clock=\"{clock}\"}})"),
            gauge_by_id_div(&format!("^gpu_clock/{clock}/[0-9]+$"), 1.0),
        );
    }

    view.group(clocks);

    view
}
