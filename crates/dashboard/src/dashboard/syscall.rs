use crate::Tsdb;
use crate::plot::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");
    syscall
        .metadata
        .insert("no_collapse".to_string(), serde_json::json!(true));

    let overall = syscall.subgroup("Overall");
    overall.describe("Aggregate syscall rate and latency across all operation categories.");
    overall.plot_promql(
        PlotOpts::counter("Overall Rate", "syscall-total", Unit::Rate),
        "sum(irate(syscall[5m]))".to_string(),
    );
    overall.plot_promql(
        PlotOpts::histogram_latency("Overall Latency", "syscall-total-latency"),
        "syscall_latency".to_string(),
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
        let op_lower = op.to_lowercase();
        let sg = syscall.subgroup(*op);
        sg.plot_promql(
            PlotOpts::counter(format!("{op} Rate"), format!("syscall-{op}"), Unit::Rate),
            format!("sum(irate(syscall{{op=\"{op_lower}\"}}[5m]))"),
        );
        sg.plot_promql(
            PlotOpts::histogram_latency(format!("{op} Latency"), format!("syscall-{op}-latency")),
            format!("syscall_latency{{op=\"{op_lower}\"}}"),
        );
    }

    view.group(syscall);

    view
}
