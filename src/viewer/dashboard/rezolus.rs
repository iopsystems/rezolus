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

    rezolus.plot(
        PlotOpts::line("Total BPF Overhead", "bpf-overhead", Unit::Count),
        data.counters(
            "rezolus_bpf_run_time",
            (),
        )
        .map(|v| v.rate().sum() / 1e9),
    );

    rezolus.multi(
        PlotOpts::multi("BPF Per-Sampler Overhead", "bpf-sampler-overhead", Unit::Count),
        data.counters(
            "rezolus_bpf_run_time",
            (),
        )
        .map(|v| v.rate().by_sampler() / 1e9).map(|v| v.top_n(20, average))
    );

    if let (Some(run_time), Some(run_count)) = (
        data.counters("rezolus_bpf_run_time", ())
            .map(|v| v.rate().by_sampler() / 1e9),
         data.counters("rezolus_bpf_run_count", ())
            .map(|v| v.rate().by_sampler() / 1e9),
    ) {
        rezolus.multi(
            PlotOpts::multi("BPF Per-Sampler Execution Time", "bpf-execution-time", Unit::Time),
            Some((run_time / run_count).top_n(20, average)),
        );
    }

    view.group(rezolus);

    view
}
