use metriken::*;

use crate::agent::*;

/*
 * bpf prog stats
 */

 #[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "cpu_migrations"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "cpu_migrations"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "cpu_migrations",
    description = "The number of process CPU migrations",
    metadata = { direction = "from" }
)]
pub static CPU_MIGRATIONS_FROM: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_migrations",
    description = "The number of process CPU migrations",
    metadata = { direction = "to" }
)]
pub static CPU_MIGRATIONS_TO: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cgroup_cpu_migrations",
    description = "The number of times a process in a cgroup migrated from one CPU to another"
)]
pub static CGROUP_CPU_MIGRATIONS: CounterGroup = CounterGroup::new(MAX_CGROUPS);
