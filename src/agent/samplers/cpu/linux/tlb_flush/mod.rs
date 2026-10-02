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

/// The metrics a cgroup id reaches, one list per acquisition group.
///
/// One id spans several groups, and each group's schema carries its own copy
/// of the labels, so each group's metrics need their own entry.
static CGROUP_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(CGROUP_IDENTITY_GROUPS);

static CGROUP_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &CGROUP_TLB_FLUSH_TASK_SWITCH,
    &CGROUP_TLB_FLUSH_REMOTE_SHOOTDOWN,
    &CGROUP_TLB_FLUSH_LOCAL_SHOOTDOWN,
    &CGROUP_TLB_FLUSH_LOCAL_MM_SHOOTDOWN,
    &CGROUP_TLB_FLUSH_REMOTE_SEND_IPI,
]];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

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
    // The tracepoint as tp_btf where the kernel has BTF (the per-cgroup path
    // reads the task group through a BTF pointer), raw_tp otherwise.
    #[cfg(target_arch = "x86_64")]
    let enabled_programs: &[&'static str] = if kernel_has_btf() {
        &["tlb_flush_btf"]
    } else {
        &["tlb_flush_raw"]
    };

    #[cfg(target_arch = "aarch64")]
    let enabled_programs = &["tlb_finish_mmu"];

    // Other architectures are not supported
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        debug!("{NAME} sampler is not supported on this architecture");
        return Err(crate::agent::sampler_status::Unsupported(
            "not supported on this CPU architecture".to_string(),
        )
        .into());
    }

    let cgroup_attribution = config.cgroup_attribution_or(NAME, true);

    let mut builder = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .enabled_programs(enabled_programs)
    .cpu_counters("events", events, &EVENTS_ACQ)
    // The switch is read-only data the verifier folds at load (see
    // `cgroup_attribution` in mod.bpf.c); the cgroup maps and their series
    // exist only when it is on.
    .pre_load(move |open| {
        open.maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data")
            .cgroup_attribution = cgroup_attribution as u8;
    });

    if cgroup_attribution {
        builder = builder
            .packed_counters(
                "cgroup_task_switch",
                &CGROUP_TLB_FLUSH_TASK_SWITCH,
                &CGROUP_EVENTS_ACQ,
            )
            .packed_counters(
                "cgroup_remote_shootdown",
                &CGROUP_TLB_FLUSH_REMOTE_SHOOTDOWN,
                &CGROUP_EVENTS_ACQ,
            )
            .packed_counters(
                "cgroup_local_shootdown",
                &CGROUP_TLB_FLUSH_LOCAL_SHOOTDOWN,
                &CGROUP_EVENTS_ACQ,
            )
            .packed_counters(
                "cgroup_local_mm_shootdown",
                &CGROUP_TLB_FLUSH_LOCAL_MM_SHOOTDOWN,
                &CGROUP_EVENTS_ACQ,
            )
            .packed_counters(
                "cgroup_remote_send_ipi",
                &CGROUP_TLB_FLUSH_REMOTE_SEND_IPI,
                &CGROUP_EVENTS_ACQ,
            )
            .ringbuf_handler("cgroup_info", handle_cgroup_info);
    }

    let bpf = builder.build()?;

    Ok(Some(Box::new(bpf)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

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
        {
            debug!(
                "{NAME} tlb_flush_btf() BPF instruction count: {}",
                self.progs.tlb_flush_btf.insn_cnt()
            );
            debug!(
                "{NAME} tlb_flush_raw() BPF instruction count: {}",
                self.progs.tlb_flush_raw.insn_cnt()
            );
        }

        #[cfg(target_arch = "aarch64")]
        debug!(
            "{NAME} tlb_finish_mmu() BPF instruction count: {}",
            self.progs.tlb_finish_mmu.insn_cnt()
        );
    }
}
