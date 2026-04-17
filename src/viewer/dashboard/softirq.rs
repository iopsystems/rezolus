use super::*;

/// Adds the standard 4-plot pattern for a softirq kind: rate, rate heatmap,
/// CPU %, and CPU % heatmap.
fn add_softirq_group(view: &mut View, label: &str, kind: &str) {
    let mut group = Group::new(label, format!("softirq-{kind}"));

    group.plot_promql(
        PlotOpts::counter("Rate", format!("softirq-{kind}-rate"), Unit::Rate),
        format!("sum(irate(softirq{{kind=\"{kind}\"}}[5m]))"),
    );

    group.plot_promql(
        PlotOpts::counter("Rate", format!("softirq-{kind}-rate-heatmap"), Unit::Rate),
        format!("sum by (id) (irate(softirq{{kind=\"{kind}\"}}[5m]))"),
    );

    group.plot_promql(
        PlotOpts::counter("CPU %", format!("softirq-{kind}-time"), Unit::Percentage)
            .percentage_range(),
        format!("sum(irate(softirq_time{{kind=\"{kind}\"}}[5m])) / sum(cpu_cores) / 1000000000"),
    );

    group.plot_promql(
        PlotOpts::counter(
            "CPU %",
            format!("softirq-{kind}-time-heatmap"),
            Unit::Percentage,
        )
        .percentage_range(),
        format!("sum by (id) (irate(softirq_time{{kind=\"{kind}\"}}[5m])) / 1000000000"),
    );

    view.group(group);
}

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    // Total softirq (uses the same pattern but without a kind filter)
    let mut softirq = Group::new("Softirq", "softirq");

    softirq.plot_promql(
        PlotOpts::counter("Rate", "softirq-total-rate", Unit::Rate),
        "sum(irate(softirq[5m]))".to_string(),
    );

    softirq.plot_promql(
        PlotOpts::counter("Rate", "softirq-total-rate-heatmap", Unit::Rate),
        "sum by (id) (irate(softirq[5m]))".to_string(),
    );

    softirq.plot_promql(
        PlotOpts::counter("CPU %", "softirq-total-time", Unit::Percentage).percentage_range(),
        "sum(irate(softirq_time[5m])) / sum(cpu_cores) / 1000000000".to_string(),
    );

    softirq.plot_promql(
        PlotOpts::counter("CPU %", "softirq-total-time-heatmap", Unit::Percentage)
            .percentage_range(),
        "sum by (id) (irate(softirq_time[5m])) / 1000000000".to_string(),
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
