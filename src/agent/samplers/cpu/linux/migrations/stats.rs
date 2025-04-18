use metriken::*;

use crate::agent::*;

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