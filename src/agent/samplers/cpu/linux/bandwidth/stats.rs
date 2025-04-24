use metriken::*;

use crate::agent::*;

#[metric(
    name = "cgroup_cpu_bandwidth_quota",
    description = "The CPU bandwidth quota assigned to the cgroup in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static CGROUP_CPU_BANDWIDTH_QUOTA: GaugeGroup = GaugeGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_bandwidth_period",
    description = "The duration of the CFS bandwidth period in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static CGROUP_CPU_BANDWIDTH_PERIOD: GaugeGroup = GaugeGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_throttled_time",
    description = "The total time a cgroup has been throttled by the CPU controller in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static CGROUP_CPU_THROTTLED_TIME: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_throttled",
    description = "The number of times a cgroup has been throttled by the CPU controller",
    metadata = { unit = "events" }
)]
pub static CGROUP_CPU_THROTTLED: CounterGroup = CounterGroup::new(MAX_CGROUPS);
