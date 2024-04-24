use crate::samplers::filesystem::*;
use crate::samplers::hwinfo::hardware_info;
use crate::{error, Config, Duration, Instant, Sampler};
use metriken::DynBoxedMetric;
use metriken::LazyGauge;
use metriken::MetricBuilder;
use std::fs::File;
use std::io::{Read, Seek};

use super::NAME;

pub struct Procfs {
    prev: Instant,
    next: Instant,
    interval: Duration,
    file: File,
}

impl Procfs {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let now = Instant::now();

        let file = std::fs::File::open("/proc/sys/fs/file-nr").map_err(|e| {
            error!("failed to open: {e}");
        })?;

        Ok(Self {
            file,
            prev: now,
            next: now,
            interval: config.interval(NAME),
        })
    }
}

impl Sampler for Procfs {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let _ = self.sample_procfs();

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

impl Procfs {
    fn sample_procfs(&mut self) -> Result<(), std::io::Error> {
        self.file.rewind()?;

        let mut data = String::new();
        self.file.read_to_string(&mut data)?;

        let mut lines = data.lines();

        if let Some(line) = lines.next() {
            let parts: Vec<&str> = line.split_whitespace().collect();

            if parts.len() == 3 {
                if let Ok(open) = parts[0].parse::<i64>() {
                    FILESYSTEM_DESCRIPTORS_OPEN.set(open);
                }
            }
        }

        Ok(())
    }
}
