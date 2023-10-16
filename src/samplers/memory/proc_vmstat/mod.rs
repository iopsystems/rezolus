use super::stats::*;
use super::*;
use crate::common::{Counter, Nop};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};

#[distributed_slice(MEMORY_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = ProcVmstat::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

const NAME: &str = "memory_vmstat";

pub struct ProcVmstat {
    prev: Instant,
    next: Instant,
    interval: Duration,
    counters: HashMap<&'static str, Counter>,
    file: File,
}

impl ProcVmstat {
    #[allow(dead_code)]
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let now = Instant::now();

        let counters = HashMap::from([
            ("numa_hit", Counter::new(&MEMORY_NUMA_HIT, None)),
            ("numa_miss", Counter::new(&MEMORY_NUMA_MISS, None)),
            ("numa_foreign", Counter::new(&MEMORY_NUMA_FOREIGN, None)),
            (
                "numa_interleave",
                Counter::new(&MEMORY_NUMA_INTERLEAVE, None),
            ),
            ("numa_local", Counter::new(&MEMORY_NUMA_LOCAL, None)),
            ("numa_other", Counter::new(&MEMORY_NUMA_OTHER, None)),
        ]);

        Ok(Self {
            file: File::open("/proc/vmstat").expect("file not found"),
            counters,
            prev: now,
            next: now,
            interval: config.interval(NAME),
        })
    }
}

impl Sampler for ProcVmstat {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        if self.sample_proc_vmstat(elapsed).is_err() {
            return;
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

impl ProcVmstat {
    fn sample_proc_vmstat(&mut self, elapsed: f64) -> Result<(), std::io::Error> {
        self.file.rewind()?;

        let mut data = String::new();
        self.file.read_to_string(&mut data)?;

        let lines = data.lines();

        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();

            if parts.is_empty() {
                continue;
            }

            if let Some(counter) = self.counters.get_mut(*parts.first().unwrap()) {
                if let Some(Ok(v)) = parts.get(1).map(|v| v.parse::<u64>()) {
                    counter.set(elapsed, v);
                }
            }
        }

        Ok(())
    }
}
