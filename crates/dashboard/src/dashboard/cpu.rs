use crate::Tsdb;
use crate::plot::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Utilization
     */

    let mut utilization = Group::new("Utilization", "utilization");

    let busy = utilization.subgroup("Total CPU");
    busy.describe("Overall CPU busy time across all cores, with per-core breakdown.");
    busy.plot_promql(
        PlotOpts::counter("Busy %", "busy-pct", Unit::Percentage).percentage_range(),
        "sum by (node) (irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
    );
    busy.plot_promql(
        PlotOpts::counter("Busy % (Per-CPU)", "busy-pct-per-cpu", Unit::Percentage)
            .percentage_range(),
        "sum by (id, node) (irate(cpu_usage[5m])) / 1000000000".to_string(),
    );

    let by_state = utilization.subgroup("CPU Time by State");
    by_state.describe("Kernel vs. user-space CPU time, aggregate and per-core.");
    for state in &["user", "system"] {
        let capitalized = if *state == "user" { "User" } else { "System" };
        by_state.plot_promql(
            PlotOpts::counter(
                format!("{capitalized} %"),
                format!("{state}-pct"),
                Unit::Percentage,
            )
            .percentage_range(),
            format!("sum by (node) (irate(cpu_usage{{state=\"{state}\"}}[5m])) / cpu_cores / 1000000000"),
        );
        by_state.plot_promql(
            PlotOpts::counter(
                format!("{capitalized} % (Per-CPU)"),
                format!("{state}-pct-per-cpu"),
                Unit::Percentage,
            )
            .percentage_range(),
            format!("sum by (id, node) (irate(cpu_usage{{state=\"{state}\"}}[5m])) / 1000000000"),
        );
    }

    view.group(utilization);

    /*
     * Performance
     */

    let mut performance = Group::new("Performance", "performance");

    let ipc = performance.subgroup("Instructions per Cycle");
    ipc.describe("How efficiently the CPU retires instructions per clock cycle.");
    ipc.plot_promql(
        PlotOpts::counter("IPC", "ipc", Unit::Count),
        "sum by (node) (irate(cpu_instructions[5m])) / sum by (node) (irate(cpu_cycles[5m]))".to_string(),
    );
    ipc.plot_promql(
        PlotOpts::counter("IPC (Per-CPU)", "ipc-per-cpu", Unit::Count),
        "sum by (id, node) (irate(cpu_instructions[5m])) / sum by (id, node) (irate(cpu_cycles[5m]))"
            .to_string(),
    );

    let ipns = performance.subgroup("Instructions per Nanosecond");
    ipns.describe("Wall-clock-normalized instruction throughput — accounts for frequency scaling.");
    ipns.plot_promql(
        PlotOpts::counter("IPNS", "ipns", Unit::Count),
        "sum by (node) (irate(cpu_instructions[5m])) / sum by (node) (irate(cpu_cycles[5m])) * sum by (node) (irate(cpu_tsc[5m])) * sum by (node) (irate(cpu_aperf[5m])) / sum by (node) (irate(cpu_mperf[5m])) / 1000000000 / cpu_cores".to_string(),
    );
    ipns.plot_promql(
        PlotOpts::counter("IPNS (Per-CPU)", "ipns-per-cpu", Unit::Count),
        "sum by (id, node) (irate(cpu_instructions[5m])) / sum by (id, node) (irate(cpu_cycles[5m])) * sum by (id, node) (irate(cpu_tsc[5m])) * sum by (id, node) (irate(cpu_aperf[5m])) / sum by (id, node) (irate(cpu_mperf[5m])) / 1000000000".to_string(),
    );

    let l3 = performance.subgroup("L3 Cache Hit Rate");
    l3.describe("Fraction of L3 cache accesses that hit, indicating last-level cache efficiency.");
    l3.plot_promql(
        PlotOpts::counter("L3 Hit %", "l3-hit", Unit::Percentage).percentage_range(),
        "1 - sum by (node) (irate(cpu_l3_miss[5m])) / sum by (node) (irate(cpu_l3_access[5m]))".to_string(),
    );
    l3.plot_promql(
        PlotOpts::counter("L3 Hit % (Per-CPU)", "l3-hit-per-cpu", Unit::Percentage)
            .percentage_range(),
        "1 - sum by (id, node) (irate(cpu_l3_miss[5m])) / sum by (id, node) (irate(cpu_l3_access[5m]))"
            .to_string(),
    );

    let freq = performance.subgroup("Frequency");
    freq.describe("Effective CPU clock speed, averaged and per-core.");
    freq.plot_promql(
        PlotOpts::counter("Frequency", "frequency", Unit::Frequency),
        "sum by (node) (irate(cpu_tsc[5m])) * sum by (node) (irate(cpu_aperf[5m])) / sum by (node) (irate(cpu_mperf[5m])) / cpu_cores".to_string(),
    );
    freq.plot_promql(
        PlotOpts::counter("Frequency (Per-CPU)", "frequency-per-cpu", Unit::Frequency),
        "sum by (id, node) (irate(cpu_tsc[5m])) * sum by (id, node) (irate(cpu_aperf[5m])) / sum by (id, node) (irate(cpu_mperf[5m]))".to_string(),
    );

    view.group(performance);

    /*
     * Branch Prediction
     */

    let mut branch = Group::new("Branch Prediction", "branch-prediction");

    let miss = branch.subgroup("Misprediction Rate");
    miss.describe("Fraction of branches that the predictor got wrong.");
    miss.plot_promql(
        PlotOpts::counter("Misprediction Rate %", "branch-miss-rate", Unit::Percentage)
            .percentage_range(),
        "sum by (node) (irate(cpu_branch_misses[5m])) / sum by (node) (irate(cpu_branch_instructions[5m]))".to_string(),
    );
    miss.plot_promql(
        PlotOpts::counter(
            "Misprediction Rate % (Per-CPU)",
            "branch-miss-rate-per-cpu",
            Unit::Percentage,
        )
        .percentage_range(),
        "sum by (id, node) (irate(cpu_branch_misses[5m])) / sum by (id, node) (irate(cpu_branch_instructions[5m]))"
            .to_string(),
    );

    let activity = branch.subgroup("Branch Activity");
    activity.describe("Absolute branch instruction and miss rates.");
    activity.plot_promql(
        PlotOpts::counter("Instructions", "branch-instructions", Unit::Rate),
        "sum by (node) (irate(cpu_branch_instructions[5m]))".to_string(),
    );
    activity.plot_promql(
        PlotOpts::counter(
            "Instructions (Per-CPU)",
            "branch-instructions-per-cpu",
            Unit::Rate,
        ),
        "sum by (id, node) (irate(cpu_branch_instructions[5m]))".to_string(),
    );
    activity.plot_promql(
        PlotOpts::counter("Misses", "branch-misses", Unit::Rate),
        "sum by (node) (irate(cpu_branch_misses[5m]))".to_string(),
    );
    activity.plot_promql(
        PlotOpts::counter("Misses (Per-CPU)", "branch-misses-per-cpu", Unit::Rate),
        "sum by (id, node) (irate(cpu_branch_misses[5m]))".to_string(),
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

    let misses = dtlb.subgroup("DTLB Misses");
    misses.describe("Raw data-TLB miss rate, aggregated and per-core.");
    misses.plot_promql(
        PlotOpts::counter("Misses", "dtlb-misses", Unit::Rate),
        "sum by (node) (irate(cpu_dtlb_miss[5m]))".to_string(),
    );
    misses.plot_promql(
        PlotOpts::counter("Misses (Per-CPU)", "dtlb-misses-per-cpu", Unit::Rate),
        "sum by (id, node) (irate(cpu_dtlb_miss[5m]))".to_string(),
    );

    let mpki = dtlb.subgroup("DTLB MPKI");
    mpki.describe("Misses per thousand instructions, normalized so workload differences don't distort the rate.");
    mpki.plot_promql(
        PlotOpts::counter("MPKI", "dtlb-mpki", Unit::Count),
        "sum by (node) (irate(cpu_dtlb_miss[5m])) / sum by (node) (irate(cpu_instructions[5m])) * 1000".to_string(),
    );
    mpki.plot_promql(
        PlotOpts::counter("MPKI (Per-CPU)", "dtlb-mpki-per-cpu", Unit::Count),
        "sum by (id, node) (irate(cpu_dtlb_miss[5m])) / sum by (id, node) (irate(cpu_instructions[5m])) * 1000"
            .to_string(),
    );

    view.group(dtlb);

    /*
     * Migrations
     */

    let mut migrations = Group::new("Migrations", "migrations");

    let to = migrations.subgroup("Incoming Migrations");
    to.describe("Tasks migrated onto a CPU, per second.");
    to.plot_promql(
        PlotOpts::counter("To", "cpu-migrations-to", Unit::Rate),
        "sum by (node) (irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
    );
    to.plot_promql(
        PlotOpts::counter("To (Per-CPU)", "cpu-migrations-to-per-cpu", Unit::Rate),
        "sum by (id, node) (irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
    );

    let from = migrations.subgroup("Outgoing Migrations");
    from.describe("Tasks migrated off a CPU, per second.");
    from.plot_promql(
        PlotOpts::counter("From", "cpu-migrations-from", Unit::Rate),
        "sum by (node) (irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
    );
    from.plot_promql(
        PlotOpts::counter("From (Per-CPU)", "cpu-migrations-from-per-cpu", Unit::Rate),
        "sum by (id, node) (irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
    );

    view.group(migrations);

    /*
     * TLB Flush
     */

    let mut tlb = Group::new("TLB Flush", "tlb-flush");

    let total = tlb.subgroup("Total TLB Flushes");
    total.describe("Aggregate TLB invalidation rate across all reasons.");
    total.plot_promql(
        PlotOpts::counter("Total", "tlb-total", Unit::Rate),
        "sum by (node) (irate(cpu_tlb_flush[5m]))".to_string(),
    );
    total.plot_promql(
        PlotOpts::counter("Total (Per-CPU)", "tlb-total-per-cpu", Unit::Rate),
        "sum by (id, node) (irate(cpu_tlb_flush[5m]))".to_string(),
    );

    for reason in &[
        ("local_mm_shootdown", "Local MM Shootdown"),
        ("remote_send_ipi", "Remote Send IPI"),
        ("remote_shootdown", "Remote Shootdown"),
        ("task_switch", "Task Switch"),
    ] {
        let (reason_value, label) = reason;
        let id = format!("tlb-{}", reason_value.replace('_', "-"));
        let sg = tlb.subgroup(*label);
        sg.plot_promql(
            PlotOpts::counter(*label, &id, Unit::Rate),
            format!("sum by (node) (irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
        );
        sg.plot_promql(
            PlotOpts::counter(
                format!("{label} (Per-CPU)"),
                format!("{id}-per-cpu"),
                Unit::Rate,
            ),
            format!("sum by (id, node) (irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
        );
    }

    view.group(tlb);

    view
}
