use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");

    // Total syscall rate
    syscall.plot_promql(
        PlotOpts::line("Total", "syscall-total", Unit::Rate),
        "sum(irate(syscall[5m]))".to_string(),
    );

    // Total syscall latency percentiles
    syscall.plot_promql(
        PlotOpts::scatter("Total", "syscall-total-latency", Unit::Time).with_log_scale(true),
        "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], syscall_latency)".to_string(),
    );

    // Per-operation syscall metrics
    for op in &[
        "Read",
        "Write",
        "Poll",
        "Socket",
        "Lock",
        "Time",
        "Sleep",
        "Yield",
        "Filesystem",
        "Memory",
        "Process",
        "Query",
        "IPC",
        "Timer",
        "Event",
        "Other",
    ] {
        let op_lower = op.to_lowercase();

        // Rate for this operation
        syscall.plot_promql(
            PlotOpts::line(*op, format!("syscall-{op}"), Unit::Rate),
            format!("sum(irate(syscall{{op=\"{op_lower}\"}}[5m]))"),
        );

        // Latency percentiles for this operation
        syscall.plot_promql(
            PlotOpts::scatter(*op, format!("syscall-{op}-latency"), Unit::Time)
                .with_log_scale(true),
            format!("histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], syscall_latency{{op=\"{op_lower}\"}})"),
        );
    }

    view.group(syscall);

    view
}
