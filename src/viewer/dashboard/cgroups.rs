use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections.clone());

    /*
     * CPU
     */

    let mut cpu = Group::new("CPU", "cpu");

    cpu.multi(
        PlotOpts::multi("Total Cores", "cgroup-total-cores", Unit::Count),
        data.counters("cgroup_cpu_usage", ())
            .map(|v| (v.rate().by_name() / 1000000000.0).top_n(5, average)),
    );

    cpu.multi(
        PlotOpts::multi("User Cores", "cgroup-user-cores", Unit::Count),
        data.counters("cgroup_cpu_usage", [("state", "user")])
            .map(|v| (v.rate().by_name() / 1000000000.0).top_n(5, average)),
    );

    cpu.multi(
        PlotOpts::multi("System Cores", "cgroup-system-cores", Unit::Count),
        data.counters("cgroup_cpu_usage", [("state", "system")])
            .map(|v| (v.rate().by_name() / 1000000000.0).top_n(5, average)),
    );

    cpu.multi(
        PlotOpts::multi("CPU Migrations", "cgroup-cpu-migrations", Unit::Rate),
        data.counters("cgroup_cpu_migrations", ())
            .map(|v| (v.rate().by_name()).top_n(5, average)),
    );

    view.group(cpu);

    /*
     * Performance
     */

    let mut performance = Group::new("Performance", "performance");

    if let (Some(cycles), Some(instructions)) = (
        data.counters("cgroup_cpu_cycles", ())
            .map(|v| v.rate().by_name()),
        data.counters("cgroup_cpu_instructions", ())
            .map(|v| v.rate().by_name()),
    ) {
        performance.multi(
            PlotOpts::multi("Highest IPC", "cgroup-ipc-low", Unit::Count),
            Some((cycles.clone() / instructions.clone()).top_n(5, average)),
        );

        performance.multi(
            PlotOpts::multi("Lowest IPC", "cgroup-ipc-high", Unit::Count),
            Some((cycles / instructions).bottom_n(5, average)),
        );
    }

    view.group(performance);

    /*
     * TLB Flush
     */

    let mut tlb = Group::new("TLB", "tlb");

    tlb.multi(
        PlotOpts::multi("Total", "cgroup-tlb-flush", Unit::Count),
        data.counters("cgroup_tlb_flush", ())
            .map(|v| (v.rate().by_name()).top_n(5, average)),
    );

    view.group(tlb);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");

    syscall.multi(
        PlotOpts::multi("Total", "cgroup-syscall", Unit::Rate),
        data.counters("cgroup_syscall", ())
            .map(|v| (v.rate().by_name()).top_n(5, average)),
    );

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
        syscall.multi(
            PlotOpts::multi(*op, format!("syscall-{op}"), Unit::Rate),
            data.counters("cgroup_syscall", [("op", op.to_lowercase())])
                .map(|v| (v.rate().by_name()).top_n(5, average)),
        );
    }

    view.group(syscall);

    view
}
