use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");

    syscall.plot(
        PlotOpts::line("Total", "syscall-total", Unit::Rate),
        data.counters("syscall", ()).map(|v| v.rate().sum()),
    );

    syscall.scatter(
        PlotOpts::scatter("Total", "syscall-total-latency", Unit::Time).with_log_scale(true),
        data.percentiles("syscall_latency", (), PERCENTILES),
    );

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
        syscall.plot(
            PlotOpts::line(*op, format!("syscall-{op}"), Unit::Rate),
            data.counters("syscall", [("op", op.to_lowercase())])
                .map(|v| v.rate().sum()),
        );

        syscall.scatter(
            PlotOpts::scatter(*op, format!("syscall-{op}-latency"), Unit::Time),
            data.percentiles("syscall_latency", [("op", op.to_lowercase())], PERCENTILES),
        );
    }

    view.group(syscall);

    view
}
