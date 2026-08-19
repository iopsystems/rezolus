use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_CPUS};
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `cpu/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only. A metric declaring `acq_group = "migrations"` must find its
// group registered on every platform that compiles this file, not just the
// one that actually drives it.
//
/// Brackets the `migrations` cpu_counters refresh (single writer: this
/// sampler's own BPF refresh path).
pub static MIGRATIONS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_migrations"),
    "migrations",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static MIGRATIONS_ACQ_REG: &'static AcquisitionGroup = &MIGRATIONS_ACQ;

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
    metadata = { direction = "from", acq_group = "migrations" }
)]
pub static CPU_MIGRATIONS_FROM: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_migrations",
    description = "The number of process CPU migrations",
    metadata = { direction = "to", acq_group = "migrations" }
)]
pub static CPU_MIGRATIONS_TO: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cgroup_cpu_migrations",
    description = "The number of times a process in a cgroup migrated from one CPU to another"
)]
pub static CGROUP_CPU_MIGRATIONS: CounterGroup = CounterGroup::new(MAX_CGROUPS);
