use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;

// Helper: gauge avg across all (source, id) entries matching `re`,
// divided by `divisor`. Used for the many `avg(gpu_X) / 100`
// percentages.
//
// `re` matches per-id columns like `gpu_utilization/0`; the SQL
// rewrites it to `gpu_utilization/0:src[0-9]+$` so the same template
// works in single-source mode (one `:src0` alias per id, identical
// to the base column) and combined-rezolus mode (one `:src<i>` alias
// per (source, id), exposed by the registry's combined view). PromQL
// `avg(metric)` averages over every series — i.e. every (source, id)
// pair — and the `:src<i>` aliasing is what lets us do that here.
fn gauge_avg_div(re: &str, divisor: f64) -> String {
    let by_src = re_with_src_suffix(re);
    format!(
        r#"SELECT timestamp::DOUBLE/1e9 AS t,
                  list_avg([*COLUMNS('{by_src}')]::BIGINT[]) / {divisor:.1} AS v
           FROM _src"#
    )
}

// Append `:src[0-9]+$` to a per-id regex, replacing the trailing `$`
// anchor. The registry's source views expose `<col>:src<i>` aliases;
// matching against the suffixed form picks up every (source, id)
// entry.
fn re_with_src_suffix(re: &str) -> String {
    let trimmed = re.strip_suffix('$').unwrap_or(re);
    format!("{trimmed}:src[0-9]+$")
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

/// True iff the recording has more than one GPU. Per-device charts are
/// suppressed when this is false because they degenerate to the aggregate.
fn has_multiple_gpus(data: &dyn DashboardData) -> bool {
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
    .any(|m| data.unique_label_values(m, "id") > 1)
}

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let multi_gpu = has_multiple_gpus(data);

    let mut utilization = Group::new("Utilization", "utilization");

    let gpu = utilization.subgroup("GPU Utilization");
    gpu.describe("Fraction of time the GPU has work scheduled, averaged and per-device.");
    if multi_gpu {
        gpu.plot_sql(
            PlotOpts::gauge("GPU %", "gpu-pct", Unit::Percentage).percentage_range(),
            gauge_avg_div("^gpu_utilization/[0-9]+$", 100.0),
        );
        gpu.plot_sql(
            PlotOpts::gauge("GPU % (Per-GPU)", "gpu-pct-per-gpu", Unit::Percentage)
                .percentage_range(),
            gauge_by_id_div("^gpu_utilization/[0-9]+$", 100.0),
        );
    } else {
        gpu.plot_sql_full(
            PlotOpts::gauge("GPU %", "gpu-pct", Unit::Percentage).percentage_range(),
            gauge_avg_div("^gpu_utilization/[0-9]+$", 100.0),
        );
    }

    let mem_ctrl = utilization.subgroup("Memory Controller");
    mem_ctrl.describe("Fraction of time the memory controller is servicing requests.");
    if multi_gpu {
        mem_ctrl.plot_sql(
            PlotOpts::gauge("Memory Controller %", "mem-ctrl-pct", Unit::Percentage)
                .percentage_range(),
            gauge_avg_div("^gpu_memory_utilization/[0-9]+$", 100.0),
        );
        mem_ctrl.plot_sql(
            PlotOpts::gauge(
                "Memory Controller % (Per-GPU)",
                "mem-ctrl-pct-per-gpu",
                Unit::Percentage,
            )
            .percentage_range(),
            gauge_by_id_div("^gpu_memory_utilization/[0-9]+$", 100.0),
        );
    } else {
        mem_ctrl.plot_sql_full(
            PlotOpts::gauge("Memory Controller %", "mem-ctrl-pct", Unit::Percentage)
                .percentage_range(),
            gauge_avg_div("^gpu_memory_utilization/[0-9]+$", 100.0),
        );
    }

    view.group(utilization);

    let mut activity = Group::new("Parallel Compute Activity", "activity");

    let tensor = activity.subgroup("Tensor Activity");
    tensor.describe("Tensor core utilization — how busy the matrix-math units are.");
    if multi_gpu {
        tensor.plot_sql(
            PlotOpts::gauge("GPU Tensor Activity %", "gpu-tensor-act", Unit::Percentage)
                .percentage_range(),
            gauge_avg_div("^gpu_tensor_utilization/[0-9]+$", 100.0),
        );
        tensor.plot_sql(
            PlotOpts::gauge(
                "GPU Tensor Activity % (Per-GPU)",
                "gpu-tensor-act-per-gpu",
                Unit::Percentage,
            )
            .percentage_range(),
            gauge_by_id_div("^gpu_tensor_utilization/[0-9]+$", 100.0),
        );
    } else {
        tensor.plot_sql_full(
            PlotOpts::gauge("GPU Tensor Activity %", "gpu-tensor-act", Unit::Percentage)
                .percentage_range(),
            gauge_avg_div("^gpu_tensor_utilization/[0-9]+$", 100.0),
        );
    }

    let sm = activity.subgroup("SM Activity & Occupancy");
    sm.describe("Streaming multiprocessor active time and warp occupancy — core indicators of compute efficiency.");
    if multi_gpu {
        sm.plot_sql(
            PlotOpts::gauge("GPU SM Activity %", "gpu-sm-act", Unit::Percentage).percentage_range(),
            gauge_avg_div("^gpu_sm_utilization/[0-9]+$", 100.0),
        );
        sm.plot_sql(
            PlotOpts::gauge(
                "GPU SM Activity % (Per-GPU)",
                "gpu-sm-act-per-gpu",
                Unit::Percentage,
            )
            .percentage_range(),
            gauge_by_id_div("^gpu_sm_utilization/[0-9]+$", 100.0),
        );
        sm.plot_sql(
            PlotOpts::gauge("GPU SM Occupancy %", "gpu-sm-ocp", Unit::Percentage)
                .percentage_range(),
            gauge_avg_div("^gpu_sm_occupancy/[0-9]+$", 100.0),
        );
        sm.plot_sql(
            PlotOpts::gauge(
                "GPU SM Occupancy % (Per-GPU)",
                "gpu-sm-ocp-per-gpu",
                Unit::Percentage,
            )
            .percentage_range(),
            gauge_by_id_div("^gpu_sm_occupancy/[0-9]+$", 100.0),
        );
    } else {
        sm.plot_sql_full(
            PlotOpts::gauge("GPU SM Activity %", "gpu-sm-act", Unit::Percentage).percentage_range(),
            gauge_avg_div("^gpu_sm_utilization/[0-9]+$", 100.0),
        );
        sm.plot_sql_full(
            PlotOpts::gauge("GPU SM Occupancy %", "gpu-sm-ocp", Unit::Percentage)
                .percentage_range(),
            gauge_avg_div("^gpu_sm_occupancy/[0-9]+$", 100.0),
        );
    }

    view.group(activity);

    let mut memory = Group::new("Memory", "memory");

    let capacity = memory.subgroup("Capacity");
    capacity
        .describe("Total GPU memory used and free across all devices, plus overall utilization.");
    capacity.plot_sql(
        PlotOpts::gauge("Used", "mem-used", Unit::Bytes),
        gauge_sum("^gpu_memory/used/[0-9]+$"),
    );
    capacity.plot_sql(
        PlotOpts::gauge("Free", "mem-free", Unit::Bytes),
        gauge_sum("^gpu_memory/free/[0-9]+$"),
    );
    capacity.plot_sql_full(
        PlotOpts::gauge("Memory Utilization %", "mem-util-pct", Unit::Percentage)
            .percentage_range(),
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
           FROM agg"#
            .to_string(),
    );

    if multi_gpu {
        let per_device = memory.subgroup("Per-Device Capacity");
        per_device.describe("Memory used and free broken out by GPU id.");
        per_device.plot_sql(
            PlotOpts::gauge("Used (Per-GPU)", "mem-used-per-gpu", Unit::Bytes),
            gauge_by_id_div("^gpu_memory/used/[0-9]+$", 1.0),
        );
        per_device.plot_sql(
            PlotOpts::gauge("Free (Per-GPU)", "mem-free-per-gpu", Unit::Bytes),
            gauge_by_id_div("^gpu_memory/free/[0-9]+$", 1.0),
        );
    }

    let dram_bw = memory.subgroup("DRAM Bandwidth");
    dram_bw.describe("Fraction of peak memory bandwidth in use.");
    if multi_gpu {
        dram_bw.plot_sql(
            PlotOpts::gauge(
                "DRAM Bandwidth Utilization %",
                "gpu-dram-act",
                Unit::Percentage,
            )
            .percentage_range(),
            gauge_avg_div("^gpu_dram_bandwidth_utilization/[0-9]+$", 100.0),
        );
        dram_bw.plot_sql(
            PlotOpts::gauge(
                "DRAM Bandwidth % (Per-GPU)",
                "gpu-dram-act-per-gpu",
                Unit::Percentage,
            )
            .percentage_range(),
            gauge_by_id_div("^gpu_dram_bandwidth_utilization/[0-9]+$", 100.0),
        );
    } else {
        dram_bw.plot_sql_full(
            PlotOpts::gauge(
                "DRAM Bandwidth Utilization %",
                "gpu-dram-act",
                Unit::Percentage,
            )
            .percentage_range(),
            gauge_avg_div("^gpu_dram_bandwidth_utilization/[0-9]+$", 100.0),
        );
    }

    view.group(memory);

    let mut pcie = Group::new("PCIe", "pcie");

    let rx = pcie.subgroup("Receive");
    rx.describe("Host-to-GPU traffic over PCIe.");
    rx.plot_sql(
        PlotOpts::gauge("Total Receive Rate", "pcie-rx-per-gpu", Unit::Datarate),
        gauge_sum("^gpu_pcie_throughput/receive/[0-9]+$"),
    );
    rx.plot_sql(
        PlotOpts::gauge(
            "Receive Bandwidth Utilization %",
            "pcie-rx-util",
            Unit::Percentage,
        )
        .percentage_range(),
        // PromQL `ignoring(direction)` matches throughput (which has the
        // direction label) against bandwidth (which doesn't). In SQL, the
        // bandwidth column has no direction in its name, so just sum each
        // and divide.
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^gpu_pcie_throughput/receive/[0-9]+$')]::BIGINT[])::DOUBLE AS rx,
                     list_sum([*COLUMNS('^gpu_pcie_bandwidth(/[a-z]+)?/[0-9]+$')]::BIGINT[])::DOUBLE AS bw
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, rx / NULLIF(bw, 0) AS v FROM agg"#.to_string(),
    );

    let tx = pcie.subgroup("Transmit");
    tx.describe("GPU-to-host traffic over PCIe.");
    tx.plot_sql(
        PlotOpts::gauge("Total Transmit Rate", "pcie-tx-per-gpu", Unit::Datarate),
        gauge_sum("^gpu_pcie_throughput/transmit/[0-9]+$"),
    );
    tx.plot_sql(
        PlotOpts::gauge(
            "Transmit Bandwidth Utilization %",
            "pcie-tx-util",
            Unit::Percentage,
        )
        .percentage_range(),
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^gpu_pcie_throughput/transmit/[0-9]+$')]::BIGINT[])::DOUBLE AS tx,
                     list_sum([*COLUMNS('^gpu_pcie_bandwidth(/[a-z]+)?/[0-9]+$')]::BIGINT[])::DOUBLE AS bw
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, tx / NULLIF(bw, 0) AS v FROM agg"#.to_string(),
    );

    let capacity = pcie.subgroup("Link Capacity");
    capacity.describe("Aggregate theoretical PCIe bandwidth available to all GPUs.");
    capacity.plot_sql_full(
        PlotOpts::gauge("Bandwidth", "pcie-bandwidth", Unit::Datarate),
        gauge_sum("^gpu_pcie_bandwidth(/[a-z]+)?/[0-9]+$"),
    );

    view.group(pcie);

    let mut power = Group::new("Power", "power");

    let draw = power.subgroup("Power Draw");
    draw.describe("Instantaneous power consumption, total and per-GPU.");
    if multi_gpu {
        draw.plot_sql(
            PlotOpts::gauge("Power (W)", "power-watts", Unit::Count).with_axis_label("Watts"),
            // Native gpu_power_usage is in milliwatts; divide to get watts.
            r#"SELECT timestamp::DOUBLE/1e9 AS t,
                      list_sum([*COLUMNS('^gpu_power_usage/[0-9]+$')]::BIGINT[])::DOUBLE / 1000.0 AS v
               FROM _src"#.to_string(),
        );
        draw.plot_sql(
            PlotOpts::gauge("Power (Per-GPU)", "power-watts-per-gpu", Unit::Count)
                .with_axis_label("Watts"),
            gauge_by_id_div("^gpu_power_usage/[0-9]+$", 1000.0),
        );
    } else {
        draw.plot_sql_full(
            PlotOpts::gauge("Power (W)", "power-watts", Unit::Count).with_axis_label("Watts"),
            r#"SELECT timestamp::DOUBLE/1e9 AS t,
                      list_sum([*COLUMNS('^gpu_power_usage/[0-9]+$')]::BIGINT[])::DOUBLE / 1000.0 AS v
               FROM _src"#.to_string(),
        );
    }

    let energy = power.subgroup("Energy");
    energy.describe("Energy consumption rate derived from the accumulating GPU energy counter.");
    energy.plot_sql_full(
        PlotOpts::counter("Energy Rate (W)", "energy-rate", Unit::Count).with_axis_label("Watts"),
        // Counter rate (5m windowed) of the energy accumulator across GPUs,
        // converted from mJ/s to W (= J/s).
        sql::scale_v(
            sql::rate_5m_total("^gpu_energy_consumption(/[^:]+)?$"),
            1000.0,
        ),
    );

    view.group(power);

    let mut thermal = Group::new("Temperature", "temperature");

    let temps = thermal.subgroup("Temperatures");
    temps.describe("Per-device temperatures and the hottest GPU across the system.");
    if multi_gpu {
        temps.plot_sql(
            PlotOpts::gauge("Temperature (Per-GPU)", "temp-per-gpu", Unit::Count)
                .with_axis_label("°C"),
            gauge_by_id_div("^gpu_temperature/[0-9]+$", 1.0),
        );
        temps.plot_sql(
            PlotOpts::gauge("Max (°C)", "temp-max", Unit::Count).with_axis_label("°C"),
            // Use the `:src<i>` aliases so multi-rezolus parquets see
            // each (source, id) value separately and `list_max` gives the
            // global max — a sum-combined `gpu_temperature/0` would land
            // 2× too high.
            r#"SELECT timestamp::DOUBLE/1e9 AS t,
                      list_max([*COLUMNS('^gpu_temperature/[0-9]+:src[0-9]+$')]::BIGINT[])::DOUBLE AS v
               FROM _src"#.to_string(),
        );
    } else {
        temps.plot_sql_full(
            PlotOpts::gauge("Max (°C)", "temp-max", Unit::Count).with_axis_label("°C"),
            r#"SELECT timestamp::DOUBLE/1e9 AS t,
                      list_max([*COLUMNS('^gpu_temperature/[0-9]+:src[0-9]+$')]::BIGINT[])::DOUBLE AS v
               FROM _src"#.to_string(),
        );
    }

    view.group(thermal);

    let mut clocks = Group::new("Clocks", "clocks");

    let freqs = clocks.subgroup("Clock Frequencies");
    freqs.describe("Per-device clock speeds for graphics, memory, compute, and video engines.");
    for (label, id, clock) in &[
        ("Graphics", "clock-graphics", "graphics"),
        ("Memory", "clock-memory", "memory"),
        ("Compute", "clock-compute", "compute"),
        ("Video", "clock-video", "video"),
    ] {
        freqs.plot_sql(
            PlotOpts::gauge(*label, *id, Unit::Frequency),
            gauge_by_id_div(&format!("^gpu_clock/{clock}/[0-9]+$"), 1.0),
        );
    }

    view.group(clocks);

    view
}
