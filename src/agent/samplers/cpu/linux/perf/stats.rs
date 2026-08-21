use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_CPUS};
use linkme::distributed_slice;

/// Brackets the per-CPU perf-event sweep for the base (non-cgroup)
/// `cpu_cycles`/`cpu_instructions` counters — `AsyncBpf::refresh`'s
/// perf-thread phase (`.perf_event()` registrations; see
/// `crate::agent::bpf::builder::Builder::perf_event`'s doc comment and
/// `bpf/mod.rs`'s `AsyncBpf::refresh`). Single writer: only `AsyncBpf`'s
/// own `refresh()` task calls `acquire()`/`finish()` — the sampler
/// scheduler dispatches it serially, and the perf-read thread(s) it
/// triggers only ever `set()` values, never touch the group. One group for
/// both metrics: `CpuPerfCounters::refresh` reads cycles then instructions
/// for one CPU in a single uninterrupted loop (both `.perf_event()` calls
/// merge into the same per-CPU `PerfCounters` entry — see
/// `Builder::perf_event`), with no phase boundary between the two — the
/// same "read together, no gap" shape as `cpu_dtlb`/`cpu_l3`/
/// `cpu_frequency`, not `cpu_usage`'s three-separate-BPF-map case.
pub static CPU_PERF_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_perf"),
    "cpu_perf_sweep",
);

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
static CPU_PERF_ACQ_REG: &'static AcquisitionGroup = &CPU_PERF_ACQ;
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
    metadata = { unit = "cycles", acq_group = "cpu_perf_sweep" }
)]
pub static CPU_CYCLES: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_instructions",
    description = "The number of instructions retired",
    metadata = { unit = "instructions", acq_group = "cpu_perf_sweep" }
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
