use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Softirq
     */

    let mut softirq = Group::new("Softirq", "softirq");

    // Total softirq rate
    softirq.plot_promql(
        PlotOpts::line("Rate", "softirq-total-rate", Unit::Rate),
        "sum(irate(softirq[5m]))".to_string(),
    );

    // Per-CPU softirq rate heatmap
    softirq.plot_promql(
        PlotOpts::heatmap("Rate", "softirq-total-rate-heatmap", Unit::Rate),
        "sum by (id) (irate(softirq[5m]))".to_string(),
    );

    // Average CPU % spent in softirq
    softirq.plot_promql(
        PlotOpts::line("CPU %", "softirq-total-time", Unit::Percentage),
        "irate(softirq_time[5m]) / cpu_cores / 1000000000".to_string(),
    );

    // Per-CPU % spent in softirq heatmap
    softirq.plot_promql(
        PlotOpts::heatmap("CPU %", "softirq-total-time-heatmap", Unit::Percentage),
        "sum by (id) (irate(softirq_time[5m])) / 1000000000".to_string(),
    );

    view.group(softirq);

    /*
     * Detailed
     */

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
        let mut group = Group::new(label, format!("softirq-{kind}"));

        // Rate for this softirq kind
        group.plot_promql(
            PlotOpts::line("Rate", format!("softirq-{kind}-rate"), Unit::Rate),
            format!("sum(irate(softirq{{kind=\"{kind}\"}}[5m]))"),
        );

        // Per-CPU rate heatmap for this softirq kind
        group.plot_promql(
            PlotOpts::heatmap("Rate", format!("softirq-{kind}-rate-heatmap"), Unit::Rate),
            format!("sum by (id) (irate(softirq{{kind=\"{kind}\"}}[5m]))"),
        );

        // Average CPU % for this softirq kind
        group.plot_promql(
            PlotOpts::line("CPU %", format!("softirq-{kind}-time"), Unit::Percentage),
            format!("irate(softirq_time{{kind=\"{kind}\"}}[5m]) / cpu_cores / 1000000000"),
        );

        // Per-CPU % heatmap for this softirq kind
        group.plot_promql(
            PlotOpts::heatmap(
                "CPU %",
                format!("softirq-{kind}-time-heatmap"),
                Unit::Percentage,
            ),
            format!("sum by (id) (irate(softirq_time{{kind=\"{kind}\"}}[5m])) / 1000000000"),
        );

        view.group(group);
    }

    view
}
