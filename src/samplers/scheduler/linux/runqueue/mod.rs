#[distributed_slice(SCHEDULER_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = Runqlat::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/scheduler_runqueue.bpf.rs"));
}

const NAME: &str = "scheduler_runqueue";

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::scheduler::stats::*;
use crate::samplers::scheduler::*;

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
    bpf: Bpf<ModSkel<'static>>,
    interval: Interval,
}

impl Runqlat {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !(config.enabled(NAME) && config.bpf(NAME)) {
            return Err(());
        }

        let open_object: &'static mut MaybeUninit<OpenObject> =
            Box::leak(Box::new(MaybeUninit::uninit()));

        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open(open_object)
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        debug!(
            "{NAME} handle__sched_wakeup() BPF instruction count: {}",
            skel.progs.handle__sched_wakeup.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_new() BPF instruction count: {}",
            skel.progs.handle__sched_wakeup_new.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_switch() BPF instruction count: {}",
            skel.progs.handle__sched_switch.insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let counters = vec![Counter::new(&SCHEDULER_IVCSW, None)];

        let bpf = BpfBuilder::new(skel)
            .counters("counters", counters)
            .distribution("runqlat", &SCHEDULER_RUNQUEUE_LATENCY)
            .distribution("running", &SCHEDULER_RUNNING)
            .distribution("offcpu", &SCHEDULER_OFFCPU)
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            interval: Interval::new(now, config.interval(NAME)),
        })
    }

    pub fn refresh(&mut self, now: Instant) -> Result<(), ()> {
        let elapsed = self.interval.try_wait(now)?;

        METADATA_SCHEDULER_RUNQUEUE_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

        self.bpf.refresh(elapsed);

        Ok(())
    }
}

impl Sampler for Runqlat {
    fn sample(&mut self) {
        let now = Instant::now();

        if self.refresh(now).is_ok() {
            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_SCHEDULER_RUNQUEUE_RUNTIME.add(elapsed);
            let _ = METADATA_SCHEDULER_RUNQUEUE_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
