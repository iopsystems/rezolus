use crate::common::SyncPrimitive;
use crate::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !(config.enabled(NAME) && config.bpf(NAME)) {
        return;
    }

    let sync = SyncPrimitive::new();

    let thread = spawn_bpf(sync.clone());

    runtime.spawn(async move {
        let mut s = Runqlat {
            thread,
            sync,
            interval: config.async_interval(NAME),
        };

        if s.thread.is_finished() {
            return;
        }

        loop {
            s.sample().await;
        }
    });
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/scheduler_runqueue.bpf.rs"));
}

const NAME: &str = "scheduler_runqueue";

use bpf::*;

use crate::common::bpf2::*;
use crate::common::*;
use crate::samplers::scheduler::stats::*;
use crate::samplers::scheduler::*;

use std::thread::JoinHandle;

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

/// Collects Scheduler Runqueue Latency stats using BPF and traces:
/// * `sched_wakeup`
/// * `sched_wakeup_new`
/// * `sched_switch`
///
/// And produces these stats:
/// * `scheduler/runqueue/latency`
/// * `scheduler/running`
/// * `scheduler/context_switch/involuntary`
/// * `scheduler/context_switch/voluntary`
pub struct Runqlat {
    thread: JoinHandle<()>,
    sync: SyncPrimitive,
    interval: AsyncInterval,
}

fn spawn_bpf(sync: SyncPrimitive) -> std::thread::JoinHandle<()> {
    // define userspace metric sets
    let counters = vec![Counter::new(&SCHEDULER_IVCSW, None)];

    std::thread::spawn(move || {
        let mut prev = Instant::now();

        let builder = BpfBuilder::new(ModSkelBuilder::default());

        if builder.is_err() {
            return;
        }

        let mut bpf = builder
            .unwrap()
            .counters("counters", counters)
            .distribution("runqlat", &SCHEDULER_RUNQUEUE_LATENCY)
            .distribution("running", &SCHEDULER_RUNNING)
            .distribution("offcpu", &SCHEDULER_OFFCPU)
            .build();

        // the sampler loop
        loop {
            // wait until we are notified to start
            sync.wait_trigger();

            let now = Instant::now();
            METADATA_SCHEDULER_RUNQUEUE_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

            // refresh userspace metrics
            bpf.refresh(now.duration_since(prev));

            // update metadata metrics
            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_SCHEDULER_RUNQUEUE_RUNTIME.add(elapsed);
            let _ = METADATA_SCHEDULER_RUNQUEUE_RUNTIME_HISTOGRAM.increment(elapsed);

            prev = now;

            // notify that we have finished running
            sync.notify();
        }
    })
}

#[async_trait]
impl AsyncSampler for Runqlat {
    async fn sample(&mut self) {
        // wait until it's time to sample
        self.interval.tick().await;

        // check that the thread has not exited
        if self.thread.is_finished() {
            return;
        }

        // notify the thread to start
        self.sync.trigger();

        // wait for notification that thread has finished
        self.sync.wait_notify().await;
    }
}
