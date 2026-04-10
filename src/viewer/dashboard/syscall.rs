use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");

    // Total syscall rate
    syscall.plot_promql(
        PlotOpts::counter("Total", "syscall-total", Unit::Rate),
        "sum(irate(syscall[5m]))".to_string(),
    );

    // Total syscall latency percentiles
    syscall.plot_promql(
        PlotOpts::histogram_latency("Total", "syscall-total-latency"),
        "syscall_latency".to_string(),
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
            PlotOpts::counter(*op, format!("syscall-{op}"), Unit::Rate),
            format!("sum(irate(syscall{{op=\"{op_lower}\"}}[5m]))"),
        );

        // Latency percentiles for this operation
        syscall.plot_promql(
            PlotOpts::histogram_latency(*op, format!("syscall-{op}-latency")),
            format!("syscall_latency{{op=\"{op_lower}\"}}"),
        );
    }

    view.group(syscall);

    view
}
