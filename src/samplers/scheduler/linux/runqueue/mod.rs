/// Collects Scheduler Runqueue stats using BPF and traces:
/// * `sched_wakeup`
/// * `sched_wakeup_new`
/// * `sched_switch`
///
/// And produces these stats:
/// * `scheduler/runqueue/latency`
/// * `scheduler/running`
/// * `scheduler/context_switch/involuntary`
/// * `scheduler/context_switch/voluntary`

const NAME: &str = "scheduler_runqueue";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/scheduler_runqueue.bpf.rs"));
}

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::scheduler::stats::*;
use crate::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !(config.enabled(NAME) && config.bpf(NAME)) {
        return;
    }

    let counters = vec![Counter::new(&SCHEDULER_IVCSW, None)];

    let bpf = AsyncBpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .distribution("runqlat", &SCHEDULER_RUNQUEUE_LATENCY)
        .distribution("running", &SCHEDULER_RUNNING)
        .distribution("offcpu", &SCHEDULER_OFFCPU)
        .collected_at(&METADATA_SCHEDULER_RUNQUEUE_COLLECTED_AT)
        .runtime(
            &METADATA_SCHEDULER_RUNQUEUE_RUNTIME,
            &METADATA_SCHEDULER_RUNQUEUE_RUNTIME_HISTOGRAM,
        )
        .build();

    if bpf.is_err() {
        return;
    }

    runtime.spawn(async move {
        let mut sampler = AsyncBpfSampler::new(bpf.unwrap(), config.async_interval(NAME));

        loop {
            if sampler.is_finished() {
                return;
            }

            sampler.sample().await;
        }
    });
}

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            "runqlat" => &self.maps.runqlat,
            "running" => &self.maps.running,
            "offcpu" => &self.maps.offcpu,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} handle__sched_wakeup() BPF instruction count: {}",
            self.progs.handle__sched_wakeup.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_new() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_new.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_switch() BPF instruction count: {}",
            self.progs.handle__sched_switch.insn_cnt()
        );
    }
}
