#[distributed_slice(SYSCALL_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Syscall::new(config))
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/syscall_latency.bpf.rs"));
}

use bpf::*;

use super::stats::*;
use super::*;
use crate::common::bpf::*;
use crate::common::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.obj.map(name).unwrap()
    }
}

/// Collects Scheduler Runqueue Latency stats using BPF and traces:
/// * `raw_syscalls/sys_enter`
/// * `raw_syscalls/sys_exit`
///
/// And produces these stats:
/// * `syscall/total`
/// * `syscall/total/latency`
pub struct Syscall {
    bpf: Bpf<ModSkel<'static>>,
    counter_interval: Duration,
    counter_next: Instant,
    counter_prev: Instant,
    distribution_interval: Duration,
    distribution_next: Instant,
    distribution_prev: Instant,
}

impl Syscall {
    pub fn new(_config: &Config) -> Self {
        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open()
            .expect("failed to open bpf builder")
            .load()
            .expect("failed to load bpf program");
        skel.attach().expect("failed to attach bpf");

        let mut bpf = Bpf::from_skel(skel);

        let counters = vec![
            Counter::new(&SYSCALL_TOTAL, Some(&SYSCALL_TOTAL_HEATMAP)),
            Counter::new(&SYSCALL_READ, Some(&SYSCALL_READ_HEATMAP)),
            Counter::new(&SYSCALL_WRITE, Some(&SYSCALL_WRITE_HEATMAP)),
        ];

        bpf.add_counters("counters", counters);

        let mut distributions = vec![("total_latency", &SYSCALL_TOTAL_LATENCY)];

        for (name, heatmap) in distributions.drain(..) {
            bpf.add_distribution(name, heatmap);
        }

        Self {
            bpf,
            counter_interval: Duration::from_millis(10),
            counter_next: Instant::now(),
            counter_prev: Instant::now(),
            distribution_interval: Duration::from_millis(50),
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

impl Sampler for Syscall {
    fn sample(&mut self) {
        let now = Instant::now();
        self.refresh_counters(now);
        self.refresh_distributions(now);
    }
}
