use crate::data::DashboardData;
use crate::plot::*;
use crate::sql::{self, CgroupSide};

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

    let side = if individual {
        CgroupSide::Individual
    } else {
        CgroupSide::Aggregate
    };

    let promql_rate = |metric: &str| {
        if individual {
            format!("sum by (name) (irate({metric}{{{filter}}}[5m]))")
        } else {
            format!("sum(irate({metric}{{{filter}}}[5m]))")
        }
    };

    let sql_rate = |metric: &str, label_filter: Option<(&str, &str)>| -> String {
        if individual {
            sql::cgroup_irate_by_name(metric, side, label_filter)
        } else {
            sql::cgroup_irate_total(metric, side, label_filter)
        }
    };

    // CPU Total Cores
    group.plot_promql_with_sql(
        PlotOpts::counter(
            "Total CPU Cores",
            format!("{prefix}-total-cores"),
            Unit::Count,
        ),
        format!("{} / 1000000000", promql_rate("cgroup_cpu_usage")),
        sql::scale_v(sql_rate("cgroup_cpu_usage", None), 1e9),
    );

    // CPU User Cores
    group.plot_promql_with_sql(
        PlotOpts::counter("User CPU Cores", format!("{prefix}-user-cores"), Unit::Count),
        if individual {
            format!("sum by (name) (irate(cgroup_cpu_usage{{state=\"user\",{filter}}}[5m])) / 1000000000")
        } else {
            format!("sum(irate(cgroup_cpu_usage{{state=\"user\",{filter}}}[5m])) / 1000000000")
        },
        sql::scale_v(sql_rate("cgroup_cpu_usage", Some(("state", "user"))), 1e9),
    );

    // CPU System Cores
    group.plot_promql_with_sql(
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
        sql::scale_v(sql_rate("cgroup_cpu_usage", Some(("state", "system"))), 1e9),
    );

    // CPU Migrations
    group.plot_promql_with_sql(
        PlotOpts::counter(
            "CPU Migrations",
            format!("{prefix}-cpu-migrations"),
            Unit::Rate,
        ),
        promql_rate("cgroup_cpu_migrations"),
        sql_rate("cgroup_cpu_migrations", None),
    );

    // CPU Throttled Time
    group.plot_promql_with_sql(
        PlotOpts::counter(
            "CPU Throttled Time",
            format!("{prefix}-cpu-throttled-time"),
            Unit::Time,
        ),
        promql_rate("cgroup_cpu_throttled_time"),
        sql_rate("cgroup_cpu_throttled_time", None),
    );

    // IPC
    group.plot_promql_with_sql(
        PlotOpts::counter("IPC", format!("{prefix}-ipc"), Unit::Count),
        if individual {
            format!("sum by (name) (irate(cgroup_cpu_instructions{{{filter}}}[5m])) / sum by (name) (irate(cgroup_cpu_cycles{{{filter}}}[5m]))")
        } else {
            format!("sum(irate(cgroup_cpu_instructions{{{filter}}}[5m])) / sum(irate(cgroup_cpu_cycles{{{filter}}}[5m]))")
        },
        if individual {
            sql::cgroup_ratio_by_name("cgroup_cpu_instructions", "cgroup_cpu_cycles", side)
        } else {
            sql::cgroup_ratio_total("cgroup_cpu_instructions", "cgroup_cpu_cycles", side)
        },
    );

    // TLB Flushes
    group.plot_promql_with_sql(
        PlotOpts::counter("TLB Flushes", format!("{prefix}-tlb-flush"), Unit::Rate),
        promql_rate("cgroup_cpu_tlb_flush"),
        sql_rate("cgroup_cpu_tlb_flush", None),
    );

    // Syscalls
    group.plot_promql_with_sql(
        PlotOpts::counter("Syscalls", format!("{prefix}-syscall"), Unit::Rate),
        promql_rate("cgroup_syscall"),
        sql_rate("cgroup_syscall", None),
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
        group.plot_promql_with_sql(
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
            sql_rate("cgroup_syscall", Some(("op", &op_lower))),
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
