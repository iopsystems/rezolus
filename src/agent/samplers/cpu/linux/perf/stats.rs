use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_CPUS};
use linkme::distributed_slice;

// Reader-stamped (mmap-direct `PackedCounters`) groups, one per distinct
// metric family — see `docs/principles.md` principle 18 and
// `crate::agent::timing::AcquisitionGroup::set_reader_stamped`.
pub static CGROUP_CYCLES_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("cpu_perf"),
    "cpu_perf_cgroup_cycles",
);
pub static CGROUP_INSTRUCTIONS_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("cpu_perf"),
    "cpu_perf_cgroup_instructions",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_CYCLES_ACQ_REG: &'static AcquisitionGroup = &CGROUP_CYCLES_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_INSTRUCTIONS_ACQ_REG: &'static AcquisitionGroup = &CGROUP_INSTRUCTIONS_ACQ;

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
    metadata = { unit = "cycles", acq_group = "cpu_perf_cgroup_cycles" }
)]
pub static CGROUP_CPU_CYCLES: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_instructions",
    description = "The number of instructions retired on a per-cgroup basis",
    metadata = { unit = "instructions", acq_group = "cpu_perf_cgroup_instructions" }
)]
pub static CGROUP_CPU_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CGROUPS);
