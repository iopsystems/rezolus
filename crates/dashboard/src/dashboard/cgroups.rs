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

    // An individual plot draws a line per cgroup, named and coloured by the
    // cgroup, also when one cgroup is selected; inferred from the result, one
    // series would be a plain line in the accent colour.
    let plot = |group: &mut Group, opts: PlotOpts, query: String| {
        group.plot_promql(
            if individual {
                opts.with_style("multi")
            } else {
                opts
            },
            query,
        )
    };

    let rate = |metric: &str| {
        if individual {
            format!("sum by (name) (irate({metric}{{{filter}}}[5m]))")
        } else {
            format!("sum(irate({metric}{{{filter}}}[5m]))")
        }
    };

    // Memory accounting includes descendants. Keep individual subtree series;
    // summing non-selected ancestors and children would double-count memory.
    if individual {
        plot(
            group,
            PlotOpts::gauge(
                "Memory (Including Descendants)",
                "individual-memory",
                Unit::Bytes,
            ),
            format!("cgroup_memory_usage{{{filter}}}"),
        );
        plot(
            group,
            PlotOpts::gauge(
                "Swap (Including Descendants)",
                "individual-memory-swap",
                Unit::Bytes,
            ),
            format!("cgroup_memory_swap_usage{{{filter}}}"),
        );
        for (kind, title) in [
            ("anon", "Anonymous Pages"),
            ("file", "File Cache (Includes Shared Memory)"),
            ("shmem", "Shared Memory / tmpfs"),
            ("kernel", "Kernel Memory"),
            ("sock", "Socket Buffers"),
            ("slab_reclaimable", "Reclaimable Slab"),
            ("slab_unreclaimable", "Unreclaimable Slab"),
            ("pagetables", "Page Tables"),
            ("kernel_stack", "Kernel Stacks"),
            ("file_dirty", "Dirty File Pages"),
            ("file_writeback", "File Pages Under Writeback"),
            ("file_mapped", "Mapped File Pages"),
            ("active_anon", "Active Anonymous Pages"),
            ("inactive_anon", "Inactive Anonymous Pages"),
            ("active_file", "Active File Pages"),
            ("inactive_file", "Inactive File Pages"),
            ("unevictable", "Unevictable Pages"),
            ("slab", "Slab (Total)"),
            ("sec_pagetables", "Secondary Page Tables"),
            ("percpu", "Per-CPU Allocations"),
            ("vmalloc", "vmalloc"),
            ("hugetlb", "HugeTLB Pages"),
            ("anon_thp", "Anonymous Transparent Huge Pages"),
            ("file_thp", "File Transparent Huge Pages"),
            ("shmem_thp", "Shared Memory Transparent Huge Pages"),
            ("swapcached", "Swap Cache"),
            ("zswap", "Zswap Compressed Pool"),
            ("zswapped", "Pages Backed by Zswap"),
        ] {
            plot(
                group,
                PlotOpts::gauge(title, format!("individual-memory-{kind}"), Unit::Bytes),
                format!("cgroup_memory_stat{{kind=\"{kind}\",{filter}}}"),
            );
        }
    }

    plot(
        group,
        PlotOpts::counter(
            "Total CPU Cores",
            format!("{prefix}-total-cores"),
            Unit::Count,
        ),
        format!("{} / 1000000000", rate("cgroup_cpu_usage")),
    );

    plot(
        group,
        PlotOpts::counter(
            "User CPU Cores",
            format!("{prefix}-user-cores"),
            Unit::Count,
        ),
        if individual {
            format!(
                "sum by (name) (irate(cgroup_cpu_usage{{state=\"user\",{filter}}}[5m])) / 1000000000"
            )
        } else {
            format!("sum(irate(cgroup_cpu_usage{{state=\"user\",{filter}}}[5m])) / 1000000000")
        },
    );

    plot(
        group,
        PlotOpts::counter(
            "System CPU Cores",
            format!("{prefix}-system-cores"),
            Unit::Count,
        ),
        if individual {
            format!(
                "sum by (name) (irate(cgroup_cpu_usage{{state=\"system\",{filter}}}[5m])) / 1000000000"
            )
        } else {
            format!("sum(irate(cgroup_cpu_usage{{state=\"system\",{filter}}}[5m])) / 1000000000")
        },
    );

    plot(
        group,
        PlotOpts::counter(
            "CPU Migrations",
            format!("{prefix}-cpu-migrations"),
            Unit::Rate,
        ),
        rate("cgroup_cpu_migrations"),
    );

    plot(
        group,
        PlotOpts::counter(
            "CPU Throttled Time",
            format!("{prefix}-cpu-throttled-time"),
            Unit::Time,
        ),
        rate("cgroup_cpu_throttled_time"),
    );

    plot(
        group,
        PlotOpts::counter(
            "ext4 Blocked Time",
            format!("{prefix}-ext4-blocked-time"),
            Unit::Time,
        ),
        rate("cgroup_ext4_op_time"),
    );

    plot(
        group,
        PlotOpts::counter(
            "XFS Log Blocked Time",
            format!("{prefix}-xfs-log-blocked-time"),
            Unit::Time,
        ),
        rate("cgroup_xfs_log_wait_time"),
    );

    plot(
        group,
        PlotOpts::counter("IPC", format!("{prefix}-ipc"), Unit::Count),
        if individual {
            format!(
                "sum by (name) (irate(cgroup_cpu_instructions{{{filter}}}[5m])) / sum by (name) (irate(cgroup_cpu_cycles{{{filter}}}[5m]))"
            )
        } else {
            format!(
                "sum(irate(cgroup_cpu_instructions{{{filter}}}[5m])) / sum(irate(cgroup_cpu_cycles{{{filter}}}[5m]))"
            )
        },
    );

    plot(
        group,
        PlotOpts::counter("TLB Flushes", format!("{prefix}-tlb-flush"), Unit::Rate),
        rate("cgroup_cpu_tlb_flush"),
    );

    plot(
        group,
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
        plot(
            group,
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
                "cgroup_syscall",
                "cgroup_memory_usage",
                "cgroup_memory_swap_usage",
                "cgroup_memory_stat"
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
