#[distributed_slice(SCHEDULER_BPF_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Runqlat::new(config))
}

mod bpf;

use bpf::*;

use crate::common::*;
use crate::common::bpf::*;
use super::super::stats::*;
use super::super::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.obj.map(name).unwrap()
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
    counter_interval: Duration,
    counter_next: Instant,
    counter_prev: Instant,
    distribution_interval: Duration,
    distribution_next: Instant,
    distribution_prev: Instant,
}

impl Runqlat {
    pub fn new(_config: &Config) -> Self {
        let builder = ModSkelBuilder::default();
        let mut skel = builder.open().expect("failed to open bpf builder").load().expect("failed to load bpf program");
        skel.attach().expect("failed to attach bpf");

        let mut bpf = Bpf::from_skel(skel);

        let mut percpu_counters = vec![
            ("ivcsw", Counter::new(&SCHEDULER_IVCSW, None)),
            ("vcsw", Counter::new(&SCHEDULER_VCSW, None)),
        ];

        for (name, counter) in percpu_counters.drain(..) {
            bpf.add_percpu_counter(name, counter);
        }

        let mut distributions = vec![
            ("runqlat", &SCHEDULER_RUNQUEUE_LATENCY),
            ("running", &SCHEDULER_RUNNING),
        ];

        for (name, heatmap) in distributions.drain(..) {
            bpf.add_distribution(name, heatmap);
        }

        Self {
            bpf,
            counter_interval: Duration::from_millis(10),
            counter_next: Instant::now(),
            counter_prev: Instant::now(),
            distribution_interval: Duration::from_millis(200),
            distribution_next: Instant::now(),
            distribution_prev: Instant::now(),
        }
    }

    pub fn refresh_counters(&mut self, now: Instant) {
        if now < self.counter_next {
            return;
        }

        let elapsed = (now - self.counter_prev).as_secs_f64();

        self.bpf.refresh_counters(now, elapsed);

        // determine when to sample next
        let next = self.counter_next + self.counter_interval;
        
        // check that next sample time is in the future
        if next > now {
            self.counter_next = next;
        } else {
            self.counter_next = now + self.counter_interval;
        }

        // mark when we last sampled
        self.counter_prev = now;

    }

    pub fn refresh_distributions(&mut self, now: Instant) {
        if now < self.distribution_next {
            return;
        }

        self.bpf.refresh_distributions(now);

        // determine when to sample next
        let next = self.distribution_next + self.distribution_interval;
        
        // check that next sample time is in the future
        if next > now {
            self.distribution_next = next;
        } else {
            self.distribution_next = now + self.distribution_interval;
        }

        // mark when we last sampled
        self.distribution_prev = now;
    }
}

impl Sampler for Runqlat {
    fn sample(&mut self) {
        let now = Instant::now();
        self.refresh_counters(now);
        self.refresh_distributions(now);
    }
}

impl std::fmt::Display for Runqlat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "scheduler::bpf::runqlat")
    }
}
