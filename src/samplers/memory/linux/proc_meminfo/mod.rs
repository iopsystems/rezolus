use crate::common::units::KIBIBYTES;
use crate::common::{Interval, Nop};
use crate::samplers::memory::stats::*;
use crate::samplers::memory::*;
use metriken::Gauge;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};

#[distributed_slice(MEMORY_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = ProcMeminfo::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

const NAME: &str = "memory_meminfo";

pub struct ProcMeminfo {
    interval: Interval,
    file: File,
    gauges: HashMap<&'static str, &'static Gauge>,
}

impl ProcMeminfo {
    #![allow(dead_code)]
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let gauges: HashMap<&str, &Gauge> = HashMap::from([
            ("MemTotal:", &*MEMORY_TOTAL),
            ("MemFree:", &*MEMORY_FREE),
            ("MemAvailable:", &*MEMORY_AVAILABLE),
            ("Buffers:", &*MEMORY_BUFFERS),
            ("Cached:", &*MEMORY_CACHED),
        ]);

        Ok(Self {
            file: File::open("/proc/meminfo").expect("file not found"),
            gauges,
            interval: Interval::new(Instant::now(), config.interval(NAME)),
        })
    }
}

impl Sampler for ProcMeminfo {
    fn sample(&mut self) {
        let now = Instant::now();

        if self.interval.try_wait(now).is_err() {
            return;
        }

        let _ = self.sample_proc_meminfo(now).is_err();
    }
}

impl ProcMeminfo {
    fn sample_proc_meminfo(&mut self, _now: Instant) -> Result<(), std::io::Error> {
        self.file.rewind()?;

        let mut data = String::new();
        self.file.read_to_string(&mut data)?;

        let lines = data.lines();

        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            if let Some(gauge) = self.gauges.get_mut(*parts.first().unwrap()) {
                if let Some(Ok(v)) = parts.get(1).map(|v| v.parse::<i64>()) {
                    gauge.set(v * KIBIBYTES as i64);
                }
            }
        }

        Ok(())
    }
}
