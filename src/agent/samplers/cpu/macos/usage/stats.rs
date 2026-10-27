use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CPUS;
use linkme::distributed_slice;

// Its own group rather than `cpu::stats::usage::CPU_USAGE_ACQ`: that one is
// compiled here too, but resolves to the `"unattributed"` sampler off Linux
// and is bounded to zero members, so nothing set through it would be
// published. Brackets the single `host_processor_info` call (single writer:
// this sampler's `refresh()`).
pub static CPU_USAGE_ACQ: AcquisitionGroup = AcquisitionGroup::new("cpu_usage", "cpu_usage_cpu");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_USAGE_ACQ_REG: &'static AcquisitionGroup = &CPU_USAGE_ACQ;

#[metric(
    name = "cpu_cores",
    description = "The total number of logical cores that are currently online"
)]
pub static CPU_CORES: LazyGauge = LazyGauge::new(Gauge::default);

// Name, description and metadata match the Linux per-CPU series so the same
// queries (`sum by (id) (irate(cpu_usage[5m]))`) work on both. Niced time is
// counted as `user`, as on Linux, where the BPF sampler reads each task's
// `utime`, which includes it.

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent in each state",
    metadata = { state = "user", unit = "nanoseconds", acq_group = "cpu_usage_cpu" }
)]
pub static CPU_USAGE_USER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent in each state",
    metadata = { state = "system", unit = "nanoseconds", acq_group = "cpu_usage_cpu" }
)]
pub static CPU_USAGE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CPUS);
