use super::stats::*;
use super::*;
use crate::common::{Counter, Nop};
use perf_event::events::Hardware;
use perf_event::{Builder, Group};

#[distributed_slice(CPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(cpi) = Cpi::new(config) {
        Box::new(cpi)
    } else {
        Box::new(Nop {})
    }
}

pub struct Cpi {
    prev: Instant,
    next: Instant,
    interval: Duration,
    group: Group,
    counters: Vec<(Counter, perf_event::Counter)>,
}

impl Cpi {
    pub fn new(_config: &Config) -> Result<Self, ()> {
        let now = Instant::now();

        // initialize a group for the perf counters
        let mut group = match Group::new() {
            Ok(g) => g,
            Err(_) => {
                error!("couldn't init perf group");
                return Err(());
            }
        };

        // initialize the counters
        let mut counters = vec![];

        match Builder::new()
            .group(&mut group)
            .kind(Hardware::CPU_CYCLES)
            .build()
        {
            Ok(counter) => {
                counters.push((Counter::new(&CPU_CYCLES, None), counter));
            }
            Err(_) => {
                error!("failed to initialize cpu cycles perf counter");
                return Err(());
            }
        }

        match Builder::new()
            .group(&mut group)
            .kind(Hardware::INSTRUCTIONS)
            .build()
        {
            Ok(counter) => {
                counters.push((Counter::new(&CPU_INSTRUCTIONS, None), counter));
            }
            Err(_) => {
                error!("failed to initialize cpu instructions perf counter");
                return Err(());
            }
        }

        // enable the counters in the group
        if group.enable().is_err() {
            error!("couldn't enable perf counter group");
            return Err(());
        }

        Ok(Self {
            prev: now,
            next: now,
            interval: Duration::from_millis(10),
            group,
            counters,
        })
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
