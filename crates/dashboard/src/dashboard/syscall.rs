use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut syscall = Group::new("Syscall", "syscall");
    syscall
        .metadata
        .insert("no_collapse".to_string(), serde_json::json!(true));

    let overall = syscall.subgroup("Overall");
    overall.describe("Aggregate syscall rate and latency across all operation categories.");
    overall.plot_promql_with_sql(
        PlotOpts::counter("Overall Rate", "syscall-total", Unit::Rate),
        "sum(irate(syscall[5m]))".to_string(),
        sql::irate_total("^syscall/[a-z_]+(/[0-9]+)?$"),
    );
    overall.plot_promql_with_sql(
        PlotOpts::histogram_latency("Overall Latency", "syscall-total-latency"),
        "syscall_latency".to_string(),
        // Bare `syscall_latency` aggregates across all op labels. The
        // physical schema only has per-op `syscall_latency/<op>:buckets`
        // columns — combine them via h2_combine before quantile fan-out.
        sql::hist_percentile_series_combined("^syscall_latency/[a-z]+:buckets$"),
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
        sg.plot_promql_with_sql(
            PlotOpts::counter(format!("{op} Rate"), format!("syscall-{op}"), Unit::Rate),
            format!("sum(irate(syscall{{op=\"{op_lower}\"}}[5m]))"),
            sql::irate_total(&format!("^syscall/{op_lower}(/[0-9]+)?$")),
        );
        sg.plot_promql_with_sql(
            PlotOpts::histogram_latency(format!("{op} Latency"), format!("syscall-{op}-latency")),
            format!("syscall_latency{{op=\"{op_lower}\"}}"),
            sql::hist_percentile_series(&format!("syscall_latency/{op_lower}")),
        );
    }

    view.group(syscall);

    view
}
