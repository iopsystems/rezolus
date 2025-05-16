use metriken::*;

use crate::agent::*;

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
    metadata = { unit = "nanoseconds" }
)]
pub static CGROUP_CPU_THROTTLED_TIME: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_throttled",
    description = "The number of times all runqueues in a cgroup throttled by the CPU controller",
    metadata = { unit = "events" }
)]
pub static CGROUP_CPU_THROTTLED: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_bandwidth_periods",
    description = "The total number of periods in a cgroup with the CPU bandwidth set",
    metadata = { unit = "events" }
)]
pub static CGROUP_CPU_BANDWIDTH_PERIODS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_bandwidth_throttled_periods",
    description = "The total number of throttled periods in a cgroup with the CPU bandwidth set",
    metadata = { unit = "events" }
)]
pub static CGROUP_CPU_BANDWIDTH_THROTTLED_PERIODS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_bandwidth_throttled_time",
    description = "The total throttled time of all runqueues in a cgroup read from the cgroup cfs_bandwidth statistics",
    metadata = { unit = "nanoseconds" }
)]
pub static CGROUP_CPU_BANDWIDTH_THROTTLED_TIME: CounterGroup = CounterGroup::new(MAX_CGROUPS);
