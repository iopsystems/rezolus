use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Rezolus
     */

    let mut rezolus = Group::new("Rezolus", "rezolus");

    rezolus.plot(
        PlotOpts::line("CPU %", "cpu", Unit::Percentage),
        data.counters("rezolus_cpu_usage", ())
            .map(|v| v.rate().sum() / 1e9),
    );

    rezolus.plot(
        PlotOpts::line("Memory (RSS)", "memory", Unit::Bytes),
        data.gauges("rezolus_memory_usage_resident_set_size", ())
            .map(|v| v.sum()),
    );

    if let (Some(instructions), Some(cycles)) = (
        data.counters(
            "cgroup_cpu_instructions",
            [("name", "/system.slice/rezolus.service")],
        )
        .map(|v| v.rate().sum()),
        data.counters(
            "cgroup_cpu_cycles",
            [("name", "/system.slice/rezolus.service")],
        )
        .map(|v| v.rate().sum()),
    ) {
        rezolus.plot(
            PlotOpts::line("IPC", "ipc", Unit::Count),
            Some(instructions / cycles),
        );
    }

    rezolus.plot(
        PlotOpts::line("Syscalls", "syscalls", Unit::Rate),
        data.counters(
            "cgroup_syscall",
            [("name", "/system.slice/rezolus.service")],
        )
        .map(|v| v.rate().sum()),
    );

    view.group(rezolus);

    view
}
