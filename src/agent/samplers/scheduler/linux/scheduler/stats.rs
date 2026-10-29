use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_CPUS};
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `scheduler/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
/// Brackets the `counters` cpu_counters refresh (single writer: this
/// sampler's own BPF refresh path).
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

/// One group per histogram map read (each `Histogram` reads exactly one
/// BPF map per refresh, so there is no multi-map section to share).
pub static RUNQLAT_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_runqlat",
);
pub static RUNNING_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_running",
);
pub static OFFCPU_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_offcpu",
);
pub static UNINTERRUPTIBLE_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_uninterruptible",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static RUNQLAT_ACQ_REG: &'static AcquisitionGroup = &RUNQLAT_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static RUNNING_ACQ_REG: &'static AcquisitionGroup = &RUNNING_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static OFFCPU_ACQ_REG: &'static AcquisitionGroup = &OFFCPU_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static UNINTERRUPTIBLE_ACQ_REG: &'static AcquisitionGroup = &UNINTERRUPTIBLE_ACQ;

// Per-cgroup (`CgroupCounters`) groups for the per-cgroup
// breakdown — see `docs/principles.md` principle 18 and
// `crate::agent::timing::AcquisitionGroup::new_slotted`.
// `CGROUP_SCHEDULER_IVCSW`/`CGROUP_SCHEDULER_VCSW` share ONE group: both
// are the `cgroup_scheduler_context_switch` family (distinguished by the
// `kind` label) — like-entities collapse per principle 18.
// `cgroup_scheduler_runqueue_wait`, `cgroup_scheduler_offcpu` and
// `cgroup_scheduler_uninterruptible_wait` are distinct families and keep their
// own groups.
pub static CGROUP_WAIT_ACQ: AcquisitionGroup = AcquisitionGroup::new_slotted(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_cgroup_wait",
);
pub static CGROUP_OFFCPU_ACQ: AcquisitionGroup = AcquisitionGroup::new_slotted(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_cgroup_offcpu",
);
pub static CGROUP_UNINTERRUPTIBLE_ACQ: AcquisitionGroup = AcquisitionGroup::new_slotted(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_cgroup_uninterruptible",
);
pub static CGROUP_CONTEXT_SWITCH_ACQ: AcquisitionGroup = AcquisitionGroup::new_slotted(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_runqueue_cgroup_context_switch",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_WAIT_ACQ_REG: &'static AcquisitionGroup = &CGROUP_WAIT_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_OFFCPU_ACQ_REG: &'static AcquisitionGroup = &CGROUP_OFFCPU_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_UNINTERRUPTIBLE_ACQ_REG: &'static AcquisitionGroup = &CGROUP_UNINTERRUPTIBLE_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_CONTEXT_SWITCH_ACQ_REG: &'static AcquisitionGroup = &CGROUP_CONTEXT_SWITCH_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "scheduler"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "scheduler"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "scheduler_runqueue_latency",
    description = "Distribution of the amount of time tasks were waiting in the runqueue",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_runqlat" }
)]
pub static SCHEDULER_RUNQUEUE_LATENCY: RwLockCounterHistogram =
    RwLockCounterHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_running",
    description = "Distribution of the amount of time tasks were on-CPU",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_running" }
)]
pub static SCHEDULER_RUNNING: RwLockCounterHistogram =
    RwLockCounterHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_offcpu",
    description = "Distribution of the amount of time tasks were off-CPU",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_offcpu" }
)]
pub static SCHEDULER_OFFCPU: RwLockCounterHistogram =
    RwLockCounterHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_uninterruptible",
    description = "Distribution of the amount of time tasks were off-CPU in uninterruptible sleep (D state), a subset of scheduler_offcpu. Idle kernel threads (TASK_IDLE) are excluded, as in the load average",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_uninterruptible" }
)]
pub static SCHEDULER_UNINTERRUPTIBLE: RwLockCounterHistogram =
    RwLockCounterHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_context_switch",
    description = "The number of involuntary context switches, where a runnable task was preempted. Switches away from the idle task are excluded, since nothing was competing for the CPU",
    metadata = { kind = "involuntary", acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_IVCSW: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "scheduler_context_switch",
    description = "The number of voluntary context switches, where a task left the CPU because it blocked",
    metadata = { kind = "voluntary", acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_VCSW: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "scheduler_runqueue_wait",
    description = "Tracks time spent in the runqueue on a per-CPU basis",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_RUNQUEUE_WAIT: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "scheduler_uninterruptible_wait",
    description = "Time tasks spent in uninterruptible sleep (D state), added in full when a task next runs, on the CPU it runs on. Over windows much longer than the sleeps, the rate summed across CPUs is the mean number of tasks in D state; a long sleep shows as a spike when it ends",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_UNINTERRUPTIBLE_WAIT: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "scheduler_discarded_samples",
    description = "The number of scheduler timing samples discarded because the two timestamps arrived out of order across CPUs, which would otherwise underflow into the top histogram bucket",
    metadata = { acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_DISCARDED: CounterGroup = CounterGroup::new(MAX_CPUS);

/*
 * per-cgroup
 */

#[metric(
    name = "cgroup_scheduler_runqueue_wait",
    description = "Tracks time spent in the runqueue on a per-cgroup basis",
    metadata = { state = "wait", unit = "nanoseconds", acq_group = "scheduler_runqueue_cgroup_wait" }
)]
pub static CGROUP_SCHEDULER_RUNQUEUE_WAIT: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_scheduler_offcpu",
    description = "Tracks the time when tasks were off-CPU on a per-cgroup basis",
    metadata = { state = "offcpu", unit = "nanoseconds", acq_group = "scheduler_runqueue_cgroup_offcpu" }
)]
pub static CGROUP_SCHEDULER_OFFCPU: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_scheduler_uninterruptible_wait",
    description = "Time tasks spent in uninterruptible sleep (D state) on a per-cgroup basis, added in full when a task next runs",
    metadata = { state = "uninterruptible", unit = "nanoseconds", acq_group = "scheduler_runqueue_cgroup_uninterruptible" }
)]
pub static CGROUP_SCHEDULER_UNINTERRUPTIBLE_WAIT: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_scheduler_context_switch",
    description = "The number of involuntary context switches on a per-cgroup basis, where a runnable task was preempted. Switches away from the idle task are excluded, since nothing was competing for the CPU",
    metadata = { kind = "involuntary", acq_group = "scheduler_runqueue_cgroup_context_switch" }
)]
pub static CGROUP_SCHEDULER_IVCSW: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_scheduler_context_switch",
    description = "The number of voluntary context switches on a per-cgroup basis, where a task left the CPU because it blocked",
    metadata = { kind = "voluntary", acq_group = "scheduler_runqueue_cgroup_context_switch" }
)]
pub static CGROUP_SCHEDULER_VCSW: CounterGroup = CounterGroup::new(MAX_CGROUPS);

// The `scheduler` sampler replaced `scheduler_runqueue` and `cpu_migrations`.
// The runqueue groups keep their names; the migration groups are renamed from
// `cpu_migrations_*`, since a group's name starts with its sampler's.
/// Brackets the `migrations` cpu_counters refresh (single writer: this
/// sampler's own BPF refresh path).
pub static MIGRATIONS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_migrations",
);

// Per-cgroup (`CgroupCounters`) group for the per-cgroup
// migration count — see `docs/principles.md` principle 18 and
// `crate::agent::timing::AcquisitionGroup::new_slotted`.
pub static CGROUP_MIGRATIONS_ACQ: AcquisitionGroup = AcquisitionGroup::new_slotted(
    crate::agent::samplers::bpf_sampler_name("scheduler"),
    "scheduler_cgroup_migrations",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static MIGRATIONS_ACQ_REG: &'static AcquisitionGroup = &MIGRATIONS_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_MIGRATIONS_ACQ_REG: &'static AcquisitionGroup = &CGROUP_MIGRATIONS_ACQ;

/*
 * migrations
 */

#[metric(
    name = "cpu_migrations",
    description = "The number of process CPU migrations",
    metadata = { direction = "from", acq_group = "scheduler_migrations" }
)]
pub static CPU_MIGRATIONS_FROM: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_migrations",
    description = "The number of process CPU migrations",
    metadata = { direction = "to", acq_group = "scheduler_migrations" }
)]
pub static CPU_MIGRATIONS_TO: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cgroup_cpu_migrations",
    description = "The number of times a process in a cgroup migrated from one CPU to another",
    metadata = { acq_group = "scheduler_cgroup_migrations" }
)]
pub static CGROUP_CPU_MIGRATIONS: CounterGroup = CounterGroup::new(MAX_CGROUPS);
