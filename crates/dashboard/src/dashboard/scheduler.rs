use crate::data::DashboardData;
use crate::plot::*;

/// SQL fragment producing one Prometheus matrix series per default percentile
/// for a histogram metric. The bare metric `M` is the parquet column
/// `M:buckets` (UBIGINT[]). We pre-compute the per-row delta in a CTE so the
/// LAG-based windowing happens once per row regardless of how many quantiles
/// we cross-join with.
fn hist_percentile_series_sql(metric: &str) -> String {
    format!(
        r#"WITH d AS (
              SELECT timestamp,
                     h2_delta("{metric}:buckets",
                              LAG("{metric}:buckets") OVER (ORDER BY timestamp)) AS d
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  q::VARCHAR AS quantile,
                  h2_quantile(d, q)::DOUBLE AS v
           FROM d, (VALUES (0.5), (0.9), (0.99), (0.999), (0.9999)) qs(q)
           WHERE d IS NOT NULL"#
    )
}

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    let queueing = scheduler.subgroup("Runqueue Latency");
    queueing.describe("How long tasks waited on the runqueue before getting CPU time.");
    queueing.plot_promql_with_sql_full(
        PlotOpts::histogram_latency("Runqueue Latency", "scheduler-runqueue-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "scheduler_runqueue_latency".to_string(),
        hist_percentile_series_sql("scheduler_runqueue_latency"),
    );

    let timing = scheduler.subgroup("Task Timing");
    timing.describe("Time tasks spent off-CPU (blocked, waiting) and on-CPU (running).");
    timing.plot_promql_with_sql(
        PlotOpts::histogram_latency("Off CPU Time", "off-cpu-time")
            .with_axis_label("Time")
            .with_unit_system("time"),
        "scheduler_offcpu".to_string(),
        hist_percentile_series_sql("scheduler_offcpu"),
    );
    timing.plot_promql_with_sql(
        PlotOpts::histogram_latency("Running Time", "running-time")
            .with_axis_label("Time")
            .with_unit_system("time"),
        "scheduler_running".to_string(),
        hist_percentile_series_sql("scheduler_running"),
    );

    let switches = scheduler.subgroup("Context Switches");
    switches.describe("Overall context-switch rate across all cores.");
    switches.plot_promql_with_sql_full(
        PlotOpts::counter("Context Switch", "cswitch", Unit::Rate),
        "sum(irate(scheduler_context_switch[5m]))".to_string(),
        // scheduler_context_switch is split per (kind, id) — sum across all
        // matching columns first, then per-second rate via irate_1s.
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^scheduler_context_switch(/[^:]+)?$')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(s, timestamp) AS v FROM agg"#.to_string(),
    );

    view.group(scheduler);

    view
}
