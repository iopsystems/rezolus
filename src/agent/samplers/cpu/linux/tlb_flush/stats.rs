use metriken::*;

use crate::common::*;

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
