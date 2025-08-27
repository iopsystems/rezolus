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

static CGROUP_METRICS: &[&dyn MetricGroup] = &[
    &CGROUP_SCHEDULER_IVCSW,
    &CGROUP_SCHEDULER_OFFCPU,
    &CGROUP_SCHEDULER_RUNQUEUE_WAIT,
];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, CGROUP_METRICS)
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![&SCHEDULER_IVCSW, &SCHEDULER_RUNQUEUE_WAIT];

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .cpu_counters("counters", counters)
    .histogram("runqlat", &SCHEDULER_RUNQUEUE_LATENCY)
    .histogram("running", &SCHEDULER_RUNNING)
    .histogram("offcpu", &SCHEDULER_OFFCPU)
    .packed_counters("cgroup_runq_wait", &CGROUP_SCHEDULER_RUNQUEUE_WAIT)
    .packed_counters("cgroup_offcpu", &CGROUP_SCHEDULER_OFFCPU)
    .packed_counters("cgroup_ivcsw", &CGROUP_SCHEDULER_IVCSW)
    .ringbuf_handler("cgroup_info", handle_cgroup_info)
    .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            "offcpu" => &self.maps.offcpu,
            "running" => &self.maps.running,
            "runqlat" => &self.maps.runqlat,
            "cgroup_runq_wait" => &self.maps.cgroup_runq_wait,
            "cgroup_offcpu" => &self.maps.cgroup_offcpu,
            "cgroup_ivcsw" => &self.maps.cgroup_ivcsw,
            "cgroup_info" => &self.maps.cgroup_info,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} handle__sched_switch() BPF instruction count: {}",
            self.progs.handle__sched_switch.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup() BPF instruction count: {}",
            self.progs.handle__sched_wakeup.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_new() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_new.insn_cnt()
        );
    }
}
