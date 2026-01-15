//! Collects tlb flush event information using BPF and traces:
//! * `tlb_flush` (x86_64 tracepoint)
//! * `tlb_finish_mmu` (ARM64 kprobe fallback)
//!
//! And produces these stats:
//! * `cpu_tlb_flush`
//! * `cgroup_cpu_tlb_flush`
//!
//! These stats can be used to understand the reason for TLB flushes.
//!
//! ## Architecture Support
//!
//! - **x86_64**: Uses the `tlb_flush` tracepoint which provides detailed reason
//!   codes (task_switch, remote_shootdown, local_shootdown, etc.)
//!
//! - **ARM64**: Uses a kprobe on `tlb_finish_mmu` which is called after TLB
//!   batch operations. This provides basic TLB flush counting without reason
//!   breakdown (all flushes are counted as "unknown" reason).

const NAME: &str = "cpu_tlb_flush";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_tlb_flush.bpf.rs"));
}

use bpf::*;

use crate::agent::*;

use std::sync::Arc;

mod stats;

use stats::*;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

static CGROUP_METRICS: &[&dyn MetricGroup] = &[
    &CGROUP_TLB_FLUSH_TASK_SWITCH,
    &CGROUP_TLB_FLUSH_REMOTE_SHOOTDOWN,
    &CGROUP_TLB_FLUSH_LOCAL_SHOOTDOWN,
    &CGROUP_TLB_FLUSH_LOCAL_MM_SHOOTDOWN,
    &CGROUP_TLB_FLUSH_REMOTE_SEND_IPI,
];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, CGROUP_METRICS)
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Events vector includes all reason types
    // On x86_64: detailed reason counters from the tracepoint
    // On ARM64: only TLB_FLUSH_UNKNOWN is used (reason unavailable)
    let events = vec![
        &TLB_FLUSH_TASK_SWITCH,
        &TLB_FLUSH_REMOTE_SHOOTDOWN,
        &TLB_FLUSH_LOCAL_SHOOTDOWN,
        &TLB_FLUSH_LOCAL_MM_SHOOTDOWN,
        &TLB_FLUSH_REMOTE_SEND_IPI,
        &TLB_FLUSH_UNKNOWN,
    ];

    // Select the appropriate BPF program based on architecture
    // x86_64: use tlb_flush tracepoint (provides detailed reason codes)
    // ARM64: use tlb_finish_mmu kprobe (basic counting, no reason breakdown)
    #[cfg(target_arch = "x86_64")]
    let enabled_programs = &["tlb_flush"];

    #[cfg(target_arch = "aarch64")]
    let enabled_programs = &["tlb_finish_mmu"];

    // Other architectures are not supported
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        debug!("{NAME} sampler is not supported on this architecture");
        return Ok(None);
    }

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .enabled_programs(enabled_programs)
    .cpu_counters("events", events)
    .packed_counters("cgroup_task_switch", &CGROUP_TLB_FLUSH_TASK_SWITCH)
    .packed_counters(
        "cgroup_remote_shootdown",
        &CGROUP_TLB_FLUSH_REMOTE_SHOOTDOWN,
    )
    .packed_counters("cgroup_local_shootdown", &CGROUP_TLB_FLUSH_LOCAL_SHOOTDOWN)
    .packed_counters(
        "cgroup_local_mm_shootdown",
        &CGROUP_TLB_FLUSH_LOCAL_MM_SHOOTDOWN,
    )
    .packed_counters("cgroup_remote_send_ipi", &CGROUP_TLB_FLUSH_REMOTE_SEND_IPI)
    .ringbuf_handler("cgroup_info", handle_cgroup_info)
    .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_> {
        match name {
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_task_switch" => &self.maps.cgroup_task_switch,
            "cgroup_remote_shootdown" => &self.maps.cgroup_remote_shootdown,
            "cgroup_local_shootdown" => &self.maps.cgroup_local_shootdown,
            "cgroup_local_mm_shootdown" => &self.maps.cgroup_local_mm_shootdown,
            "cgroup_remote_send_ipi" => &self.maps.cgroup_remote_send_ipi,
            "events" => &self.maps.events,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        #[cfg(target_arch = "x86_64")]
        debug!(
            "{NAME} tlb_flush() BPF instruction count: {}",
            self.progs.tlb_flush.insn_cnt()
        );

        #[cfg(target_arch = "aarch64")]
        debug!(
            "{NAME} tlb_finish_mmu() BPF instruction count: {}",
            self.progs.tlb_finish_mmu.insn_cnt()
        );
    }
}
