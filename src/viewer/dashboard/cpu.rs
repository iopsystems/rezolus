use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Utilization
     */

    let mut utilization = Group::new("Utilization", "utilization");

    // Average CPU busy percentage across all cores
    utilization.plot_promql(
        PlotOpts::counter("Busy %", "busy-pct", Unit::Percentage).range(0.0, 1.0),
        "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
    );

    // Per-CPU busy percentage heatmap
    utilization.plot_promql(
        PlotOpts::counter("Busy % (Per-CPU)", "busy-pct-per-cpu", Unit::Percentage).range(0.0, 1.0),
        "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
    );

    // User and System CPU usage
    for state in &["user", "system"] {
        let capitalized = if *state == "user" { "User" } else { "System" };

        // Average across all cores for this state
        utilization.plot_promql(
            PlotOpts::counter(
                format!("{capitalized} %"),
                format!("{state}-pct"),
                Unit::Percentage,
            )
            .range(0.0, 1.0),
            format!("sum(irate(cpu_usage{{state=\"{state}\"}}[5m])) / cpu_cores / 1000000000"),
        );

        // Per-CPU for this state
        utilization.plot_promql(
            PlotOpts::counter(
                format!("{capitalized} % (Per-CPU)"),
                format!("{state}-pct-per-cpu"),
                Unit::Percentage,
            )
            .range(0.0, 1.0),
            format!("sum by (id) (irate(cpu_usage{{state=\"{state}\"}}[5m])) / 1000000000"),
        );
    }

    view.group(utilization);

    /*
     * Performance
     */

    let mut performance = Group::new("Performance", "performance");

    // IPC (Instructions per Cycle)
    performance.plot_promql(
        PlotOpts::counter("Instructions per Cycle (IPC)", "ipc", Unit::Count),
        "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))".to_string(),
    );

    // Per-CPU IPC
    performance.plot_promql(
        PlotOpts::counter("IPC (Per-CPU)", "ipc-per-cpu", Unit::Count),
        "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m]))"
            .to_string(),
    );

    // IPNS (Instructions per Nanosecond)
    // Complex calculation: instructions / cycles * tsc * aperf / mperf / 1e9 / cores
    performance.plot_promql(
        PlotOpts::counter("Instructions per Nanosecond (IPNS)", "ipns", Unit::Count),
        "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m])) * sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / 1000000000 / cpu_cores".to_string(),
    );

    // Per-CPU IPNS
    performance.plot_promql(
        PlotOpts::counter("IPNS (Per-CPU)", "ipns-per-cpu", Unit::Count),
        "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m])) * sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m])) / 1000000000".to_string(),
    );

    // L3 Cache Hit Rate
    performance.plot_promql(
        PlotOpts::counter("L3 Hit %", "l3-hit", Unit::Percentage).range(0.0, 1.0),
        "1 - sum(irate(cpu_l3_miss[5m])) / sum(irate(cpu_l3_access[5m]))".to_string(),
    );

    // Per-CPU L3 Hit Rate
    performance.plot_promql(
        PlotOpts::counter("L3 Hit % (Per-CPU)", "l3-hit-per-cpu", Unit::Percentage).range(0.0, 1.0),
        "1 - sum by (id) (irate(cpu_l3_miss[5m])) / sum by (id) (irate(cpu_l3_access[5m]))"
            .to_string(),
    );

    // CPU Frequency
    performance.plot_promql(
        PlotOpts::counter("Frequency", "frequency", Unit::Frequency),
        "sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / cpu_cores".to_string(),
    );

    // Per-CPU Frequency
    performance.plot_promql(
        PlotOpts::counter("Frequency (Per-CPU)", "frequency-per-cpu", Unit::Frequency),
        "sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m]))".to_string(),
    );

    view.group(performance);

    /*
     * Branch Prediction
     */

    let mut branch = Group::new("Branch Prediction", "branch-prediction");

    // Branch Misprediction Rate %
    branch.plot_promql(
        PlotOpts::counter("Misprediction Rate %", "branch-miss-rate", Unit::Percentage)
            .range(0.0, 1.0),
        "sum(irate(cpu_branch_misses[5m])) / sum(irate(cpu_branch_instructions[5m]))".to_string(),
    );

    // Per-CPU Branch Misprediction Rate
    branch.plot_promql(
        PlotOpts::counter(
            "Misprediction Rate % (Per-CPU)",
            "branch-miss-rate-per-cpu",
            Unit::Percentage,
        )
        .range(0.0, 1.0),
        "sum by (id) (irate(cpu_branch_misses[5m])) / sum by (id) (irate(cpu_branch_instructions[5m]))"
            .to_string(),
    );

    // Branch Instructions per Second
    branch.plot_promql(
        PlotOpts::counter("Instructions", "branch-instructions", Unit::Rate),
        "sum(irate(cpu_branch_instructions[5m]))".to_string(),
    );

    // Per-CPU Branch Instructions
    branch.plot_promql(
        PlotOpts::counter(
            "Instructions (Per-CPU)",
            "branch-instructions-per-cpu",
            Unit::Rate,
        ),
        "sum by (id) (irate(cpu_branch_instructions[5m]))".to_string(),
    );

    // Branch Misses per Second
    branch.plot_promql(
        PlotOpts::counter("Misses", "branch-misses", Unit::Rate),
        "sum(irate(cpu_branch_misses[5m]))".to_string(),
    );

    // Per-CPU Branch Misses
    branch.plot_promql(
        PlotOpts::counter("Misses (Per-CPU)", "branch-misses-per-cpu", Unit::Rate),
        "sum by (id) (irate(cpu_branch_misses[5m]))".to_string(),
    );

    view.group(branch);

    /*
     * DTLB (Data Translation Lookaside Buffer)
     * cpu_dtlb_miss aggregates all variants:
     * - unlabeled (AMD/ARM combined)
     * - op="load" (Intel)
     * - op="store" (Intel)
     */

    let mut dtlb = Group::new("DTLB", "dtlb");

    // Total DTLB Misses (aggregates all op labels)
    dtlb.plot_promql(
        PlotOpts::counter("Misses", "dtlb-misses", Unit::Rate),
        "sum(irate(cpu_dtlb_miss[5m]))".to_string(),
    );

    // Per-CPU DTLB Misses
    dtlb.plot_promql(
        PlotOpts::counter("Misses (Per-CPU)", "dtlb-misses-per-cpu", Unit::Rate),
        "sum by (id) (irate(cpu_dtlb_miss[5m]))".to_string(),
    );

    // DTLB MPKI (Misses Per Kilo Instructions) - system-wide
    dtlb.plot_promql(
        PlotOpts::counter("MPKI", "dtlb-mpki", Unit::Count),
        "sum(irate(cpu_dtlb_miss[5m])) / sum(irate(cpu_instructions[5m])) * 1000".to_string(),
    );

    // DTLB MPKI - per-CPU
    dtlb.plot_promql(
        PlotOpts::counter("MPKI (Per-CPU)", "dtlb-mpki-per-cpu", Unit::Count),
        "sum by (id) (irate(cpu_dtlb_miss[5m])) / sum by (id) (irate(cpu_instructions[5m])) * 1000"
            .to_string(),
    );

    view.group(dtlb);

    /*
     * Migrations
     */

    let mut migrations = Group::new("Migrations", "migrations");

    // Migrations To
    migrations.plot_promql(
        PlotOpts::counter("To", "cpu-migrations-to", Unit::Rate),
        "sum(irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
    );

    // Per-CPU Migrations To
    migrations.plot_promql(
        PlotOpts::counter("To (Per-CPU)", "cpu-migrations-to-per-cpu", Unit::Rate),
        "sum by (id) (irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
    );

    // Migrations From
    migrations.plot_promql(
        PlotOpts::counter("From", "cpu-migrations-from", Unit::Rate),
        "sum(irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
    );

    // Per-CPU Migrations From
    migrations.plot_promql(
        PlotOpts::counter("From (Per-CPU)", "cpu-migrations-from-per-cpu", Unit::Rate),
        "sum by (id) (irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
    );

    view.group(migrations);

    /*
     * TLB Flush
     */

    let mut tlb = Group::new("TLB Flush", "tlb-flush");

    // Total TLB Flushes
    tlb.plot_promql(
        PlotOpts::counter("Total", "tlb-total", Unit::Rate),
        "sum(irate(cpu_tlb_flush[5m]))".to_string(),
    );

    // Per-CPU TLB Flushes
    tlb.plot_promql(
        PlotOpts::counter("Total (Per-CPU)", "tlb-total-per-cpu", Unit::Rate),
        "sum by (id) (irate(cpu_tlb_flush[5m]))".to_string(),
    );

    // TLB Flushes by reason
    for reason in &[
        ("local_mm_shootdown", "Local MM Shootdown"),
        ("remote_send_ipi", "Remote Send IPI"),
        ("remote_shootdown", "Remote Shootdown"),
        ("task_switch", "Task Switch"),
    ] {
        let (reason_value, label) = reason;
        let id = format!("tlb-{}", reason_value.replace('_', "-"));

        tlb.plot_promql(
            PlotOpts::counter(*label, &id, Unit::Rate),
            format!("sum(irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
        );

        tlb.plot_promql(
            PlotOpts::counter(
                format!("{label} (Per-CPU)"),
                format!("{id}-per-cpu"),
                Unit::Rate,
            ),
            format!("sum by (id) (irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
        );
    }

    view.group(tlb);

    view
}
