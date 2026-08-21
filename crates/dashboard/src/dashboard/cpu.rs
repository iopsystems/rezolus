use crate::MetricsSource;
use crate::plot::*;

/// True iff the recording has more than one CPU. Per-core charts are
/// suppressed when this is false because they degenerate to the aggregate.
fn has_multiple_cpus(data: &dyn MetricsSource) -> bool {
    [
        "cpu_usage",
        "cpu_cycles",
        "cpu_instructions",
        "cpu_tsc",
        "cpu_aperf",
        "cpu_mperf",
    ]
    .iter()
    .any(|m| metric_unique_label_count(data, m, "id") > 1)
}

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let multi_cpu = has_multiple_cpus(data);

    let mut utilization = Group::new("Utilization", "utilization");

    let busy = utilization.subgroup("Total CPU");
    busy.describe("Overall CPU busy time across all cores, with per-core breakdown.");
    if multi_cpu {
        busy.plot_promql(
            PlotOpts::counter("Busy %", "busy-pct", Unit::Percentage).percentage_range(),
            "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
        );
        busy.plot_promql(
            PlotOpts::counter("Busy % (Per-CPU)", "busy-pct-per-cpu", Unit::Percentage)
                .percentage_range(),
            "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
        );
    } else {
        busy.plot_promql_full(
            PlotOpts::counter("Busy %", "busy-pct", Unit::Percentage).percentage_range(),
            "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
        );
    }

    let by_state = utilization.subgroup("CPU Time by State");
    by_state.describe("Kernel vs. user-space CPU time, aggregate and per-core.");
    for state in &["user", "system"] {
        let capitalized = if *state == "user" { "User" } else { "System" };
        if multi_cpu {
            by_state.plot_promql(
                PlotOpts::counter(
                    format!("{capitalized} %"),
                    format!("{state}-pct"),
                    Unit::Percentage,
                )
                .percentage_range(),
                format!("sum(irate(cpu_usage{{state=\"{state}\"}}[5m])) / cpu_cores / 1000000000"),
            );
            by_state.plot_promql(
                PlotOpts::counter(
                    format!("{capitalized} % (Per-CPU)"),
                    format!("{state}-pct-per-cpu"),
                    Unit::Percentage,
                )
                .percentage_range(),
                format!("sum by (id) (irate(cpu_usage{{state=\"{state}\"}}[5m])) / 1000000000"),
            );
        } else {
            by_state.plot_promql_full(
                PlotOpts::counter(
                    format!("{capitalized} %"),
                    format!("{state}-pct"),
                    Unit::Percentage,
                )
                .percentage_range(),
                format!("sum(irate(cpu_usage{{state=\"{state}\"}}[5m])) / cpu_cores / 1000000000"),
            );
        }
    }

    view.group(utilization);

    let mut performance = Group::new("Performance", "performance");

    let ipc = performance.subgroup("Instructions per Cycle");
    ipc.describe("How efficiently the CPU retires instructions per clock cycle.");
    if multi_cpu {
        ipc.plot_promql(
            PlotOpts::counter("IPC", "ipc", Unit::Count),
            "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))".to_string(),
        );
        ipc.plot_promql(
            PlotOpts::counter("IPC (Per-CPU)", "ipc-per-cpu", Unit::Count),
            "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m]))"
                .to_string(),
        );
    } else {
        ipc.plot_promql_full(
            PlotOpts::counter("IPC", "ipc", Unit::Count),
            "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))".to_string(),
        );
    }

    let ipns = performance.subgroup("Instructions per Nanosecond");
    ipns.describe("Wall-clock-normalized instruction throughput — accounts for frequency scaling.");
    if multi_cpu {
        ipns.plot_promql(
            PlotOpts::counter("IPNS", "ipns", Unit::Count),
            "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m])) * sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / 1000000000 / cpu_cores".to_string(),
        );
        ipns.plot_promql(
            PlotOpts::counter("IPNS (Per-CPU)", "ipns-per-cpu", Unit::Count),
            "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m])) * sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m])) / 1000000000".to_string(),
        );
    } else {
        ipns.plot_promql_full(
            PlotOpts::counter("IPNS", "ipns", Unit::Count),
            "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m])) * sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / 1000000000 / cpu_cores".to_string(),
        );
    }

    let l3 = performance.subgroup("L3 Cache Hit Rate");
    l3.describe("Fraction of L3 cache accesses that hit, indicating last-level cache efficiency.");
    if multi_cpu {
        l3.plot_promql(
            PlotOpts::counter("L3 Hit %", "l3-hit", Unit::Percentage).percentage_range(),
            "1 - sum(irate(cpu_l3_miss[5m])) / sum(irate(cpu_l3_access[5m]))".to_string(),
        );
        l3.plot_promql(
            PlotOpts::counter("L3 Hit % (Per-CPU)", "l3-hit-per-cpu", Unit::Percentage)
                .percentage_range(),
            "1 - sum by (id) (irate(cpu_l3_miss[5m])) / sum by (id) (irate(cpu_l3_access[5m]))"
                .to_string(),
        );
    } else {
        l3.plot_promql_full(
            PlotOpts::counter("L3 Hit %", "l3-hit", Unit::Percentage).percentage_range(),
            "1 - sum(irate(cpu_l3_miss[5m])) / sum(irate(cpu_l3_access[5m]))".to_string(),
        );
    }

    let freq = performance.subgroup("Frequency");
    freq.describe("Effective CPU clock speed, averaged and per-core.");
    if multi_cpu {
        freq.plot_promql(
            PlotOpts::counter("Frequency", "frequency", Unit::Frequency),
            "sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / cpu_cores".to_string(),
        );
        freq.plot_promql(
            PlotOpts::counter("Frequency (Per-CPU)", "frequency-per-cpu", Unit::Frequency),
            "sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m]))".to_string(),
        );
    } else {
        freq.plot_promql_full(
            PlotOpts::counter("Frequency", "frequency", Unit::Frequency),
            "sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / cpu_cores".to_string(),
        );
    }

    view.group(performance);

    let mut branch = Group::new("Branch Prediction", "branch-prediction");

    let miss = branch.subgroup("Misprediction Rate");
    miss.describe("Fraction of branches that the predictor got wrong.");
    if multi_cpu {
        miss.plot_promql(
            PlotOpts::counter("Misprediction Rate %", "branch-miss-rate", Unit::Percentage)
                .percentage_range(),
            "sum(irate(cpu_branch_misses[5m])) / sum(irate(cpu_branch_instructions[5m]))"
                .to_string(),
        );
        miss.plot_promql(
            PlotOpts::counter(
                "Misprediction Rate % (Per-CPU)",
                "branch-miss-rate-per-cpu",
                Unit::Percentage,
            )
            .percentage_range(),
            "sum by (id) (irate(cpu_branch_misses[5m])) / sum by (id) (irate(cpu_branch_instructions[5m]))"
                .to_string(),
        );
    } else {
        miss.plot_promql_full(
            PlotOpts::counter("Misprediction Rate %", "branch-miss-rate", Unit::Percentage)
                .percentage_range(),
            "sum(irate(cpu_branch_misses[5m])) / sum(irate(cpu_branch_instructions[5m]))"
                .to_string(),
        );
    }

    let activity = branch.subgroup("Branch Activity");
    activity.describe("Absolute branch instruction and miss rates.");
    activity.plot_promql(
        PlotOpts::counter("Instructions", "branch-instructions", Unit::Rate),
        "sum(irate(cpu_branch_instructions[5m]))".to_string(),
    );
    if multi_cpu {
        activity.plot_promql(
            PlotOpts::counter(
                "Instructions (Per-CPU)",
                "branch-instructions-per-cpu",
                Unit::Rate,
            ),
            "sum by (id) (irate(cpu_branch_instructions[5m]))".to_string(),
        );
    }
    activity.plot_promql(
        PlotOpts::counter("Misses", "branch-misses", Unit::Rate),
        "sum(irate(cpu_branch_misses[5m]))".to_string(),
    );
    if multi_cpu {
        activity.plot_promql(
            PlotOpts::counter("Misses (Per-CPU)", "branch-misses-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_branch_misses[5m]))".to_string(),
        );
    }

    view.group(branch);

    // cpu_dtlb_miss aggregates all variants: unlabeled (AMD/ARM combined),
    // op="load" (Intel), op="store" (Intel).
    let mut dtlb = Group::new("DTLB", "dtlb");

    let misses = dtlb.subgroup("DTLB Misses");
    misses.describe("Raw data-TLB miss rate, aggregated and per-core.");
    if multi_cpu {
        misses.plot_promql(
            PlotOpts::counter("Misses", "dtlb-misses", Unit::Rate),
            "sum(irate(cpu_dtlb_miss[5m]))".to_string(),
        );
        misses.plot_promql(
            PlotOpts::counter("Misses (Per-CPU)", "dtlb-misses-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_dtlb_miss[5m]))".to_string(),
        );
    } else {
        misses.plot_promql_full(
            PlotOpts::counter("Misses", "dtlb-misses", Unit::Rate),
            "sum(irate(cpu_dtlb_miss[5m]))".to_string(),
        );
    }

    let mpki = dtlb.subgroup("DTLB MPKI");
    mpki.describe("Misses per thousand instructions, normalized so workload differences don't distort the rate.");
    if multi_cpu {
        mpki.plot_promql(
            PlotOpts::counter("MPKI", "dtlb-mpki", Unit::Count),
            "sum(irate(cpu_dtlb_miss[5m])) / sum(irate(cpu_instructions[5m])) * 1000".to_string(),
        );
        mpki.plot_promql(
            PlotOpts::counter("MPKI (Per-CPU)", "dtlb-mpki-per-cpu", Unit::Count),
            "sum by (id) (irate(cpu_dtlb_miss[5m])) / sum by (id) (irate(cpu_instructions[5m])) * 1000"
                .to_string(),
        );
    } else {
        mpki.plot_promql_full(
            PlotOpts::counter("MPKI", "dtlb-mpki", Unit::Count),
            "sum(irate(cpu_dtlb_miss[5m])) / sum(irate(cpu_instructions[5m])) * 1000".to_string(),
        );
    }

    view.group(dtlb);

    let mut migrations = Group::new("Migrations", "migrations");

    let to = migrations.subgroup("Incoming Migrations");
    to.describe("Tasks migrated onto a CPU, per second.");
    if multi_cpu {
        to.plot_promql(
            PlotOpts::counter("To", "cpu-migrations-to", Unit::Rate),
            "sum(irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
        );
        to.plot_promql(
            PlotOpts::counter("To (Per-CPU)", "cpu-migrations-to-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
        );
    } else {
        to.plot_promql_full(
            PlotOpts::counter("To", "cpu-migrations-to", Unit::Rate),
            "sum(irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
        );
    }

    let from = migrations.subgroup("Outgoing Migrations");
    from.describe("Tasks migrated off a CPU, per second.");
    if multi_cpu {
        from.plot_promql(
            PlotOpts::counter("From", "cpu-migrations-from", Unit::Rate),
            "sum(irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
        );
        from.plot_promql(
            PlotOpts::counter("From (Per-CPU)", "cpu-migrations-from-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
        );
    } else {
        from.plot_promql_full(
            PlotOpts::counter("From", "cpu-migrations-from", Unit::Rate),
            "sum(irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
        );
    }

    view.group(migrations);

    let mut tlb = Group::new("TLB Flush", "tlb-flush");

    let total = tlb.subgroup("Total TLB Flushes");
    total.describe("Aggregate TLB invalidation rate across all reasons.");
    if multi_cpu {
        total.plot_promql(
            PlotOpts::counter("Total", "tlb-total", Unit::Rate),
            "sum(irate(cpu_tlb_flush[5m]))".to_string(),
        );
        total.plot_promql(
            PlotOpts::counter("Total (Per-CPU)", "tlb-total-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_tlb_flush[5m]))".to_string(),
        );
    } else {
        total.plot_promql_full(
            PlotOpts::counter("Total", "tlb-total", Unit::Rate),
            "sum(irate(cpu_tlb_flush[5m]))".to_string(),
        );
    }

    for reason in &[
        ("local_mm_shootdown", "Local MM Shootdown"),
        ("remote_send_ipi", "Remote Send IPI"),
        ("remote_shootdown", "Remote Shootdown"),
        ("task_switch", "Task Switch"),
    ] {
        let (reason_value, label) = reason;
        let id = format!("tlb-{}", reason_value.replace('_', "-"));
        let sg = tlb.subgroup(*label);
        if multi_cpu {
            sg.plot_promql(
                PlotOpts::counter(*label, &id, Unit::Rate),
                format!("sum(irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
            );
            sg.plot_promql(
                PlotOpts::counter(
                    format!("{label} (Per-CPU)"),
                    format!("{id}-per-cpu"),
                    Unit::Rate,
                ),
                format!("sum by (id) (irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
            );
        } else {
            sg.plot_promql_full(
                PlotOpts::counter(*label, &id, Unit::Rate),
                format!("sum(irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
            );
        }
    }

    view.group(tlb);

    // Power and energy from the cpu_power sampler. Which domains exist is
    // hardware-dependent — Intel exposes `cpu_cores_energy` per package, AMD
    // exposes `cpu_core_energy` per core, and DRAM/platform/iGPU are present
    // only on parts that implement them — so every subgroup is gated on the
    // metric actually appearing in the recording.
    //
    // Power is derived from the energy counters rather than read from the
    // `cpu_*_power` gauges. The gauges are averages over the sampler's own
    // refresh interval, so a scrape landing mid-interval returns a stale
    // value; `irate` over the monotonic energy counter is correct for any
    // window. The counters are microjoules, so irate() gives uJ/s (= uW) and
    // the /1e6 converts to watts.
    let mut power = Group::new("Power", "power");

    let domains: &[(&str, &str, &str)] = &[
        ("cpu_package_energy", "Package", "package"),
        ("cpu_cores_energy", "Cores", "cores"),
        ("cpu_core_energy", "Core", "core"),
        ("cpu_igpu_energy", "Integrated Graphics", "igpu"),
        ("cpu_dram_energy", "DRAM", "dram"),
        ("cpu_platform_energy", "Platform", "platform"),
    ];

    // Each domain gets a half-width aggregate plot, with the per-id breakdown
    // beside it as a heatmap.
    //
    // The per-id plot is only worth emitting when the domain actually has more
    // than one series. The frontend's resolveStyle() only picks 'heatmap' for a
    // multi-series result and otherwise falls back to a line, so on a
    // single-package host a per-id plot would draw the same line as the
    // aggregate next to it. Package-scope domains have one series per socket,
    // so they pair up only on multi-socket machines; core-scope domains (AMD's
    // energy-core) always do.
    for (metric, label, id) in domains {
        if !data.has_counter(metric) {
            continue;
        }

        let sg = power.subgroup(format!("{label} Power"));
        sg.describe(format!(
            "Power drawn by the {} domain, derived from its cumulative energy counter.",
            label.to_lowercase()
        ));

        let per_id = metric_unique_label_count(data, metric, "id") > 1;

        sg.plot_promql(
            PlotOpts::counter(format!("{label} Power"), format!("{id}-power"), Unit::Power),
            format!("sum(irate({metric}[5m])) / 1000000"),
        );

        if per_id {
            sg.plot_promql(
                PlotOpts::counter(
                    format!("{label} Power (Per-ID)"),
                    format!("{id}-power-heatmap"),
                    Unit::Power,
                ),
                format!("sum by (id) (irate({metric}[5m])) / 1000000"),
            );
        }
    }

    if !power.is_empty() {
        view.group(power);
    }

    // Idle-state residency from the cstate_core/cstate_pkg PMUs. Counters tick
    // in TSC cycles, so dividing by cpu_tsc gives the residency fraction. Only
    // the levels a part implements are recorded, so each is gated.
    let mut cstate = Group::new("C-State Residency", "cstate");

    if data.has_counter("core_cstate_residency") {
        let total = cstate.subgroup("Total C-State Residency");
        total.describe(
            "Fraction of time cores spent in any idle C-state, summed across every level the hardware exposes.",
        );
        total.plot_promql(
            PlotOpts::counter("Idle %", "cstate-total", Unit::Percentage).percentage_range(),
            "sum(irate(core_cstate_residency[5m])) / sum(irate(cpu_tsc[5m]))".to_string(),
        );
        if metric_unique_label_count(data, "core_cstate_residency", "id") > 1 {
            total.plot_promql(
                PlotOpts::counter(
                    "Idle % (Per-Core)",
                    "cstate-total-per-core",
                    Unit::Percentage,
                )
                .percentage_range(),
                "sum by (id) (irate(core_cstate_residency[5m])) / sum by (id) (irate(cpu_tsc[5m]))"
                    .to_string(),
            );
        }
    }

    for level in [1u8, 2, 3, 6, 7, 8, 9, 10] {
        let metric = format!("core_c{level}_residency");
        if !data.has_counter(&metric) {
            continue;
        }
        let sg = cstate.subgroup(format!("Core C{level}"));
        sg.describe(format!(
            "Fraction of time cores spent in the C{level} idle state."
        ));
        sg.plot_promql(
            PlotOpts::counter(
                format!("C{level} %"),
                format!("cstate-core-c{level}"),
                Unit::Percentage,
            )
            .percentage_range(),
            format!("sum(irate({metric}[5m])) / sum(irate(cpu_tsc[5m]))"),
        );
        if metric_unique_label_count(data, &metric, "id") > 1 {
            sg.plot_promql(
                PlotOpts::counter(
                    format!("C{level} % (Per-Core)"),
                    format!("cstate-core-c{level}-per-core"),
                    Unit::Percentage,
                )
                .percentage_range(),
                format!("sum by (id) (irate({metric}[5m])) / sum by (id) (irate(cpu_tsc[5m]))"),
            );
        }
    }

    for level in [1u8, 2, 3, 6, 7, 8, 9, 10] {
        let metric = format!("package_c{level}_residency");
        if !data.has_counter(&metric) {
            continue;
        }
        let sg = cstate.subgroup(format!("Package C{level}"));
        sg.describe(format!(
            "Fraction of time the package spent in the C{level} idle state."
        ));
        sg.plot_promql_full(
            PlotOpts::counter(
                format!("Package C{level} %"),
                format!("cstate-pkg-c{level}"),
                Unit::Percentage,
            )
            .percentage_range(),
            format!("sum(irate({metric}[5m])) / sum(irate(cpu_tsc[5m]))"),
        );
    }

    if !cstate.is_empty() {
        view.group(cstate);
    }

    view
}
