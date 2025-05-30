use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Softirq
     */

    let mut softirq = Group::new("Softirq", "softirq");

    softirq.plot(
        PlotOpts::line("Rate", "softirq-total-rate", Unit::Rate),
        data.counters("softirq", ()).map(|v| v.rate().sum()),
    );

    softirq.heatmap_echarts(
        PlotOpts::heatmap("Rate", "softirq-total-rate-heatmap", Unit::Rate),
        data.cpu_heatmap("softirq", ()),
    );

    softirq.plot(
        PlotOpts::line("CPU %", "softirq-total-time", Unit::Percentage),
        data.cpu_avg("softirq_time", ()).map(|v| v / 1000000000.0),
    );

    softirq.heatmap_echarts(
        PlotOpts::heatmap("CPU %", "softirq-total-time-heatmap", Unit::Percentage),
        data.cpu_heatmap("softirq_time", ())
            .map(|v| v / 1000000000.0),
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

        group.plot(
            PlotOpts::line("Rate", format!("softirq-{kind}-rate"), Unit::Rate),
            data.counters("softirq", [("kind", kind)])
                .map(|v| v.rate().sum()),
        );

        group.heatmap_echarts(
            PlotOpts::heatmap("Rate", format!("softirq-{kind}-rate-heatmap"), Unit::Rate),
            data.cpu_heatmap("softirq", [("kind", kind)]),
        );

        group.plot(
            PlotOpts::line("CPU %", format!("softirq-{kind}-time"), Unit::Percentage),
            data.cpu_avg("softirq_time", [("kind", kind)])
                .map(|v| v / 1000000000.0),
        );

        group.heatmap_echarts(
            PlotOpts::heatmap(
                "CPU %",
                format!("softirq-{kind}-time-heatmap"),
                Unit::Percentage,
            ),
            data.cpu_heatmap("softirq_time", [("kind", kind)])
                .map(|v| v / 1000000000.0),
        );

        view.group(group);
    }

    view
}
