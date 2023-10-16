use super::*;
use std::fs::File;
use std::io::{Read, Seek};

pub struct ProcCpuinfo {
    prev: Instant,
    next: Instant,
    interval: Duration,
    file: File,
}

impl ProcCpuinfo {
    pub fn new(_config: &Config) -> Result<Self, ()> {
        let now = Instant::now();
        let file = File::open("/proc/cpuinfo").map_err(|e| {
            error!("failed to open /proc/cpuinfo: {e}");
        })?;

        Ok(Self {
            file,
            prev: now,
            next: now,
            interval: Duration::from_millis(50),
        })
    }
}

impl Sampler for ProcCpuinfo {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        if self.sample_proc_cpuinfo().is_err() {
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

impl ProcCpuinfo {
    fn sample_proc_cpuinfo(&mut self) -> Result<(), std::io::Error> {
        self.file.rewind()?;

        let mut data = String::new();
        self.file.read_to_string(&mut data)?;

        let mut online_cores = 0;

        let lines = data.lines();

        let mut frequency = 0;

        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();

            if let Some(&"processor") = parts.first() {
                online_cores += 1;
            }

            if let (Some(&"cpu"), Some(&"MHz")) = (parts.first(), parts.get(1)) {
                if let Some(Ok(freq)) = parts
                    .get(3)
                    .map(|v| v.parse::<f64>().map(|v| v.floor() as u64))
                {
                    let _ = CPU_FREQUENCY_HISTOGRAM.increment(freq);
                    frequency += freq;
                }
            }
        }

        CPU_CORES.set(online_cores);

        if frequency != 0 && online_cores != 0 {
            CPU_FREQUENCY_AVERAGE.set(frequency as i64 / online_cores);
        }

        Ok(())
    }
}
