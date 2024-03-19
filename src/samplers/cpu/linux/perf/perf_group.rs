use super::*;

#[derive(Copy, Clone, Debug)]
enum Counter {
    Cycles,
    Instructions,
    Tsc,
    Aperf,
    Mperf,
}

impl Counter {
    fn builder(&self) -> Result<perf_event::Builder, std::io::Error> {
        match self {
            Self::Cycles => Ok(Builder::new(Hardware::CPU_CYCLES)),
            Self::Instructions => Ok(Builder::new(Hardware::INSTRUCTIONS)),
            Self::Tsc => {
                let msr = Msr::new(MsrId::TSC)?;
                Ok(Builder::new(msr))
            }
            Self::Aperf => {
                let msr = Msr::new(MsrId::APERF)?;
                Ok(Builder::new(msr))
            }
            Self::Mperf => {
                let msr = Msr::new(MsrId::MPERF)?;
                Ok(Builder::new(msr))
            }
        }
    }

    pub fn as_leader(&self, cpu: usize) -> Result<perf_event::Counter, std::io::Error> {
        self.builder()?
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .pinned(true)
            .read_format(
                ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
            )
            .build()
    }

    pub fn as_follower(
        &self,
        cpu: usize,
        leader: &mut perf_event::Counter,
    ) -> Result<perf_event::Counter, std::io::Error> {
        self.builder()?
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(leader)
    }
}

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
    /// The CPU this reading is from
    pub cpu: usize,
    pub cycles: Option<u64>,
    pub instructions: Option<u64>,
    pub ipkc: Option<u64>,
    pub ipus: Option<u64>,
    pub base_frequency_mhz: Option<u64>,
    pub running_frequency_mhz: Option<u64>,
}

/// Per-cpu perf event group that measure all tasks on one CPU
pub struct PerfGroup {
    /// The CPU this group measures
    cpu: usize,
    /// The group member that is the leader
    leader_id: usize,
    /// The counters in this group
    group: Vec<Option<perf_event::Counter>>,
    /// prev holds the previous readings
    prev: Option<GroupData>,
}

impl PerfGroup {
    /// Create and enable the group on the cpu
    pub fn new(cpu: usize) -> Result<Self, ()> {
        let mut group = Vec::new();

        let leader_id;

        if let Ok(c) = Counter::Cycles.as_leader(cpu) {
            leader_id = Counter::Cycles as usize;

            group.resize_with(Counter::Cycles as usize + 1, || None);
            group[Counter::Cycles as usize] = Some(c);
        } else if let Ok(c) = Counter::Tsc.as_leader(cpu) {
            leader_id = Counter::Tsc as usize;

            group.resize_with(Counter::Tsc as usize + 1, || None);
            group[Counter::Tsc as usize] = Some(c);
        } else {
            error!("failed to initialize a group leader on CPU{cpu}");
            return Err(());
        }

        for counter in &[
            Counter::Instructions,
            Counter::Tsc,
            Counter::Aperf,
            Counter::Mperf,
        ] {
            if leader_id == *counter as usize {
                continue;
            }

            if let Ok(c) = counter.as_follower(cpu, &mut group[leader_id].as_mut().unwrap()) {
                group.resize_with(*counter as usize + 1, || None);
                group[*counter as usize] = Some(c);
            }
        }

        group[leader_id]
            .as_mut()
            .unwrap()
            .enable_group()
            .map_err(|e| {
                error!("failed to enable the perf group on CPU{cpu}: {e}");
            })?;

        let prev = group[leader_id]
            .as_mut()
            .unwrap()
            .read_group()
            .map_err(|e| {
                warn!("failed to read the perf group on CPU{cpu}: {e}");
            })
            .map(|inner| GroupData { inner })
            .ok();

        return Ok(Self {
            cpu,
            leader_id,
            group,
            prev,
        });
    }

    pub fn get_metrics(&mut self) -> Result<Reading, ()> {
        let current = self.group[self.leader_id]
            .as_mut()
            .unwrap()
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

        if running_us != enabled_us || running_us == 0 {
            self.prev = Some(current);
            return Err(());
        }

        let mut cycles = None;
        let mut instructions = None;
        let mut tsc = None;
        let mut aperf = None;
        let mut mperf = None;

        if let Some(Some(c)) = &self.group.get(Counter::Cycles as usize) {
            cycles = current.delta(prev, &c);
        }

        if let Some(Some(c)) = &self.group.get(Counter::Instructions as usize) {
            instructions = current.delta(prev, &c);
        }

        if let Some(Some(c)) = &self.group.get(Counter::Tsc as usize) {
            tsc = current.delta(prev, &c);
        }

        if let Some(Some(c)) = &self.group.get(Counter::Aperf as usize) {
            aperf = current.delta(prev, &c);
        }

        if let Some(Some(c)) = &self.group.get(Counter::Mperf as usize) {
            mperf = current.delta(prev, &c);
        }

        let ipkc = if instructions.is_some() && cycles.is_some() {
            if cycles.unwrap() == 0 {
                None
            } else {
                Some(instructions.unwrap() * 1000 / cycles.unwrap())
            }
        } else {
            None
        };

        let base_frequency_mhz = tsc.map(|v| v / running_us);

        let mut running_frequency_mhz = None;
        let mut ipus = None;

        if aperf.is_some() && mperf.is_some() {
            if base_frequency_mhz.is_some() {
                running_frequency_mhz =
                    Some(base_frequency_mhz.unwrap() * aperf.unwrap() / mperf.unwrap());
            }

            if ipkc.is_some() {
                ipus = Some(ipkc.unwrap() * aperf.unwrap() / mperf.unwrap());
            }
        }

        self.prev = Some(current);

        Ok(Reading {
            cpu: self.cpu,
            cycles,
            instructions,
            ipkc,
            ipus,
            base_frequency_mhz,
            running_frequency_mhz,
        })
    }
}
