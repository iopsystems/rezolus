use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CGROUPS;
use linkme::distributed_slice;

// Reader-stamped (mmap-direct `PackedCounters`) groups, one per distinct
// metric family (principle 18's like-entities rule: these 5 are 5
// different metric NAMES, not label-distinguished instances of one family
// — unlike `cpu_tlb_flush`'s cgroup breakdown, see its stats.rs) — see
// `docs/principles.md` principle 18 and
// `crate::agent::timing::AcquisitionGroup::set_reader_stamped`.
pub static CGROUP_THROTTLED_TIME_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("cpu_bandwidth"),
    "cpu_bandwidth_cgroup_throttled_time",
);
pub static CGROUP_THROTTLED_COUNT_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("cpu_bandwidth"),
    "cpu_bandwidth_cgroup_throttled_count",
);
pub static CGROUP_BANDWIDTH_PERIODS_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("cpu_bandwidth"),
    "cpu_bandwidth_cgroup_bandwidth_periods",
);
pub static CGROUP_BANDWIDTH_THROTTLED_PERIODS_ACQ: AcquisitionGroup =
    AcquisitionGroup::new_reader_stamped(
        crate::agent::samplers::bpf_sampler_name("cpu_bandwidth"),
        "cpu_bandwidth_cgroup_bandwidth_throttled_periods",
    );
pub static CGROUP_BANDWIDTH_THROTTLED_TIME_ACQ: AcquisitionGroup =
    AcquisitionGroup::new_reader_stamped(
        crate::agent::samplers::bpf_sampler_name("cpu_bandwidth"),
        "cpu_bandwidth_cgroup_bandwidth_throttled_time",
    );

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_THROTTLED_TIME_ACQ_REG: &'static AcquisitionGroup = &CGROUP_THROTTLED_TIME_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_THROTTLED_COUNT_ACQ_REG: &'static AcquisitionGroup = &CGROUP_THROTTLED_COUNT_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_BANDWIDTH_PERIODS_ACQ_REG: &'static AcquisitionGroup = &CGROUP_BANDWIDTH_PERIODS_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_BANDWIDTH_THROTTLED_PERIODS_ACQ_REG: &'static AcquisitionGroup =
    &CGROUP_BANDWIDTH_THROTTLED_PERIODS_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_BANDWIDTH_THROTTLED_TIME_ACQ_REG: &'static AcquisitionGroup =
    &CGROUP_BANDWIDTH_THROTTLED_TIME_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "cpu_bandwidth"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "cpu_bandwidth"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * per-cgroup
 */

#[metric(
    name = "cgroup_cpu_bandwidth_quota",
    description = "The CPU bandwidth quota assigned to the cgroup in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static CGROUP_CPU_BANDWIDTH_QUOTA: GaugeGroup = GaugeGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_bandwidth_period_duration",
    description = "The duration of the CFS bandwidth period in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static CGROUP_CPU_BANDWIDTH_PERIOD_DURATION: GaugeGroup = GaugeGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_throttled_time",
    description = "The total time all runqueues in a cgroup throttled by the CPU controller in nanoseconds",
    metadata = { unit = "nanoseconds", acq_group = "cpu_bandwidth_cgroup_throttled_time" }
)]
pub static CGROUP_CPU_THROTTLED_TIME: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_throttled",
    description = "The number of times all runqueues in a cgroup throttled by the CPU controller",
    metadata = { unit = "events", acq_group = "cpu_bandwidth_cgroup_throttled_count" }
)]
pub static CGROUP_CPU_THROTTLED: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_bandwidth_periods",
    description = "The total number of periods in a cgroup with the CPU bandwidth set",
    metadata = { unit = "events", acq_group = "cpu_bandwidth_cgroup_bandwidth_periods" }
)]
pub static CGROUP_CPU_BANDWIDTH_PERIODS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_bandwidth_throttled_periods",
    description = "The total number of throttled periods in a cgroup with the CPU bandwidth set",
    metadata = { unit = "events", acq_group = "cpu_bandwidth_cgroup_bandwidth_throttled_periods" }
)]
pub static CGROUP_CPU_BANDWIDTH_THROTTLED_PERIODS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_bandwidth_throttled_time",
    description = "The total throttled time of all runqueues in a cgroup read from the cgroup cfs_bandwidth statistics",
    metadata = { unit = "nanoseconds", acq_group = "cpu_bandwidth_cgroup_bandwidth_throttled_time" }
)]
pub static CGROUP_CPU_BANDWIDTH_THROTTLED_TIME: CounterGroup = CounterGroup::new(MAX_CGROUPS);
