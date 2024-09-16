/// Collects Scheduler Runqueue stats using BPF and traces:
/// * `sched_wakeup`
/// * `sched_wakeup_new`
/// * `sched_switch`
///
/// And produces these stats:
/// * `scheduler/runqueue/latency`
/// * `scheduler/running`
/// * `scheduler/offcpu`
/// * `scheduler/context_switch/involuntary`

const NAME: &str = "scheduler_runqueue";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/scheduler_runqueue.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::samplers::scheduler::linux::stats::*;
use crate::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![&SCHEDULER_IVCSW];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .histogram("runqlat", &SCHEDULER_RUNQUEUE_LATENCY)
        .histogram("running", &SCHEDULER_RUNNING)
        .histogram("offcpu", &SCHEDULER_OFFCPU)
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
