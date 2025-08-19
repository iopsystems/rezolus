use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Utilization
     */

    let mut utilization = Group::new("Utilization", "utilization");

    // Average CPU busy percentage across all cores
    utilization.plot_promql(
        PlotOpts::line("Busy %", "busy-pct", Unit::Percentage),
        "irate(cpu_usage[5m]) / cpu_cores / 1000000000".to_string(),
    );

    // Per-CPU busy percentage heatmap
    // cpu_heatmap("cpu_usage", ()) becomes: sum by (id) (irate(cpu_usage[5m])) / 1e9
    utilization.plot_promql(
        PlotOpts::heatmap("Busy % (Per-CPU)", "busy-pct-per-cpu", Unit::Percentage),
        "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
    );

    // User and System CPU usage
    for state in &["user", "system"] {
        let capitalized = if *state == "user" { "User" } else { "System" };

        // Average across all cores for this state
        utilization.plot_promql(
            PlotOpts::line(
                format!("{capitalized} %"),
                format!("{state}-pct"),
                Unit::Percentage,
            ),
            format!("irate(cpu_usage{{state=\"{state}\"}}[5m]) / cpu_cores / 1000000000"),
        );

        // Per-CPU for this state
        utilization.plot_promql(
            PlotOpts::heatmap(
                format!("{capitalized} % (Per-CPU)"),
                format!("{state}-pct-per-cpu"),
                Unit::Percentage,
            ),
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
        PlotOpts::line("Instructions per Cycle (IPC)", "ipc", Unit::Count),
        "irate(cpu_instructions[5m]) / irate(cpu_cycles[5m])".to_string(),
    );

    // Per-CPU IPC
    performance.plot_promql(
        PlotOpts::heatmap("IPC (Per-CPU)", "ipc-per-cpu", Unit::Count),
        "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m]))"
            .to_string(),
    );

    // IPNS (Instructions per Nanosecond)
    // Complex calculation: instructions / cycles * tsc * aperf / mperf / 1e9 / cores
    performance.plot_promql(
        PlotOpts::line("Instructions per Nanosecond (IPNS)", "ipns", Unit::Count),
        "irate(cpu_instructions[5m]) / irate(cpu_cycles[5m]) * irate(cpu_tsc[5m]) * irate(cpu_aperf[5m]) / irate(cpu_mperf[5m]) / 1000000000 / cpu_cores".to_string(),
    );

    // Per-CPU IPNS
    performance.plot_promql(
        PlotOpts::heatmap("IPNS (Per-CPU)", "ipns-per-cpu", Unit::Count),
        "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m])) * sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m])) / 1000000000".to_string(),
    );

    // L3 Cache Hit Rate
    performance.plot_promql(
        PlotOpts::line("L3 Hit %", "l3-hit", Unit::Percentage),
        "(1 - irate(cpu_l3_miss[5m]) / irate(cpu_l3_access[5m])) * 100".to_string(),
    );

    // Per-CPU L3 Hit Rate
    performance.plot_promql(
        PlotOpts::heatmap("L3 Hit % (Per-CPU)", "l3-hit-per-cpu", Unit::Percentage),
        "(1 - sum by (id) (irate(cpu_l3_miss[5m])) / sum by (id) (irate(cpu_l3_access[5m]))) * 100"
            .to_string(),
    );

    // CPU Frequency
    performance.plot_promql(
        PlotOpts::line("Frequency", "frequency", Unit::Frequency),
        "irate(cpu_tsc[5m]) * irate(cpu_aperf[5m]) / irate(cpu_mperf[5m]) / cpu_cores".to_string(),
    );

    // Per-CPU Frequency
    performance.plot_promql(
        PlotOpts::heatmap("Frequency (Per-CPU)", "frequency-per-cpu", Unit::Frequency),
        "sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m]))".to_string(),
    );

    view.group(performance);

    /*
     * Migrations
     */

    let mut migrations = Group::new("Migrations", "migrations");

    // Migrations To
    migrations.plot_promql(
        PlotOpts::line("To", "cpu-migrations-to", Unit::Rate),
        "irate(cpu_migrations{direction=\"to\"}[5m])".to_string(),
    );

    // Per-CPU Migrations To
    migrations.plot_promql(
        PlotOpts::heatmap("To (Per-CPU)", "cpu-migrations-to-per-cpu", Unit::Rate),
        "sum by (id) (irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
    );

    // Migrations From
    migrations.plot_promql(
        PlotOpts::line("From", "cpu-migrations-from", Unit::Rate),
        "irate(cpu_migrations{direction=\"from\"}[5m])".to_string(),
    );

    // Per-CPU Migrations From
    migrations.plot_promql(
        PlotOpts::heatmap("From (Per-CPU)", "cpu-migrations-from-per-cpu", Unit::Rate),
        "sum by (id) (irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
    );

    view.group(migrations);

    /*
     * TLB Flush
     */

    let mut tlb = Group::new("TLB Flush", "tlb-flush");

    // Total TLB Flushes
    tlb.plot_promql(
        PlotOpts::line("Total", "tlb-total", Unit::Rate),
        "irate(cpu_tlb_flush[5m])".to_string(),
    );

    // Per-CPU TLB Flushes
    tlb.plot_promql(
        PlotOpts::heatmap("Total (Per-CPU)", "tlb-total-per-cpu", Unit::Rate),
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
            PlotOpts::line(*label, &id, Unit::Rate),
            format!("irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m])"),
        );

        tlb.plot_promql(
            PlotOpts::heatmap(
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
