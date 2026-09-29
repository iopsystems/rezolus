//! Collects CPU perf counters using BPF and traces:
//! * `sched_switch`
//!
//! Initializes perf events to collect cycles and instructions.
//!
//! And produces these stats:
//! * `cpu_cycles`
//! * `cpu_instructions`
//! * `cgroup_cpu_cycles`
//! * `cgroup_cpu_instructions`
//!
//! These stats can be used to calculate the IPC and IPNS in post-processing or
//! in an observability stack.
//!
//! The per-cgroup series need a program on `sched_switch` that reads both
//! counters at every context switch and credits the difference to the
//! outgoing task's cgroup. That program is the sampler's only per-event cost,
//! and on a guest whose PMU is emulated by the hypervisor each counter read
//! is a VM exit (1-11 us measured on KVM; 20 us per switch for the two). With
//! `cgroup_attribution = false` the program is not loaded: `cpu_cycles` and
//! `cpu_instructions` are read per CPU at scrape time as before, and the
//! `cgroup_cpu_*` series are absent. On by default for this sampler.

const NAME: &str = "cpu_perf";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_perf.bpf.rs"));
}

use bpf::*;

use crate::agent::*;

use std::sync::Arc;

mod stats;

use stats::*;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

/// Every group a cgroup id reaches, paired with the metrics carrying it.
///
/// One id spans several streams, and a subscriber keeps identity per
/// stream — so each needs its own entry. Pairing them here is what stops a
/// call site publishing one group's identity under another's name.
static CGROUP_IDENTITY: crate::agent::identity::SlotIdentity =
    crate::agent::identity::SlotIdentity::new(CGROUP_IDENTITY_GROUPS);

#[linkme::distributed_slice(crate::agent::identity::SLOT_IDENTITIES)]
static CGROUP_IDENTITY_REG: &'static crate::agent::identity::SlotIdentity = &CGROUP_IDENTITY;

static CGROUP_IDENTITY_GROUPS: &[crate::agent::identity::GroupMetrics] = &[
    (&CGROUP_CYCLES_ACQ, &[&CGROUP_CPU_CYCLES]),
    (&CGROUP_INSTRUCTIONS_ACQ, &[&CGROUP_CPU_INSTRUCTIONS]),
];

fn handle_event(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let cgroup_attribution = config.cgroup_attribution_or(NAME, true);
    if !cgroup_attribution {
        info!("{NAME}: cgroup_attribution is off; no sched_switch program, per-CPU counters only");
    }

    // The sched_switch twin that matches the kernel's BTF, or neither when
    // cgroup attribution is off: then nothing runs per context switch.
    let disabled: &[&str] = if !cgroup_attribution {
        &["handle__sched_switch_raw", "handle__sched_switch_btf"]
    } else if kernel_has_btf() {
        &["handle__sched_switch_raw"]
    } else {
        &["handle__sched_switch_btf"]
    };

    let mut builder = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .perf_event(
        "cycles",
        PerfEvent::cpu_cycles(),
        &CPU_CYCLES,
        &CPU_PERF_ACQ,
    )
    .perf_event(
        "instructions",
        PerfEvent::instructions(),
        &CPU_INSTRUCTIONS,
        &CPU_PERF_ACQ,
    )
    .disabled_programs(disabled);

    if cgroup_attribution {
        builder = builder
            .packed_counters("cgroup_cycles", &CGROUP_CPU_CYCLES, &CGROUP_CYCLES_ACQ)
            .packed_counters(
                "cgroup_instructions",
                &CGROUP_CPU_INSTRUCTIONS,
                &CGROUP_INSTRUCTIONS_ACQ,
            )
            .ringbuf_handler("cgroup_info", handle_event);
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
            "cgroup_cycles" => &self.maps.cgroup_cycles,
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_instructions" => &self.maps.cgroup_instructions,
            "cycles" => &self.maps.cycles,
            "instructions" => &self.maps.instructions,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} handle__sched_switch_btf() BPF instruction count: {}",
            self.progs.handle__sched_switch_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_switch_raw() BPF instruction count: {}",
            self.progs.handle__sched_switch_raw.insn_cnt()
        );
    }
}
