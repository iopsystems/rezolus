use crate::MetricsSource;
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

    group.plot_promql(
        PlotOpts::counter(
            "Total CPU Cores",
            format!("{prefix}-total-cores"),
            Unit::Count,
        ),
        format!("{} / 1000000000", rate("cgroup_cpu_usage")),
    );

    group.plot_promql(
        PlotOpts::counter("User CPU Cores", format!("{prefix}-user-cores"), Unit::Count),
        if individual {
            format!("sum by (name) (irate(cgroup_cpu_usage{{state=\"user\",{filter}}}[5m])) / 1000000000")
        } else {
            format!("sum(irate(cgroup_cpu_usage{{state=\"user\",{filter}}}[5m])) / 1000000000")
        },
    );

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

    group.plot_promql(
        PlotOpts::counter(
            "CPU Migrations",
            format!("{prefix}-cpu-migrations"),
            Unit::Rate,
        ),
        rate("cgroup_cpu_migrations"),
    );

    group.plot_promql(
        PlotOpts::counter(
            "CPU Throttled Time",
            format!("{prefix}-cpu-throttled-time"),
            Unit::Time,
        ),
        rate("cgroup_cpu_throttled_time"),
    );

    group.plot_promql(
        PlotOpts::counter(
            "ext4 Blocked Time",
            format!("{prefix}-ext4-blocked-time"),
            Unit::Time,
        ),
        rate("cgroup_ext4_op_time"),
    );

    group.plot_promql(
        PlotOpts::counter(
            "XFS Log Blocked Time",
            format!("{prefix}-xfs-log-blocked-time"),
            Unit::Time,
        ),
        rate("cgroup_xfs_log_wait_time"),
    );

    group.plot_promql(
        PlotOpts::counter("IPC", format!("{prefix}-ipc"), Unit::Count),
        if individual {
            format!("sum by (name) (irate(cgroup_cpu_instructions{{{filter}}}[5m])) / sum by (name) (irate(cgroup_cpu_cycles{{{filter}}}[5m]))")
        } else {
            format!("sum(irate(cgroup_cpu_instructions{{{filter}}}[5m])) / sum(irate(cgroup_cpu_cycles{{{filter}}}[5m]))")
        },
    );

    group.plot_promql(
        PlotOpts::counter("TLB Flushes", format!("{prefix}-tlb-flush"), Unit::Rate),
        rate("cgroup_cpu_tlb_flush"),
    );

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
        "Sync",
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

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
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

#[cfg(test)]
mod tests {
    use crate::dashboard::{build_dashboard_context, generate_section};

    /// Path to the cross-language fixture, relative to this crate.
    const FIXTURE: &str = "../../tests/fixtures/cgroup_queries.json";

    /// Every query this section generates that carries the selection
    /// placeholder. `tests/cgroup_pattern.test.mjs` substitutes a selection
    /// into each one, so a new query shape here is covered there.
    ///
    /// `UPDATE_FIXTURES=1 cargo test -p dashboard cgroup_queries` rewrites it.
    #[test]
    fn cgroup_queries_fixture_matches_the_generator() {
        let data = metriken_query::MemoryStore::builder().build();
        let ctx = build_dashboard_context(None, &[], None, &[]);
        let view = generate_section(&data, "/cgroups", &ctx).expect("cgroups section");
        let json = serde_json::to_value(&view).unwrap();

        let mut queries = Vec::new();
        fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
                serde_json::Value::Object(o) => {
                    if let Some(q) = o.get("promql_query").and_then(|q| q.as_str()) {
                        if q.contains("__SELECTED_CGROUPS__") {
                            out.push(q.to_string());
                        }
                    }
                    o.values().for_each(|v| walk(v, out));
                }
                _ => {}
            }
        }
        walk(&json, &mut queries);
        queries.sort();
        queries.dedup();

        let body = serde_json::to_string_pretty(&queries).unwrap() + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
        if std::env::var_os("UPDATE_FIXTURES").is_some() {
            std::fs::write(&path, &body).unwrap();
        }
        let committed =
            std::fs::read_to_string(&path).expect("fixture; regenerate with UPDATE_FIXTURES=1");
        assert_eq!(
            committed, body,
            "regenerate with UPDATE_FIXTURES=1 if the change is deliberate"
        );
    }
}
