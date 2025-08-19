use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections.clone());

    // Add metadata for cgroup selection UI
    // This will be used by the frontend to build the selection interface
    view.metadata.insert(
        "cgroup_selector".to_string(),
        serde_json::json!({
            "enabled": true,
            "metrics": [
                "cgroup_cpu_usage",
                "cgroup_cpu_migrations",
                "cgroup_cpu_throttled_time",
                "cgroup_cpu_throttled",
                "cgroup_cpu_cycles",
                "cgroup_cpu_instructions",
                "cgroup_cpu_tlb_flush",
                "cgroup_syscall"
            ]
        }),
    );

    /*
     * Aggregate (Left Side) - Sum of non-selected cgroups
     */

    let mut aggregate = Group::new("Aggregate Cgroups", "aggregate");
    aggregate
        .metadata
        .insert("side".to_string(), serde_json::json!("left"));

    // CPU Total Cores - aggregate of non-selected
    aggregate.plot_promql(
        PlotOpts::line("Total CPU Cores", "aggregate-total-cores", Unit::Count),
        "sum(irate(cgroup_cpu_usage{name!~\"__SELECTED_CGROUPS__\"}[5m])) / 1000000000".to_string(),
    );

    // CPU User Cores - aggregate of non-selected
    aggregate.plot_promql(
        PlotOpts::line("User CPU Cores", "aggregate-user-cores", Unit::Count),
        "sum(irate(cgroup_cpu_usage{state=\"user\",name!~\"__SELECTED_CGROUPS__\"}[5m])) / 1000000000".to_string(),
    );

    // CPU System Cores - aggregate of non-selected
    aggregate.plot_promql(
        PlotOpts::line("System CPU Cores", "aggregate-system-cores", Unit::Count),
        "sum(irate(cgroup_cpu_usage{state=\"system\",name!~\"__SELECTED_CGROUPS__\"}[5m])) / 1000000000".to_string(),
    );

    // CPU Migrations - aggregate of non-selected
    aggregate.plot_promql(
        PlotOpts::line("CPU Migrations", "aggregate-cpu-migrations", Unit::Rate),
        "sum(irate(cgroup_cpu_migrations{name!~\"__SELECTED_CGROUPS__\"}[5m]))".to_string(),
    );

    // CPU Throttled Time - aggregate of non-selected
    aggregate.plot_promql(
        PlotOpts::line(
            "CPU Throttled Time",
            "aggregate-cpu-throttled-time",
            Unit::Time,
        ),
        "sum(irate(cgroup_cpu_throttled_time{name!~\"__SELECTED_CGROUPS__\"}[5m]))".to_string(),
    );

    // IPC - aggregate of non-selected
    aggregate.plot_promql(
        PlotOpts::line("IPC", "aggregate-ipc", Unit::Count),
        "sum(irate(cgroup_cpu_instructions{name!~\"__SELECTED_CGROUPS__\"}[5m])) / sum(irate(cgroup_cpu_cycles{name!~\"__SELECTED_CGROUPS__\"}[5m]))".to_string(),
    );

    // TLB Flushes - aggregate of non-selected
    aggregate.plot_promql(
        PlotOpts::line("TLB Flushes", "aggregate-tlb-flush", Unit::Rate),
        "sum(irate(cgroup_cpu_tlb_flush{name!~\"__SELECTED_CGROUPS__\"}[5m]))".to_string(),
    );

    // Syscalls - aggregate of non-selected
    aggregate.plot_promql(
        PlotOpts::line("Syscalls", "aggregate-syscall", Unit::Rate),
        "sum(irate(cgroup_syscall{name!~\"__SELECTED_CGROUPS__\"}[5m]))".to_string(),
    );

    // Per-syscall operation breakdown for aggregate (non-selected) cgroups
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
        aggregate.plot_promql(
            PlotOpts::line(
                format!("Syscall {op}"),
                format!("aggregate-syscall-{}", op.to_lowercase()),
                Unit::Rate,
            ),
            format!(
                "sum(irate(cgroup_syscall{{op=\"{}\",name!~\"__SELECTED_CGROUPS__\"}}[5m]))",
                op.to_lowercase()
            ),
        );
    }

    view.group(aggregate);

    /*
     * Individual (Right Side) - Selected cgroups with one line per cgroup
     */

    let mut individual = Group::new("Individual Cgroups", "individual");
    individual
        .metadata
        .insert("side".to_string(), serde_json::json!("right"));

    // CPU Total Cores - per selected cgroup
    individual.plot_promql(
        PlotOpts::multi("Total CPU Cores", "individual-total-cores", Unit::Count),
        "sum by (name) (irate(cgroup_cpu_usage{name=~\"__SELECTED_CGROUPS__\"}[5m])) / 1000000000"
            .to_string(),
    );

    // CPU User Cores - per selected cgroup
    individual.plot_promql(
        PlotOpts::multi("User CPU Cores", "individual-user-cores", Unit::Count),
        "sum by (name) (irate(cgroup_cpu_usage{state=\"user\",name=~\"__SELECTED_CGROUPS__\"}[5m])) / 1000000000".to_string(),
    );

    // CPU System Cores - per selected cgroup
    individual.plot_promql(
        PlotOpts::multi("System CPU Cores", "individual-system-cores", Unit::Count),
        "sum by (name) (irate(cgroup_cpu_usage{state=\"system\",name=~\"__SELECTED_CGROUPS__\"}[5m])) / 1000000000".to_string(),
    );

    // CPU Migrations - per selected cgroup
    individual.plot_promql(
        PlotOpts::multi("CPU Migrations", "individual-cpu-migrations", Unit::Rate),
        "sum by (name) (irate(cgroup_cpu_migrations{name=~\"__SELECTED_CGROUPS__\"}[5m]))"
            .to_string(),
    );

    // CPU Throttled Time - per selected cgroup
    individual.plot_promql(
        PlotOpts::multi(
            "CPU Throttled Time",
            "individual-cpu-throttled-time",
            Unit::Time,
        ),
        "sum by (name) (irate(cgroup_cpu_throttled_time{name=~\"__SELECTED_CGROUPS__\"}[5m]))"
            .to_string(),
    );

    // IPC - per selected cgroup
    individual.plot_promql(
        PlotOpts::multi("IPC", "individual-ipc", Unit::Count),
        "sum by (name) (irate(cgroup_cpu_instructions{name=~\"__SELECTED_CGROUPS__\"}[5m])) / sum by (name) (irate(cgroup_cpu_cycles{name=~\"__SELECTED_CGROUPS__\"}[5m]))".to_string(),
    );

    // TLB Flushes - per selected cgroup
    individual.plot_promql(
        PlotOpts::multi("TLB Flushes", "individual-tlb-flush", Unit::Rate),
        "sum by (name) (irate(cgroup_cpu_tlb_flush{name=~\"__SELECTED_CGROUPS__\"}[5m]))"
            .to_string(),
    );

    // Syscalls - per selected cgroup
    individual.plot_promql(
        PlotOpts::multi("Syscalls", "individual-syscall", Unit::Rate),
        "sum by (name) (irate(cgroup_syscall{name=~\"__SELECTED_CGROUPS__\"}[5m]))".to_string(),
    );

    // Per-syscall operation breakdown for selected cgroups
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
        individual.plot_promql(
            PlotOpts::multi(format!("Syscall {op}"), format!("individual-syscall-{}", op.to_lowercase()), Unit::Rate),
            format!("sum by (name) (irate(cgroup_syscall{{op=\"{}\",name=~\"__SELECTED_CGROUPS__\"}}[5m]))", op.to_lowercase()),
        );
    }

    view.group(individual);

    view
}
