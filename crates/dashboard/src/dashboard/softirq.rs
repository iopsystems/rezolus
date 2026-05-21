use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;

/// Adds the standard 4-plot pattern for a softirq kind in two subgroups:
/// "Rate" (rate + per-CPU rate) and "CPU Time" (CPU % + per-CPU CPU %).
fn add_softirq_group(view: &mut View, label: &str, kind: &str) {
    let mut group = Group::new(label, format!("softirq-{kind}"));

    let rate = group.subgroup("Rate");
    rate.describe("Softirqs handled per second, aggregate and per-CPU.");
    rate.plot_sql(
        PlotOpts::counter("Rate", format!("softirq-{kind}-rate"), Unit::Rate),
        sql::irate_total(&format!("^softirq/{kind}/[0-9]+$")),
    );
    rate.plot_sql(
        PlotOpts::counter(
            "Rate (per-CPU)",
            format!("softirq-{kind}-rate-heatmap"),
            Unit::Rate,
        ),
        sql::irate_by_id(&format!("^softirq/{kind}/[0-9]+$"), "/([0-9]+)$"),
    );

    let time = group.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing this softirq kind, aggregate and per-CPU.");
    time.plot_sql(
        PlotOpts::counter("CPU %", format!("softirq-{kind}-time"), Unit::Percentage)
            .percentage_range(),
        sql::cpu_pct_total(&format!("^softirq_time/{kind}/[0-9]+$")),
    );
    time.plot_sql(
        PlotOpts::counter(
            "CPU % (per-CPU)",
            format!("softirq-{kind}-time-heatmap"),
            Unit::Percentage,
        )
        .percentage_range(),
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
    rate.plot_sql(
        PlotOpts::counter("Rate", "softirq-total-rate", Unit::Rate),
        sql::irate_total("^softirq/[a-z_]+/[0-9]+$"),
    );
    rate.plot_sql(
        PlotOpts::counter("Rate (per-CPU)", "softirq-total-rate-heatmap", Unit::Rate),
        sql::irate_sum_by_id("^softirq/[a-z_]+/[0-9]+$", "/([0-9]+)$"),
    );

    let time = softirq.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing softirqs, aggregate and per-CPU.");
    time.plot_sql(
        PlotOpts::counter("CPU %", "softirq-total-time", Unit::Percentage).percentage_range(),
        sql::cpu_pct_total("^softirq_time/[a-z_]+/[0-9]+$"),
    );
    time.plot_sql(
        PlotOpts::counter(
            "CPU % (per-CPU)",
            "softirq-total-time-heatmap",
            Unit::Percentage,
        )
        .percentage_range(),
        sql::scale_v(
            sql::irate_sum_by_id("^softirq_time/[a-z_]+/[0-9]+$", "/([0-9]+)$"),
            1e9,
        ),
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
