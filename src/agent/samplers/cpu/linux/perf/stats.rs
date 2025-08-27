use metriken::*;

use crate::agent::*;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "cpu_perf"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "cpu_perf"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "cpu_cycles",
    description = "The number of elapsed CPU cycles",
    metadata = { unit = "cycles" }
)]
pub static CPU_CYCLES: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_instructions",
    description = "The number of instructions retired",
    metadata = { unit = "instructions" }
)]
pub static CPU_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CPUS);

/*
 * per-cgroup
 */

#[metric(
    name = "cgroup_cpu_cycles",
    description = "The number of elapsed CPU cycles on a per-cgroup basis",
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_CYCLES: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_instructions",
    description = "The number of instructions retired on a per-cgroup basis",
    metadata = { unit = "instructions" }
)]
pub static CGROUP_CPU_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CGROUPS);
