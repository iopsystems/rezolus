#[distributed_slice(SCHEDULER_BPF_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Runqlat::new(config))
}

mod bpf;

use bpf::*;

use common::{Counter, Distribution};
use super::super::stats::*;
use super::super::*;

/// Collects Scheduler Runqueue Latency stats using BPF
/// tracepoints:
/// * "tp_btf/sched_wakeup"
/// * "tp_btf/sched_wakeup_new"
/// * "tp_btf/sched_switch"
///
/// stats:
/// * scheduler/runqueue/latency
pub struct Runqlat {
    skel: ModSkel<'static>,
    distributions: Vec<Distribution>,

    next: Instant,
    dist_next: Instant,
    prev: Instant,
    interval: Duration,
    dist_interval: Duration,
}

impl Runqlat {
    pub fn new(_config: &Config) -> Self {
        let now = Instant::now();

        let builder = ModSkelBuilder::default();
        let mut skel = builder.open().expect("failed to open bpf builder").load().expect("failed to load bpf program");
        skel.attach().expect("failed to attach bpf");

        // these need to be in the same order as in the bpf
        // let counters = vec![];

        let distributions = vec![
            Distribution::new("latency", &SCHEDULER_RUNQUEUE_LATENCY),
        ];

        Self {
            skel,
            // counters,
            distributions,
            next: now,
            prev: now,
            dist_next: now,
            interval: Duration::from_millis(1),
            dist_interval: Duration::from_millis(100),
        }
    }   
}

impl Sampler for Runqlat {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        // let elapsed = (now - self.prev).as_secs_f64();

        // let maps = self.skel.maps();

        // let counts = crate::common::bpf::read_counters(maps.counters(), self.counters.len());

        // for (id, counter) in self.counters.iter_mut().enumerate() {
        //     if let Some(current) = counts.get(&id) {
        //         counter.update(now, elapsed, *current);
        //     }
        // }

        // determine if we should sample the distributions
        if now >= self.dist_next {
            for distribution in self.distributions.iter_mut() {
                distribution.update(&self.skel.obj);
            }

            // determine when to sample next
            let next = self.dist_next + self.dist_interval;

            // check that next sample time is in the future
            if next > now {
                self.dist_next = next;
            } else {
                self.dist_next = now + self.dist_interval;
            }
        }

        // determine when to sample next
        let next = self.next + self.interval;
        
        // check that next sample time is in the future
        if next > now {
            self.next = next;
        } else {
            self.next = now + self.interval;
        }

        // mark when we last sampled
        self.prev = now;
    }
}

impl std::fmt::Display for Runqlat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "scheduler::bpf::runqlat")
    }
}