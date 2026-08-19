//! Collects Scheduler Runqueue stats using BPF and traces:
//! * `sched_wakeup`
//! * `sched_wakeup_new`
//! * `sched_switch`
//!
//! And produces these stats:
//! * `scheduler/runqueue/latency`
//! * `scheduler/running`
//! * `scheduler/offcpu`
//! * `scheduler/context_switch/involuntary`
//! * `scheduler/context_switch/voluntary`

const NAME: &str = "scheduler_runqueue";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/scheduler_runqueue.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

static CGROUP_METRICS: &[&dyn GroupMetadata] = &[
    &CGROUP_SCHEDULER_IVCSW,
    &CGROUP_SCHEDULER_VCSW,
    &CGROUP_SCHEDULER_OFFCPU,
    &CGROUP_SCHEDULER_RUNQUEUE_WAIT,
];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, CGROUP_METRICS)
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // order must match the counter positions in mod.bpf.c
    let counters = vec![
        &SCHEDULER_IVCSW,
        &SCHEDULER_RUNQUEUE_WAIT,
        &SCHEDULER_DISCARDED,
        &SCHEDULER_VCSW,
    ];

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .cpu_counters("counters", counters, &COUNTERS_ACQ)
    .histogram("runqlat", &SCHEDULER_RUNQUEUE_LATENCY, &RUNQLAT_ACQ)
    .histogram("running", &SCHEDULER_RUNNING, &RUNNING_ACQ)
    .histogram("offcpu", &SCHEDULER_OFFCPU, &OFFCPU_ACQ)
    .packed_counters(
        "cgroup_runq_wait",
        &CGROUP_SCHEDULER_RUNQUEUE_WAIT,
        &CGROUP_WAIT_ACQ,
    )
    .packed_counters(
        "cgroup_offcpu",
        &CGROUP_SCHEDULER_OFFCPU,
        &CGROUP_OFFCPU_ACQ,
    )
    .packed_counters(
        "cgroup_ivcsw",
        &CGROUP_SCHEDULER_IVCSW,
        &CGROUP_CONTEXT_SWITCH_ACQ,
    )
    .packed_counters(
        "cgroup_vcsw",
        &CGROUP_SCHEDULER_VCSW,
        &CGROUP_CONTEXT_SWITCH_ACQ,
    )
    .ringbuf_handler("cgroup_info", handle_cgroup_info)
    .disabled_programs(if kernel_has_btf() {
        &[
            "handle__sched_wakeup_raw",
            "handle__sched_wakeup_new_raw",
            "handle__sched_switch_raw",
        ]
    } else {
        &[
            "handle__sched_wakeup_btf",
            "handle__sched_wakeup_new_btf",
            "handle__sched_switch_btf",
        ]
    })
    .build()?;

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
            "counters" => &self.maps.counters,
            "offcpu" => &self.maps.offcpu,
            "running" => &self.maps.running,
            "runqlat" => &self.maps.runqlat,
            "cgroup_runq_wait" => &self.maps.cgroup_runq_wait,
            "cgroup_offcpu" => &self.maps.cgroup_offcpu,
            "cgroup_ivcsw" => &self.maps.cgroup_ivcsw,
            "cgroup_vcsw" => &self.maps.cgroup_vcsw,
            "cgroup_info" => &self.maps.cgroup_info,
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
        debug!(
            "{NAME} handle__sched_wakeup_btf() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_raw() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_raw.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_new_btf() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_new_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_new_raw() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_new_raw.insn_cnt()
        );
    }
}
