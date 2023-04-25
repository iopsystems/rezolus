use super::stats::*;
use super::*;
use crate::common::Counter;
use perf_event::events::Hardware;
use perf_event::{Builder, Group};

#[distributed_slice(CPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Cpi::new(config))
}

pub struct Cpi {
    prev: Instant,
    next: Instant,
    interval: Duration,
    group: Group,
    counters: Vec<(Counter, perf_event::Counter)>,
}

impl Cpi {
    pub fn new(_config: &Config) -> Self {
        let now = Instant::now();

        let mut group = Group::new().expect("couldn't init perf group");
        let counters = vec![
            (
                Counter::new(&CPU_CYCLES, None),
                Builder::new()
                    .group(&mut group)
                    .kind(Hardware::CPU_CYCLES)
                    .build()
                    .expect("failed to init cycles counter"),
            ),
            (
                Counter::new(&CPU_INSTRUCTIONS, None),
                Builder::new()
                    .group(&mut group)
                    .kind(Hardware::INSTRUCTIONS)
                    .build()
                    .expect("failed to init instructions counter"),
            ),
        ];
        group.enable().expect("couldn't enable perf counters");

        Self {
            prev: now,
            next: now,
            interval: Duration::from_millis(10),
            group,
            counters,
        }
    }
}

impl Sampler for Cpi {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        match self.group.read() {
            Ok(counts) => {
                for (metric, pc) in &mut self.counters {
                    metric.set(now, elapsed, counts[&pc]);
                }
            }
            Err(e) => {
                error!("error sampling perf group: {e}");
            }
        }

        // determine when to sample next
        let next = self.next + self.interval;

        // it's possible we fell behind
        if next > now {
            // if we didn't, sample at the next planned time
            self.next = next;
        } else {
            // if we did, sample after the interval has elapsed
            self.next = now + self.interval;
        }

        // mark when we last sampled
        self.prev = now;
    }
}
