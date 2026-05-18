use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;

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

/// True iff the recording has more than one CPU. Per-core charts are
/// suppressed when this is false because they degenerate to the aggregate.
fn has_multiple_cpus(data: &dyn DashboardData) -> bool {
    ["scheduler_runqueue_wait", "scheduler_context_switch"]
        .iter()
        .any(|m| data.unique_label_values(m, "id") > 1)
}

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let multi_cpu = has_multiple_cpus(data);

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

    let wait = scheduler.subgroup("Runqueue Wait");
    wait.describe(
        "Accumulated runqueue wait time, averaged across CPUs and broken out per-CPU. \
         A value of 1s/s means one task was waiting for the entire interval; values above \
         1s/s mean multiple tasks were queued concurrently — an indicator of scheduler pressure.",
    );
    // Aggregate: sum(irate(scheduler_runqueue_wait[5m])) / cpu_cores —
    // mean per-CPU wait. The SQL twin computes the unpivoted irate sum
    // per timestamp then divides by the cpu_cores gauge column.
    let wait_avg_sql = r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^scheduler_runqueue_wait/[0-9]+$') FROM _src)
                  ON COLUMNS('^scheduler_runqueue_wait/[0-9]+$')
                  INTO NAME col VALUE v
           ),
           rates AS (
              SELECT timestamp,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS rate
              FROM unp
           ),
           summed AS (
              SELECT timestamp, SUM(rate) AS s FROM rates GROUP BY timestamp
           )
           SELECT s.timestamp::DOUBLE/1e9 AS t,
                  s.s / NULLIF(c."cpu_cores", 0) AS v
           FROM summed s
              JOIN _src c ON c.timestamp = s.timestamp"#
        .to_string();
    if multi_cpu {
        wait.plot_promql_with_sql(
            PlotOpts::counter("Wait", "scheduler-runqueue-wait", Unit::Time)
                .with_unit_system("time"),
            "sum(irate(scheduler_runqueue_wait[5m])) / cpu_cores".to_string(),
            wait_avg_sql,
        );
        wait.plot_promql_with_sql(
            PlotOpts::counter(
                "Wait (Per-CPU)",
                "scheduler-runqueue-wait-per-cpu",
                Unit::Time,
            )
            .with_unit_system("time"),
            "sum by (id) (irate(scheduler_runqueue_wait[5m]))".to_string(),
            sql::irate_by_id("^scheduler_runqueue_wait/[0-9]+$", "/([0-9]+)$"),
        );
    } else {
        wait.plot_promql_with_sql_full(
            PlotOpts::counter("Wait", "scheduler-runqueue-wait", Unit::Time)
                .with_unit_system("time"),
            "sum(irate(scheduler_runqueue_wait[5m])) / cpu_cores".to_string(),
            wait_avg_sql,
        );
    }

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
    switches.describe("Involuntary context-switch rate, aggregate and per-core.");
    // Aggregate context-switch SQL: sum across (kind, id) columns then irate.
    let cswitch_total_sql = r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^scheduler_context_switch(/[^:]+)?$')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(s, timestamp) AS v FROM agg"#
        .to_string();
    if multi_cpu {
        switches.plot_promql_with_sql(
            PlotOpts::counter("Context Switch", "cswitch", Unit::Rate),
            "sum(irate(scheduler_context_switch[5m]))".to_string(),
            cswitch_total_sql,
        );
        switches.plot_promql_with_sql(
            PlotOpts::counter("Context Switch (Per-CPU)", "cswitch-per-cpu", Unit::Rate),
            "sum by (id) (irate(scheduler_context_switch[5m]))".to_string(),
            // scheduler_context_switch has variant-suffixed columns
            // (multiple kinds per id); collapse per id, then irate.
            sql::irate_sum_by_id("^scheduler_context_switch(/[^:]+)?$", "/([0-9]+)$"),
        );
    } else {
        switches.plot_promql_with_sql_full(
            PlotOpts::counter("Context Switch", "cswitch", Unit::Rate),
            "sum(irate(scheduler_context_switch[5m]))".to_string(),
            cswitch_total_sql,
        );
    }

    view.group(scheduler);

    view
}
