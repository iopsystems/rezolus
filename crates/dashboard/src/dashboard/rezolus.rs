use crate::data::DashboardData;
use crate::plot::*;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Rezolus
     */

    let mut rezolus = Group::new("Rezolus", "rezolus");

    let resources = rezolus.subgroup("Resource Usage");
    resources.describe("CPU and memory consumed by the Rezolus agent itself.");
    resources.plot_promql(
        PlotOpts::counter("CPU %", "cpu", Unit::Percentage).percentage_range(),
        "sum(irate(rezolus_cpu_usage[5m])) / 1000000000".to_string(),
    );
    resources.plot_promql(
        PlotOpts::gauge("Memory (RSS)", "memory", Unit::Bytes),
        "sum(rezolus_memory_usage_resident_set_size)".to_string(),
    );

    let perf = rezolus.subgroup("Performance");
    perf.describe("Rezolus's own IPC and syscall rate, measured via the rezolus.service cgroup.");
    perf.plot_promql(
        PlotOpts::counter("IPC", "ipc", Unit::Count),
        "sum(irate(cgroup_cpu_instructions{name=\"/system.slice/rezolus.service\"}[5m])) / sum(irate(cgroup_cpu_cycles{name=\"/system.slice/rezolus.service\"}[5m]))".to_string(),
    );
    perf.plot_promql(
        PlotOpts::counter("Syscalls", "syscalls", Unit::Rate),
        "sum(irate(cgroup_syscall{name=\"/system.slice/rezolus.service\"}[5m]))".to_string(),
    );

    let bpf = rezolus.subgroup("BPF Overhead");
    bpf.describe("Time spent in BPF programs — total agent overhead and per-sampler breakdown.");
    bpf.plot_promql_full(
        PlotOpts::counter("Total BPF Overhead", "bpf-overhead", Unit::Count),
        "sum(irate(rezolus_bpf_run_time[5m])) / 1000000000".to_string(),
    );
    bpf.plot_promql(
        PlotOpts::counter(
            "BPF Per-Sampler Overhead",
            "bpf-sampler-overhead",
            Unit::Count,
        ),
        "sum by (sampler) (irate(rezolus_bpf_run_time[5m])) / 1000000000".to_string(),
    );
    bpf.plot_promql(
        PlotOpts::counter(
            "BPF Per-Sampler Execution Time",
            "bpf-execution-time",
            Unit::Time,
        ),
        "(sum by (sampler) (irate(rezolus_bpf_run_time[5m])) / sum by (sampler) (irate(rezolus_bpf_run_count[5m]))) / 1000000000".to_string(),
    );

    view.group(rezolus);

    view
}
