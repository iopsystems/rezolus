use super::*;

struct GroupData {
    inner: perf_event::GroupData,
}

impl core::ops::Deref for GroupData {
    type Target = perf_event::GroupData;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl GroupData {
    pub fn enabled_since(&self, prev: &Self) -> Option<std::time::Duration> {
        if let (Some(this), Some(prev)) = (self.time_enabled(), prev.time_enabled()) {
            Some(this - prev)
        } else {
            None
        }
    }

    pub fn running_since(&self, prev: &Self) -> Option<std::time::Duration> {
        if let (Some(this), Some(prev)) = (self.time_running(), prev.time_running()) {
            Some(this - prev)
        } else {
            None
        }
    }

    pub fn delta(&self, prev: &Self, counter: &perf_event::Counter) -> Option<u64> {
        if let (Some(this), Some(prev)) = (self.get(counter), prev.get(counter)) {
            Some(this.value() - prev.value())
        } else {
            None
        }
    }
}

pub struct Reading {
    pub cycles: u64,
    pub instructions: u64,
    pub ipkc: u64,
    pub ipus: u64,
    pub base_frequency_mhz: u64,
    pub running_frequency_mhz: u64,
}

/// Per-cpu perf event group that measure all tasks on one CPU
pub struct PerfGroup {
    /// The CPU this group measures
    _cpuid: usize,
    /// Executed cycles and also the group leader
    cycles: perf_event::Counter,
    /// Retired instructions
    instructions: perf_event::Counter,
    /// Timestamp counter
    tsc: perf_event::Counter,
    /// Actual performance frequency clock
    aperf: perf_event::Counter,
    /// Maximum performance frequency clock
    mperf: perf_event::Counter,
    /// prev holds the previous readings
    prev: Option<GroupData>,
}

impl PerfGroup {
    /// Create and enable the group on the cpu
    pub fn new(cpuid: usize) -> Result<Self, ()> {
        let mut cycles = Builder::new(Hardware::CPU_CYCLES)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .pinned(true)
            .read_format(
                ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
            )
            .build()
            .map_err(|e| {
                error!("failed to create the cycles event on CPU{cpuid}: {e}");
            })?;

        let instructions = Builder::new(Hardware::INSTRUCTIONS)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
            .map_err(|e| {
                error!("failed to create the instructions event on CPU{cpuid}: {e}");
            })?;

        let tsc_event = Msr::new(MsrId::TSC)
            .map_err(|e| error!("failed to create perf event for tsc msr: {e}"))?;
        let tsc = Builder::new(tsc_event)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
            .map_err(|e| {
                error!("failed to create the tsc counter on CPU{cpuid}: {e}");
            })?;

        let aperf_event = Msr::new(MsrId::APERF)
            .map_err(|e| error!("failed to create perf event for aperf msr: {e}"))?;
        let aperf = Builder::new(aperf_event)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
            .map_err(|e| {
                error!("failed to create the aperf counter on CPU{cpuid}: {e}");
            })?;

        let mperf_event = Msr::new(MsrId::MPERF)
            .map_err(|e| error!("failed to create perf event for mperf msr: {e}"))?;
        let mperf = Builder::new(mperf_event)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
            .map_err(|e| {
                error!("failed to create the mperf counter on CPU{cpuid}: {e}");
            })?;

        cycles.enable_group().map_err(|e| {
            error!("failed to enable the perf group on CPU{cpuid}: {e}");
        })?;

        let prev = cycles
            .read_group()
            .map_err(|e| {
                warn!("failed to read the perf group on CPU{cpuid}: {e}");
            })
            .map(|inner| GroupData { inner })
            .ok();

        return Ok(Self {
            _cpuid: cpuid,
            cycles,
            instructions,
            tsc,
            aperf,
            mperf,
            prev,
        });
    }

    pub fn get_metrics(&mut self) -> Result<Reading, ()> {
        let current = self
            .cycles
            .read_group()
            .map_err(|e| {
                debug!("error reading perf group: {e}");
                self.prev = None;
            })
            .map(|inner| GroupData { inner })?;

        if self.prev.is_none() {
            self.prev = Some(current);
            return Err(());
        }

        let prev = self.prev.as_ref().unwrap();

        // When the CPU is offline, this.len() becomes 1
        if current.len() == 1 || current.len() != prev.len() {
            self.prev = Some(current);
            return Err(());
        }

        let enabled_us = current
            .enabled_since(prev)
            .ok_or(())
            .map(|v| v.as_micros() as u64)?;
        let running_us = current
            .running_since(prev)
            .ok_or(())
            .map(|v| v.as_micros() as u64)?;

        if running_us != enabled_us {
            self.prev = Some(current);
            return Err(());
        }

        let cycles = current.delta(prev, &self.cycles).ok_or(())?;
        let instructions = current.delta(prev, &self.instructions).ok_or(())?;

        if cycles == 0 || instructions == 0 {
            self.prev = Some(current);
            return Err(());
        }

        let tsc = current.delta(prev, &self.tsc).ok_or(())?;
        let mperf = current.delta(prev, &self.mperf).ok_or(())?;
        let aperf = current.delta(prev, &self.aperf).ok_or(())?;

        // computer IPKC IPUS BASE_FREQUENCY RUNNING_FREQUENCY
        let ipkc = (instructions * 1000) / cycles;
        let base_frequency_mhz = tsc / running_us;
        let running_frequency_mhz = (base_frequency_mhz * aperf) / mperf;
        let ipus = (ipkc * aperf) / mperf;

        self.prev = Some(current);

        Ok(Reading {
            cycles,
            instructions,
            ipkc,
            ipus,
            base_frequency_mhz,
            running_frequency_mhz,
        })
    }
}
