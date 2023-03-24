use super::stats::*;
use super::*;
use crate::common::Counter;
use std::fs::File;
use std::io::{Read, Seek};

#[distributed_slice(CPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(ProcStat::new(config))
}

pub struct ProcStat {
    prev: Instant,
    next: Instant,
    interval: Duration,
    nanos_per_tick: u64,
    file: File,
    counters: Vec<(Counter, usize)>,
}

impl ProcStat {
    pub fn new(_config: &Config) -> Self {
        let now = Instant::now();

        let counters = vec![
            (
                Counter::new(&CPU_USAGE_USER, Some(&CPU_USAGE_USER_HEATMAP)),
                1,
            ),
            (
                Counter::new(&CPU_USAGE_NICE, Some(&CPU_USAGE_NICE_HEATMAP)),
                2,
            ),
            (
                Counter::new(&CPU_USAGE_SYSTEM, Some(&CPU_USAGE_SYSTEM_HEATMAP)),
                3,
            ),
            (
                Counter::new(&CPU_USAGE_IDLE, Some(&CPU_USAGE_IDLE_HEATMAP)),
                4,
            ),
            (
                Counter::new(&CPU_USAGE_IO_WAIT, Some(&CPU_USAGE_IO_WAIT_HEATMAP)),
                5,
            ),
            (
                Counter::new(&CPU_USAGE_IRQ, Some(&CPU_USAGE_IRQ_HEATMAP)),
                6,
            ),
            (
                Counter::new(&CPU_USAGE_SOFTIRQ, Some(&CPU_USAGE_SOFTIRQ_HEATMAP)),
                7,
            ),
            (
                Counter::new(&CPU_USAGE_STEAL, Some(&CPU_USAGE_STEAL_HEATMAP)),
                8,
            ),
            (
                Counter::new(&CPU_USAGE_GUEST, Some(&CPU_USAGE_GUEST_HEATMAP)),
                9,
            ),
            (
                Counter::new(&CPU_USAGE_GUEST_NICE, Some(&CPU_USAGE_GUEST_NICE_HEATMAP)),
                10,
            ),
        ];

        let nanos_per_tick = 1_000_000_000
            / (sysconf::raw::sysconf(sysconf::raw::SysconfVariable::ScClkTck)
                .expect("Failed to get system clock tick rate") as u64);

        Self {
            file: File::open("/proc/stat").expect("file not found"),
            counters,
            nanos_per_tick,
            prev: now,
            next: now,
            interval: Duration::from_millis(100),
        }
    }
}

impl Sampler for ProcStat {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        if self.sample_proc_stat(now, elapsed).is_err() {
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

impl ProcStat {
    fn sample_proc_stat(&mut self, now: Instant, elapsed: f64) -> Result<(), std::io::Error> {
        self.file.rewind()?;

        let mut data = String::new();
        self.file.read_to_string(&mut data)?;

        let lines = data.lines();

        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();

            if let Some(&"cpu") = parts.first() {
                for (counter, field) in &mut self.counters {
                    if let Some(Ok(v)) = parts.get(*field).map(|v| v.parse::<u64>()) {
                        counter.set(now, elapsed, v.wrapping_mul(self.nanos_per_tick))
                    }
                }
            }
        }

        Ok(())
    }
}
