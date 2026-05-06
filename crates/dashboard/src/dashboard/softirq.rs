use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;

/// Adds the standard 4-plot pattern for a softirq kind in two subgroups:
/// "Rate" (rate + per-CPU rate) and "CPU Time" (CPU % + per-CPU CPU %).
fn add_softirq_group(view: &mut View, label: &str, kind: &str) {
    let mut group = Group::new(label, format!("softirq-{kind}"));

    let rate = group.subgroup("Rate");
    rate.describe("Softirqs handled per second, aggregate and per-CPU.");
    rate.plot_promql_with_sql(
        PlotOpts::counter("Rate", format!("softirq-{kind}-rate"), Unit::Rate),
        format!("sum(irate(softirq{{kind=\"{kind}\"}}[5m]))"),
        sql::irate_total(&format!("^softirq/{kind}/[0-9]+$")),
    );
    rate.plot_promql_with_sql(
        PlotOpts::counter(
            "Rate (per-CPU)",
            format!("softirq-{kind}-rate-heatmap"),
            Unit::Rate,
        ),
        format!("sum by (id) (irate(softirq{{kind=\"{kind}\"}}[5m]))"),
        sql::irate_by_id(&format!("^softirq/{kind}/[0-9]+$"), "/([0-9]+)$"),
    );

    let time = group.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing this softirq kind, aggregate and per-CPU.");
    time.plot_promql_with_sql(
        PlotOpts::counter("CPU %", format!("softirq-{kind}-time"), Unit::Percentage)
            .percentage_range(),
        format!("sum(irate(softirq_time{{kind=\"{kind}\"}}[5m])) / cpu_cores / 1000000000"),
        sql::cpu_pct_total(&format!("^softirq_time/{kind}/[0-9]+$")),
    );
    time.plot_promql_with_sql(
        PlotOpts::counter(
            "CPU % (per-CPU)",
            format!("softirq-{kind}-time-heatmap"),
            Unit::Percentage,
        )
        .percentage_range(),
        format!("sum by (id) (irate(softirq_time{{kind=\"{kind}\"}}[5m])) / 1000000000"),
        sql::cpu_pct_by_id(&format!("^softirq_time/{kind}/[0-9]+$"), "/([0-9]+)$"),
    );

    view.group(group);
}

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    // Total softirq (uses the same pattern but without a kind filter)
    let mut softirq = Group::new("Softirq", "softirq");

    let rate = softirq.subgroup("Rate");
    rate.describe("Softirqs handled per second, aggregate and per-CPU.");
    rate.plot_promql_with_sql(
        PlotOpts::counter("Rate", "softirq-total-rate", Unit::Rate),
        "sum(irate(softirq[5m]))".to_string(),
        sql::irate_total("^softirq/[a-z_]+/[0-9]+$"),
    );
    rate.plot_promql_with_sql(
        PlotOpts::counter("Rate (per-CPU)", "softirq-total-rate-heatmap", Unit::Rate),
        "sum by (id) (irate(softirq[5m]))".to_string(),
        // Aggregate across kinds, per id. Using UNPIVOT then SUM by id +
        // timestamp lets us collapse the kind dimension while keeping id.
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^softirq/[a-z_]+/[0-9]+$') FROM _src)
                  ON COLUMNS('^softirq/[a-z_]+/[0-9]+$')
                  INTO NAME col VALUE v
           ),
           by_id AS (
              SELECT timestamp,
                     regexp_extract(col, '/([0-9]+)$', 1) AS id,
                     SUM(v) AS s
              FROM unp
              GROUP BY timestamp, id
           )
           SELECT timestamp::DOUBLE/1e9 AS t, id,
                  irate_lag(
                      s,
                      LAG(s) OVER (PARTITION BY id ORDER BY timestamp),
                      timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)
                  ) AS v
           FROM by_id"#.to_string(),
    );

    let time = softirq.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing softirqs, aggregate and per-CPU.");
    time.plot_promql_with_sql(
        PlotOpts::counter("CPU %", "softirq-total-time", Unit::Percentage).percentage_range(),
        "sum(irate(softirq_time[5m])) / cpu_cores / 1000000000".to_string(),
        sql::cpu_pct_total("^softirq_time/[a-z_]+/[0-9]+$"),
    );
    time.plot_promql_with_sql(
        PlotOpts::counter(
            "CPU % (per-CPU)",
            "softirq-total-time-heatmap",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id) (irate(softirq_time[5m])) / 1000000000".to_string(),
        // Same aggregate-by-id-then-rate pattern as the total rate plot.
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^softirq_time/[a-z_]+/[0-9]+$') FROM _src)
                  ON COLUMNS('^softirq_time/[a-z_]+/[0-9]+$')
                  INTO NAME col VALUE v
           ),
           by_id AS (
              SELECT timestamp,
                     regexp_extract(col, '/([0-9]+)$', 1) AS id,
                     SUM(v) AS s
              FROM unp
              GROUP BY timestamp, id
           )
           SELECT timestamp::DOUBLE/1e9 AS t, id,
                  irate_lag(
                      s,
                      LAG(s) OVER (PARTITION BY id ORDER BY timestamp),
                      timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)
                  ) / 1e9 AS v
           FROM by_id"#.to_string(),
    );

    view.group(softirq);

    // Per-kind breakdowns
    for (label, kind) in [
        ("Hardware Interrupts", "hi"),
        ("IRQ Poll", "irq_poll"),
        ("Network Transmit", "net_tx"),
        ("Network Receive", "net_rx"),
        ("RCU", "rcu"),
        ("Sched", "sched"),
        ("Tasklet", "tasklet"),
        ("Timer", "timer"),
        ("HR Timer", "hrtimer"),
        ("Block", "block"),
    ] {
        add_softirq_group(&mut view, label, kind);
    }

    view
}
