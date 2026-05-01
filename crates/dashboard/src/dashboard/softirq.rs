use crate::Tsdb;
use crate::plot::*;

/// Adds the standard 4-plot pattern for a softirq kind in two subgroups:
/// "Rate" (rate + per-CPU rate) and "CPU Time" (CPU % + per-CPU CPU %).
fn add_softirq_group(view: &mut View, label: &str, kind: &str) {
    let mut group = Group::new(label, format!("softirq-{kind}"));

    let rate = group.subgroup("Rate");
    rate.describe("Softirqs handled per second, aggregate and per-CPU.");
    rate.plot_promql(
        PlotOpts::counter("Rate", format!("softirq-{kind}-rate"), Unit::Rate),
        format!("sum by (node) (irate(softirq{{kind=\"{kind}\"}}[5m]))"),
    );
    rate.plot_promql(
        PlotOpts::counter(
            "Rate (per-CPU)",
            format!("softirq-{kind}-rate-heatmap"),
            Unit::Rate,
        ),
        format!("sum by (id, node) (irate(softirq{{kind=\"{kind}\"}}[5m]))"),
    );

    let time = group.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing this softirq kind, aggregate and per-CPU.");
    time.plot_promql(
        PlotOpts::counter("CPU %", format!("softirq-{kind}-time"), Unit::Percentage)
            .percentage_range(),
        format!("sum by (node) (irate(softirq_time{{kind=\"{kind}\"}}[5m])) / cpu_cores / 1000000000"),
    );
    time.plot_promql(
        PlotOpts::counter(
            "CPU % (per-CPU)",
            format!("softirq-{kind}-time-heatmap"),
            Unit::Percentage,
        )
        .percentage_range(),
        format!("sum by (id, node) (irate(softirq_time{{kind=\"{kind}\"}}[5m])) / 1000000000"),
    );

    view.group(group);
}

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    // Total softirq (uses the same pattern but without a kind filter)
    let mut softirq = Group::new("Softirq", "softirq");

    let rate = softirq.subgroup("Rate");
    rate.describe("Softirqs handled per second, aggregate and per-CPU.");
    rate.plot_promql(
        PlotOpts::counter("Rate", "softirq-total-rate", Unit::Rate),
        "sum by (node) (irate(softirq[5m]))".to_string(),
    );
    rate.plot_promql(
        PlotOpts::counter("Rate (per-CPU)", "softirq-total-rate-heatmap", Unit::Rate),
        "sum by (id, node) (irate(softirq[5m]))".to_string(),
    );

    let time = softirq.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing softirqs, aggregate and per-CPU.");
    time.plot_promql(
        PlotOpts::counter("CPU %", "softirq-total-time", Unit::Percentage).percentage_range(),
        "sum by (node) (irate(softirq_time[5m])) / cpu_cores / 1000000000".to_string(),
    );
    time.plot_promql(
        PlotOpts::counter(
            "CPU % (per-CPU)",
            "softirq-total-time-heatmap",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id, node) (irate(softirq_time[5m])) / 1000000000".to_string(),
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
