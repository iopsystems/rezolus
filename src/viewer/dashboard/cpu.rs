use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Utilization
     */

    let mut utilization = Group::new("Utilization", "utilization");

    utilization.push(Plot::line(
        "Busy %",
        "busy-pct",
        Unit::Percentage,
        data.cpu_avg("cpu_usage", ()).map(|v| (v / 1000000000.0)),
    ));

    utilization.push(Plot::heatmap(
        "Busy %",
        "busy-pct-heatmap",
        Unit::Percentage,
        data.cpu_heatmap("cpu_usage", ()).map(|v| v / 1000000000.0),
    ));

    for state in &["User", "System"] {
        utilization.push(Plot::line(
            format!("{state} %"),
            format!("{}-pct", state.to_lowercase()),
            Unit::Percentage,
            data.cpu_avg("cpu_usage", [("state", state.to_lowercase())])
                .map(|v| (v / 1000000000.0)),
        ));

        utilization.push(Plot::heatmap(
            format!("{state} %"),
            format!("{}-pct-heatmap", state.to_lowercase()),
            Unit::Percentage,
            data.cpu_heatmap("cpu_usage", [("state", state.to_lowercase())])
                .map(|v| (v / 1000000000.0)),
        ));
    }

    view.group(utilization);

    /*
     * Performance
     */

    let mut performance = Group::new("Performance", "performance");

    if let (Some(cycles), Some(instructions)) = (
        data.counters("cpu_cycles", ()).map(|v| v.rate().sum()),
        data.counters("cpu_instructions", ())
            .map(|v| v.rate().sum()),
    ) {
        let ipc = instructions / cycles;
        performance.plot(
            PlotOpts::line("Instructions per Cycle (IPC)", "ipc", Unit::Count),
            Some(ipc),
        );
    }

    if let (Some(cycles), Some(instructions)) = (
        data.cpu_heatmap("cpu_cycles", ()),
        data.cpu_heatmap("cpu_instructions", ()),
    ) {
        let ipc = instructions / cycles;
        performance.heatmap(
            PlotOpts::heatmap("Instructions per Cycle (IPC)", "ipc-heatmap", Unit::Count),
            Some(ipc),
        );
    }

    if let (Some(cycles), Some(instructions), Some(aperf), Some(mperf), Some(tsc), Some(cores)) = (
        data.counters("cpu_cycles", ()).map(|v| v.rate().sum()),
        data.counters("cpu_instructions", ())
            .map(|v| v.rate().sum()),
        data.counters("cpu_aperf", ()).map(|v| v.rate().sum()),
        data.counters("cpu_mperf", ()).map(|v| v.rate().sum()),
        data.counters("cpu_tsc", ()).map(|v| v.rate().sum()),
        data.gauges("cpu_cores", ()).map(|v| v.sum()),
    ) {
        let ipns = instructions / cycles * tsc * aperf / mperf / 1000000000.0 / cores;
        performance.plot(
            PlotOpts::line("Instructions per Nanosecond (IPNS)", "ipns", Unit::Count),
            Some(ipns),
        );
    }

    if let (Some(cycles), Some(instructions), Some(aperf), Some(mperf), Some(tsc)) = (
        data.cpu_heatmap("cpu_cycles", ()),
        data.cpu_heatmap("cpu_instructions", ()),
        data.cpu_heatmap("cpu_aperf", ()),
        data.cpu_heatmap("cpu_mperf", ()),
        data.cpu_heatmap("cpu_tsc", ()),
    ) {
        let ipns = instructions / cycles * tsc * aperf / mperf / 1000000000.0;
        performance.heatmap(
            PlotOpts::heatmap(
                "Instructions per Nanosecond (IPNS)",
                "ipns-heatmap",
                Unit::Count,
            ),
            Some(ipns),
        );
    }

    if let (Some(access), Some(miss)) = (
        data.counters("cpu_l3_access", ()).map(|v| v.rate().sum()),
        data.counters("cpu_l3_miss", ()).map(|v| v.rate().sum()),
    ) {
        let hitrate = miss / access;
        performance.plot(
            PlotOpts::line("L3 Hit %", "ld-hit", Unit::Percentage),
            Some(hitrate),
        );
    }

    if let (Some(access), Some(miss)) = (
        data.cpu_heatmap("cpu_l3_access", ()),
        data.cpu_heatmap("cpu_l3_miss", ()),
    ) {
        let hitrate = miss / access;
        performance.heatmap(
            PlotOpts::heatmap("L3 Hit %", "l3-hit-heatmap", Unit::Percentage),
            Some(hitrate),
        );
    }

    if let (Some(aperf), Some(mperf), Some(tsc), Some(cores)) = (
        data.counters("cpu_aperf", ()).map(|v| v.rate().sum()),
        data.counters("cpu_mperf", ()).map(|v| v.rate().sum()),
        data.counters("cpu_tsc", ()).map(|v| v.rate().sum()),
        data.gauges("cpu_cores", ()).map(|v| v.sum()),
    ) {
        let frequency = tsc * aperf / mperf / cores;
        performance.plot(
            PlotOpts::line("Frequency", "frequency", Unit::Frequency),
            Some(frequency),
        );
    }

    if let (Some(aperf), Some(mperf), Some(tsc)) = (
        data.cpu_heatmap("cpu_aperf", ()),
        data.cpu_heatmap("cpu_mperf", ()),
        data.cpu_heatmap("cpu_tsc", ()),
    ) {
        let frequency = tsc * aperf / mperf;
        performance.heatmap(
            PlotOpts::heatmap("Frequency", "frequency-heatmap", Unit::Frequency)
                .with_unit_system("frequency"),
            Some(frequency),
        );
    }

    view.group(performance);

    /*
     * Migrations
     */

    let mut migrations = Group::new("Migrations", "migrations");

    migrations.plot(
        PlotOpts::line("To", "cpu-migrations-to", Unit::Rate),
        data.counters("cpu_migrations", [("direction", "to")])
            .map(|v| v.rate().sum()),
    );

    migrations.push(Plot::heatmap(
        "To",
        "cpu-migrations-to-heatmap",
        Unit::Rate,
        data.cpu_heatmap("cpu_migrations", [("direction", "to")]),
    ));

    migrations.plot(
        PlotOpts::line("From", "cpu-migrations-from", Unit::Rate),
        data.counters("cpu_migrations", [("direction", "from")])
            .map(|v| v.rate().sum()),
    );

    migrations.push(Plot::heatmap(
        "From",
        "cpu-migrations-from-heatmap",
        Unit::Rate,
        data.cpu_heatmap("cpu_migrations", [("direction", "from")]),
    ));

    view.group(migrations);

    /*
     * TLB Flush
     */

    let mut tlb = Group::new("TLB Flush", "tlb-flush");

    tlb.plot(
        PlotOpts::line("Total", "tlb-total", Unit::Rate),
        data.counters("cpu_tlb_flush", ()).map(|v| v.rate().sum()),
    );

    tlb.heatmap(
        PlotOpts::heatmap("Total", "tlb-total-heatmap", Unit::Rate),
        data.cpu_heatmap("cpu_tlb_flush", ()),
    );

    for reason in &[
        "Local MM Shootdown",
        "Remote Send IPI",
        "Remote Shootdown",
        "Task Switch",
    ] {
        let label = reason;
        let id = format!(
            "tlb-{}",
            reason
                .to_lowercase()
                .split(' ')
                .collect::<Vec<&str>>()
                .join("-")
        );
        let reason = reason
            .to_lowercase()
            .split(' ')
            .collect::<Vec<&str>>()
            .join("_");

        tlb.plot(
            PlotOpts::line(*label, &id, Unit::Rate),
            data.counters("cpu_tlb_flush", [("reason", reason.clone())])
                .map(|v| v.rate().sum()),
        );

        tlb.heatmap(
            PlotOpts::heatmap(*label, format!("{id}-heatmap"), Unit::Rate),
            data.cpu_heatmap("cpu_tlb_flush", [("reason", reason)]),
        );
    }

    view.group(tlb);

    view
}
