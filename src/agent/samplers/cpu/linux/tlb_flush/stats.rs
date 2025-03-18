use metriken::*;

use crate::agent::*;

// per-CPU metrics

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "task_switch" }
)]
pub static TLB_FLUSH_TASK_SWITCH: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "remote_shootdown" }
)]
pub static TLB_FLUSH_REMOTE_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "local_shootdown" }
)]
pub static TLB_FLUSH_LOCAL_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "local_mm_shootdown" }
)]
pub static TLB_FLUSH_LOCAL_MM_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "remote_send_ipi" }
)]
pub static TLB_FLUSH_REMOTE_SEND_IPI: CounterGroup = CounterGroup::new(MAX_CPUS);

// per-cgroup metrics

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "task_switch" }
)]
pub static CGROUP_TLB_FLUSH_TASK_SWITCH: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "remote_shootdown" }
)]
pub static CGROUP_TLB_FLUSH_REMOTE_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "local_shootdown" }
)]
pub static CGROUP_TLB_FLUSH_LOCAL_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "local_mm_shootdown" }
)]
pub static CGROUP_TLB_FLUSH_LOCAL_MM_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "remote_send_ipi" }
)]
pub static CGROUP_TLB_FLUSH_REMOTE_SEND_IPI: CounterGroup = CounterGroup::new(MAX_CGROUPS);
