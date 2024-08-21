use super::*;
use clocksource::precise::UnixInstant;
use std::fs::File;
use std::io::{Read, Seek};

pub struct ProcCpuinfo {
    file: File,
    interval: Interval,
}

impl ProcCpuinfo {
    pub fn new(config: &Config) -> Result<Self, ()> {
        let file = File::open("/proc/cpuinfo").map_err(|e| {
            error!("failed to open /proc/cpuinfo: {e}");
        })?;

        Ok(Self {
            file,
            interval: Interval::new(Instant::now(), config.interval(NAME)),
        })
    }
}

impl Sampler for ProcCpuinfo {
    fn sample(&mut self) {
        let now = Instant::now();

        if let Ok(_) = self.interval.try_wait(now) {
            METADATA_CPU_PERF_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

            let _ = self.sample_proc_cpuinfo();

            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_CPU_PERF_RUNTIME.add(elapsed);
            let _ = METADATA_CPU_PERF_RUNTIME_HISTOGRAM.increment(elapsed);
        }
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
