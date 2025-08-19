use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Rezolus
     */

    let mut rezolus = Group::new("Rezolus", "rezolus");

    // Rezolus CPU usage percentage
    rezolus.plot_promql(
        PlotOpts::line("CPU %", "cpu", Unit::Percentage),
        "sum(irate(rezolus_cpu_usage[5m])) / 1000000000".to_string(),
    );

    // Rezolus memory usage (RSS)
    rezolus.plot_promql(
        PlotOpts::line("Memory (RSS)", "memory", Unit::Bytes),
        "sum(rezolus_memory_usage_resident_set_size)".to_string(),
    );

    // IPC for rezolus.service cgroup
    rezolus.plot_promql(
        PlotOpts::line("IPC", "ipc", Unit::Count),
        "sum(irate(cgroup_cpu_instructions{name=\"/system.slice/rezolus.service\"}[5m])) / sum(irate(cgroup_cpu_cycles{name=\"/system.slice/rezolus.service\"}[5m]))".to_string(),
    );

    // Syscalls for rezolus.service cgroup
    rezolus.plot_promql(
        PlotOpts::line("Syscalls", "syscalls", Unit::Rate),
        "sum(irate(cgroup_syscall{name=\"/system.slice/rezolus.service\"}[5m]))".to_string(),
    );

    // Total BPF overhead
    rezolus.plot_promql(
        PlotOpts::line("Total BPF Overhead", "bpf-overhead", Unit::Count),
        "sum(irate(rezolus_bpf_run_time[5m])) / 1000000000".to_string(),
    );

    // BPF Per-Sampler Overhead
    // Using sum by (sampler) to group by sampler, then we get multiple series
    rezolus.plot_promql(
        PlotOpts::multi(
            "BPF Per-Sampler Overhead",
            "bpf-sampler-overhead",
            Unit::Count,
        ),
        "sum by (sampler) (irate(rezolus_bpf_run_time[5m])) / 1000000000".to_string(),
    );

    // BPF Per-Sampler Execution Time (run_time / run_count per sampler)
    rezolus.plot_promql(
        PlotOpts::multi(
            "BPF Per-Sampler Execution Time",
            "bpf-execution-time",
            Unit::Time,
        ),
        "(sum by (sampler) (irate(rezolus_bpf_run_time[5m])) / sum by (sampler) (irate(rezolus_bpf_run_count[5m]))) / 1000000000".to_string(),
    );

    view.group(rezolus);

    view
}
