use crate::data::DashboardData;
use crate::plot::*;

/// Adds the standard cgroup metric plots for either aggregate or individual mode.
///
/// In aggregate mode, metrics are summed across non-selected cgroups.
/// In individual mode, metrics are broken down by cgroup name.
fn add_cgroup_metrics(group: &mut Group, individual: bool) {
    let prefix = if individual {
        "individual"
    } else {
        "aggregate"
    };

    let filter = if individual {
        "name=~\"__SELECTED_CGROUPS__\""
    } else {
        "name!~\"__SELECTED_CGROUPS__\""
    };

    let rate = |metric: &str| {
        if individual {
            format!("sum by (name) (irate({metric}{{{filter}}}[5m]))")
        } else {
            format!("sum(irate({metric}{{{filter}}}[5m]))")
        }
    };

    // CPU Total Cores
    group.plot_promql(
        PlotOpts::counter(
            "Total CPU Cores",
            format!("{prefix}-total-cores"),
            Unit::Count,
        ),
        format!("{} / 1000000000", rate("cgroup_cpu_usage")),
    );

    // CPU User Cores
    group.plot_promql(
        PlotOpts::counter("User CPU Cores", format!("{prefix}-user-cores"), Unit::Count),
        if individual {
            format!("sum by (name) (irate(cgroup_cpu_usage{{state=\"user\",{filter}}}[5m])) / 1000000000")
        } else {
            format!("sum(irate(cgroup_cpu_usage{{state=\"user\",{filter}}}[5m])) / 1000000000")
        },
    );

    // CPU System Cores
    group.plot_promql(
        PlotOpts::counter(
            "System CPU Cores",
            format!("{prefix}-system-cores"),
            Unit::Count,
        ),
        if individual {
            format!("sum by (name) (irate(cgroup_cpu_usage{{state=\"system\",{filter}}}[5m])) / 1000000000")
        } else {
            format!("sum(irate(cgroup_cpu_usage{{state=\"system\",{filter}}}[5m])) / 1000000000")
        },
    );

    // CPU Migrations
    group.plot_promql(
        PlotOpts::counter(
            "CPU Migrations",
            format!("{prefix}-cpu-migrations"),
            Unit::Rate,
        ),
        rate("cgroup_cpu_migrations"),
    );

    // CPU Throttled Time
    group.plot_promql(
        PlotOpts::counter(
            "CPU Throttled Time",
            format!("{prefix}-cpu-throttled-time"),
            Unit::Time,
        ),
        rate("cgroup_cpu_throttled_time"),
    );

    // IPC
    group.plot_promql(
        PlotOpts::counter("IPC", format!("{prefix}-ipc"), Unit::Count),
        if individual {
            format!("sum by (name) (irate(cgroup_cpu_instructions{{{filter}}}[5m])) / sum by (name) (irate(cgroup_cpu_cycles{{{filter}}}[5m]))")
        } else {
            format!("sum(irate(cgroup_cpu_instructions{{{filter}}}[5m])) / sum(irate(cgroup_cpu_cycles{{{filter}}}[5m]))")
        },
    );

    // TLB Flushes
    group.plot_promql(
        PlotOpts::counter("TLB Flushes", format!("{prefix}-tlb-flush"), Unit::Rate),
        rate("cgroup_cpu_tlb_flush"),
    );

    // Syscalls
    group.plot_promql(
        PlotOpts::counter("Syscalls", format!("{prefix}-syscall"), Unit::Rate),
        rate("cgroup_syscall"),
    );

    // Per-syscall operation breakdown
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
        group.plot_promql(
            PlotOpts::counter(
                format!("Syscall {op}"),
                format!("{prefix}-syscall-{op_lower}"),
                Unit::Rate,
            ),
            if individual {
                format!("sum by (name) (irate(cgroup_syscall{{op=\"{op_lower}\",{filter}}}[5m]))")
            } else {
                format!("sum(irate(cgroup_syscall{{op=\"{op_lower}\",{filter}}}[5m]))")
            },
        );
    }
}

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections.clone());

    // Add metadata for cgroup selection UI
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

    // Aggregate (Left Side) - Sum of non-selected cgroups
    let mut aggregate = Group::new("Aggregate Cgroups", "aggregate");
    aggregate
        .metadata
        .insert("side".to_string(), serde_json::json!("left"));
    add_cgroup_metrics(&mut aggregate, false);
    view.group(aggregate);

    // Individual (Right Side) - Selected cgroups with one line per cgroup
    let mut individual = Group::new("Individual Cgroups", "individual");
    individual
        .metadata
        .insert("side".to_string(), serde_json::json!("right"));
    add_cgroup_metrics(&mut individual, true);
    view.group(individual);

    view
}
