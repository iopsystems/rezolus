use super::stats::*;
use super::*;
use crate::common::Nop;
use perf_event::events::x86::{Msr, MsrId};
use perf_event::events::Hardware;
use perf_event::{Builder, GroupData, ReadFormat};
use samplers::hwinfo::hardware_info;

#[distributed_slice(CPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(cpi) = Cpi::new(config) {
        Box::new(cpi)
    } else {
        Box::new(Nop {})
    }
}

/// Per-cpu perf event group that measure all tasks on one CPU
struct PerfGroup {
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
    /// prev holds the previous reading and this has the last reading    
    prev: Result<GroupData, std::io::Error>,
    this: Result<GroupData, std::io::Error>,
}

impl PerfGroup {
    /// Create and enable the group on the cpu
    pub fn new(cpuid: usize) -> Result<Self, ()> {
        let mut cycles = match Builder::new(Hardware::CPU_CYCLES)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .pinned(true)
            .read_format(
                ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
            )
            .build()
        {
            Ok(counter) => counter,
            Err(_) => {
                error!("failed to create the cycles event on CPU{cpuid}");
                return Err(());
            }
        };

        let instructions = match Builder::new(Hardware::INSTRUCTIONS)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
        {
            Ok(counter) => counter,
            Err(_) => {
                error!("failed to create the instructions event on CPU{cpuid}");
                return Err(());
            }
        };

        let tsc_event = match Msr::new(MsrId::TSC) {
            Ok(e) => e,
            Err(_) => {
                error!("failed to find the tsc event on CPU{cpuid}");
                return Err(());
            }
        };
        let tsc = match Builder::new(tsc_event)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
        {
            Ok(e) => e,
            Err(_) => {
                error!("Failed to create the tsc event on CPU{cpuid}");
                return Err(());
            }
        };

        let aperf_event = match Msr::new(MsrId::APERF) {
            Ok(e) => e,
            Err(_) => return Err(()),
        };
        let aperf = match Builder::new(aperf_event)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
        {
            Ok(e) => e,
            Err(_) => {
                error!("Failed to create the aperf event on CPU{cpuid}");
                return Err(());
            }
        };

        let mperf_event = match Msr::new(MsrId::MPERF) {
            Ok(e) => e,
            Err(_) => return Err(()),
        };
        let mperf = match Builder::new(mperf_event)
            .one_cpu(cpuid)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
        {
            Ok(e) => e,
            Err(_) => {
                error!("failed to create the mperf event on CPU{cpuid}");
                return Err(());
            }
        };

        if let Err(_) = cycles.enable_group() {
            error!("failed to enable the perf group on CPU{cpuid}");
            return Err(());
        }
        let prev = cycles.read_group();
        let this = cycles.read_group();
        return Ok(Self {
            _cpuid: cpuid,
            cycles,
            instructions,
            tsc,
            aperf,
            mperf,
            prev,
            this,
        });
    }

    pub fn read_group(&mut self) -> Result<GroupData, std::io::Error> {
        return self.cycles.read_group();
    }

    pub fn update_group(&mut self) {
        std::mem::swap(&mut self.prev, &mut self.this);
        self.this = self.read_group();
    }

    pub fn get_metrics(&mut self) -> Option<(u64, u64, u64, u64, u64, u64)> {
        let (prev, this) = match (&mut self.prev, &mut self.this) {
            (Ok(prev), Ok(this)) => (prev, this),
            (_, _) => return None,
        };

        // When the CPU is offline, this.len() becomes 1
        if this.len() == 1 || this.len() != prev.len() {
            return None;
        }

        let enabled_us = match (this.time_enabled(), prev.time_enabled()) {
            (Some(this), Some(prev)) => (this.as_micros() - prev.as_micros()) as u64,
            (_, _) => return None,
        };

        let running_us = match (this.time_running(), prev.time_running()) {
            (Some(this), Some(prev)) => (this.as_micros() - prev.as_micros()) as u64,
            (_, _) => return None,
        };

        if running_us != enabled_us {
            return None;
        }

        let cycles = match (this.get(&self.cycles), prev.get(&self.cycles)) {
            (Some(this_counter), Some(prev_counter)) => {
                (this_counter.value() - prev_counter.value()) as u64
            }
            (_, _) => return None,
        };
        let instructions = match (this.get(&self.instructions), prev.get(&self.instructions)) {
            (Some(this_counter), Some(prev_counter)) => {
                (this_counter.value() - prev_counter.value()) as u64
            }
            (_, _) => return None,
        };
        if cycles == 0 || instructions == 0 {
            return None;
        }
        let tsc = match (this.get(&self.tsc), prev.get(&self.tsc)) {
            (Some(this_counter), Some(prev_counter)) => {
                (this_counter.value() - prev_counter.value()) as u64
            }
            (_, _) => return None,
        };
        let mperf = match (this.get(&self.mperf), prev.get(&self.mperf)) {
            (Some(this_counter), Some(prev_counter)) => {
                (this_counter.value() - prev_counter.value()) as u64
            }
            (_, _) => return None,
        };
        let aperf = match (this.get(&self.aperf), prev.get(&self.aperf)) {
            (Some(this_counter), Some(prev_counter)) => {
                (this_counter.value() - prev_counter.value()) as u64
            }
            (_, _) => return None,
        };

        // computer IPKC IPUS BASE_FREQUENCY RUNNING_FREQUENCY
        let ipkc = (instructions * 1000) / cycles;
        let base_frequency_mhz = tsc / running_us;
        let running_frequency_mhz = (base_frequency_mhz * aperf) / mperf;
        let ipus = (ipkc * aperf) / mperf;

        return Some((
            cycles,
            instructions,
            ipkc,
            ipus,
            base_frequency_mhz,
            running_frequency_mhz,
        ));
    }
}

pub struct Cpi {
    prev: Instant,
    next: Instant,
    interval: Duration,
    groups: Vec<PerfGroup>,
}

impl Cpi {
    pub fn new(_config: &Config) -> Result<Self, ()> {
        let now = Instant::now();
        // initialize the groups
        let mut groups = vec![];

        let cpus = match hardware_info() {
            Ok(hwinfo) => hwinfo.get_cpus(),
            Err(_) => return Err(()),
        };

        for cpu in cpus {
            match PerfGroup::new(cpu.get_cpuid()) {
                Ok(g) => groups.push(g),
                Err(_) => {
                    warn!("Failed to create the perf group on CPU {}", cpu.get_cpuid());
                    // we want to continue because it's possible that this CPU is offline
                    continue;
                }
            };
        }

        if groups.len() == 0 {
            error!("Failed to create the perf group on any CPU");
            return Err(());
        }

        return Ok(Self {
            prev: now,
            next: now,
            interval: Duration::from_millis(10),
            groups,
        });
    }
}

impl Sampler for Cpi {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let mut nr_active_groups: u64 = 0;
        let mut total_cycles = 0;
        let mut total_instructions = 0;
        let mut avg_ipkc = 0;
        let mut avg_ipus = 0;
        let mut avg_base_frequency = 0;
        let mut avg_running_frequency = 0;

        for group in &mut self.groups {
            group.update_group();
            if let Some((
                cycles,
                instructions,
                ipkc,
                ipus,
                base_frequency_mhz,
                running_frequency_mhz,
            )) = group.get_metrics()
            {
                nr_active_groups += 1;
                total_cycles += cycles;
                total_instructions += instructions;
                avg_ipkc += ipkc;
                avg_ipus += ipus;
                avg_base_frequency += base_frequency_mhz;
                avg_running_frequency += running_frequency_mhz;
                CPU_IPKC.increment(now, ipkc, 1);
                CPU_IPUS.increment(now, ipus, 1);
                CPU_RUNNING_FREQUENCY.increment(now, running_frequency_mhz, 1);
            }
        }
        // we increase the total cycles executed in the last sampling period instead of using the cycle perf event value to handle offlined CPUs.
        CPU_CYCLES.set(CPU_CYCLES.value() + total_cycles);
        CPU_INSTRUCTIONS.set(CPU_INSTRUCTIONS.value() + total_instructions);
        CPU_ACTIVE_PERF_GROUPS.set(nr_active_groups as i64);
        CPU_AVG_IPKC.set((avg_ipkc / nr_active_groups) as i64);
        CPU_AVG_IPUS.set((avg_ipus / nr_active_groups) as i64);
        CPU_AVG_BASE_FREQUENCY.set((avg_base_frequency / nr_active_groups) as i64);
        CPU_AVG_RUNNING_FREQUENCY.set((avg_running_frequency / nr_active_groups) as i64);

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
