use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CPUS;
use linkme::distributed_slice;

// `cpu::stats::usage::CPU_USAGE_ACQ` is also compiled on macOS, but its
// sampler is `"unattributed"`, so it never matches these metrics, which
// attribute to `cpu_usage`. This group brackets the single
// `host_processor_info` call; this sampler's `refresh()` is its only writer.
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
