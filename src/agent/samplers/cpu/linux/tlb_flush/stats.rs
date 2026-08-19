use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_CPUS};
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `cpu/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
/// Brackets the `events` cpu_counters refresh (single writer: this
/// sampler's own BPF refresh path).
pub static EVENTS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_tlb_flush"),
    "cpu_tlb_flush_events",
);

// Reader-stamped (mmap-direct `PackedCounters`) group for the per-cgroup
// tlb_flush breakdown — see `docs/principles.md` principle 18 and
// `crate::agent::timing::AcquisitionGroup::set_reader_stamped`. All five
// `CGROUP_TLB_FLUSH_*` counters below share this ONE group: they are all
// the `cgroup_cpu_tlb_flush` metric family (distinguished by the `reason`
// label, backed by five separate BPF maps) — like-entities collapse per
// principle 18, the same rule syscall_latency's 16 op-class histograms and
// this sampler's own per-CPU `TLB_FLUSH_*` breakdown (via `cpu_counters`,
// `EVENTS_ACQ` above) already apply.
pub static CGROUP_EVENTS_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("cpu_tlb_flush"),
    "cpu_tlb_flush_cgroup",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static EVENTS_ACQ_REG: &'static AcquisitionGroup = &EVENTS_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_EVENTS_ACQ_REG: &'static AcquisitionGroup = &CGROUP_EVENTS_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "cpu_tlb_flush"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "cpu_tlb_flush"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * per-cpu
 */

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "task_switch", acq_group = "cpu_tlb_flush_events" }
)]
pub static TLB_FLUSH_TASK_SWITCH: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "remote_shootdown", acq_group = "cpu_tlb_flush_events" }
)]
pub static TLB_FLUSH_REMOTE_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "local_shootdown", acq_group = "cpu_tlb_flush_events" }
)]
pub static TLB_FLUSH_LOCAL_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "local_mm_shootdown", acq_group = "cpu_tlb_flush_events" }
)]
pub static TLB_FLUSH_LOCAL_MM_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events",
    metadata = { reason = "remote_send_ipi", acq_group = "cpu_tlb_flush_events" }
)]
pub static TLB_FLUSH_REMOTE_SEND_IPI: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tlb_flush",
    description = "The number of tlb_flush events with unknown reason (e.g., ARM64 where reason breakdown is unavailable)",
    metadata = { reason = "unknown", acq_group = "cpu_tlb_flush_events" }
)]
pub static TLB_FLUSH_UNKNOWN: CounterGroup = CounterGroup::new(MAX_CPUS);

/*
 * per-cgroup
 */

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "task_switch", acq_group = "cpu_tlb_flush_cgroup" }
)]
pub static CGROUP_TLB_FLUSH_TASK_SWITCH: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "remote_shootdown", acq_group = "cpu_tlb_flush_cgroup" }
)]
pub static CGROUP_TLB_FLUSH_REMOTE_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "local_shootdown", acq_group = "cpu_tlb_flush_cgroup" }
)]
pub static CGROUP_TLB_FLUSH_LOCAL_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "local_mm_shootdown", acq_group = "cpu_tlb_flush_cgroup" }
)]
pub static CGROUP_TLB_FLUSH_LOCAL_MM_SHOOTDOWN: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tlb_flush",
    description = "The number of tlb_flush events on a per-cgroup basis",
    metadata = { reason = "remote_send_ipi", acq_group = "cpu_tlb_flush_cgroup" }
)]
pub static CGROUP_TLB_FLUSH_REMOTE_SEND_IPI: CounterGroup = CounterGroup::new(MAX_CGROUPS);
