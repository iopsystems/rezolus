use super::stats::*;
use super::*;
use crate::common::{Counter, Nop};
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

/// Per-cpu perf event group consists of a set of perf counters
struct PerfGroup {
    cpu: usize,
    cycles: perf_event::Counter,
    instructions: perf_event::Counter,
    tsc: perf_event::Counter,
    aperf: perf_event::Counter,
    mperf: perf_event::Counter,
}

impl PerfGroup {
    pub fn new(cpu: usize) -> Result<Self, ()> {
        let mut cycles = match Builder::new(Hardware::CPU_CYCLES)
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .read_format(
                ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
            )
            .build()
        {
            Ok(counter) => counter,
            Err(_) => {
                error!("failed to initialize cpu cycles perf counter");
                return Err(());
            }
        };

        let instructions = match Builder::new(Hardware::INSTRUCTIONS)
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
        {
            Ok(counter) => counter,
            Err(_) => {
                error!("failed to initialize cpu instructions perf counter");
                return Err(());
            }
        };

        let tsc_event = match Msr::new(MsrId::TSC) {
            Ok(e) => e,
            Err(_) => return Err(()),
        };
        let tsc = match Builder::new(tsc_event)
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
        {
            Ok(e) => e,
            Err(_) => return Err(()),
        };

        let aperf_event = match Msr::new(MsrId::APERF) {
            Ok(e) => e,
            Err(_) => return Err(()),
        };
        let aperf = match Builder::new(aperf_event)
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
        {
            Ok(e) => e,
            Err(_) => return Err(()),
        };

        let mperf_event = match Msr::new(MsrId::MPERF) {
            Ok(e) => e,
            Err(_) => return Err(()),
        };
        let mperf = match Builder::new(mperf_event)
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .build_with_group(&mut cycles)
        {
            Ok(e) => e,
            Err(_) => return Err(()),
        };
        // enable the group
        match cycles.enable_group() {
            Ok(_) => Ok(Self {
                cpu,
                cycles,
                instructions,
                tsc,
                aperf,
                mperf,
            }),
            Err(_) => Err(()),
        }
    }
    pub fn read_group(&mut self) -> Result<GroupData, std::io::Error> {
        return self.cycles.read_group();
    }
}

pub struct Cpi {
    prev: Instant,
    next: Instant,
    interval: Duration,
    groups: Vec<PerfGroup>,
    // we need to keep the last perf event readings because we need to compute gauge IPC, IPNS, and running CPU frequency.
    prev_group_data: Option<Vec<GroupData>>,
    total_cycles: Counter,
    total_instructions: Counter,
}

impl Cpi {
    pub fn new(_config: &Config) -> Result<Self, ()> {
        let now = Instant::now();
        // initialize the groups
        let mut groups = vec![];
        let mut prev_group_data = vec![];
        let total_cycles = Counter::new(&CPU_CYCLES, None);
        let total_instructions = Counter::new(&CPU_INSTRUCTIONS, None);

        let nr_cpu = match hardware_info() {
            Ok(hwinfo) => hwinfo.get_cpusize(),
            Err(_) => return Err(()),
        };

        for cpu in 0..nr_cpu {
            let mut group = match PerfGroup::new(cpu) {
                Ok(g) => g,
                Err(_) => return Err(()),
            };
            match group.read_group() {
                Ok(r) => prev_group_data.push(r),
                Err(_) => return Err(()),
            }
            groups.push(group);
        }

        return Ok(Self {
            prev: now,
            next: now,
            interval: Duration::from_millis(10),
            groups,
            prev_group_data: Some(prev_group_data),
            total_cycles,
            total_instructions,
        });
    }
}

impl Sampler for Cpi {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        let mut total_cycles = 0;
        let mut total_instructions = 0;
        let mut base_frequency = 1;
        let mut cur_group_data = vec![];
        for group in &mut self.groups {
            match group.read_group() {
                Ok(counts) => {
                    let running_us;
                    match (counts.time_enabled(), counts.time_running()) {
                        (Some(enable_time), Some(running_time)) => {
                            if enable_time.as_nanos() != running_time.as_nanos() {
                                error!("The perf group {} is not always running", group.cpu);
                                continue;
                            }
                            running_us = running_time.as_micros()
                        }
                        (_, _) => {
                            error!(
                                "The perf group {} has no enabled time and running time",
                                group.cpu
                            );
                            continue;
                        }
                    }
                    if counts.time_enabled() != counts.time_running() {}
                    let cycles = counts[&(group.cycles)];
                    let instructions = counts[&(group.instructions)];

                    total_cycles += cycles;
                    total_instructions += instructions;
                    let tsc = counts[&(group.tsc)];
                    base_frequency = tsc / running_us as u64;
                    BASEFREQUENCY.set(base_frequency as i64);
                    cur_group_data.push(counts);
                }
                Err(e) => {
                    error!("error in sampling the perf group: {e}");
                }
            };
        }

        if cur_group_data.len() == self.groups.len() {
            // update total cycles and instructions
            self.total_cycles.set(now, elapsed, total_cycles);
            self.total_instructions
                .set(now, elapsed, total_instructions);
            // update the IPC and IPNS
            if let Some(prev_group_data) = &self.prev_group_data {
                let nr_cpu = cur_group_data.len() as i64;
                if cur_group_data.len() == prev_group_data.len() {
                    let mut ipc: i64 = 0;
                    let mut avg_frequency = 0;
                    let mut ipns: i64 = 0;
                    for cpu in 0..cur_group_data.len() {
                        let cur_cycles = cur_group_data[cpu][&(self.groups[cpu].cycles)];
                        let prev_cycles = prev_group_data[cpu][&(self.groups[cpu].cycles)];
                        let cur_instructions =
                            cur_group_data[cpu][&(self.groups[cpu].instructions)];
                        let prev_instructions =
                            prev_group_data[cpu][&(self.groups[cpu].instructions)];
                        let cpu_ipc = (cur_instructions - prev_instructions) * 1000
                            / (cur_cycles - prev_cycles);
                        ipc += cpu_ipc as i64;
                        let cur_mperf = cur_group_data[cpu][&(self.groups[cpu].mperf)];
                        let prev_mperf = prev_group_data[cpu][&(self.groups[cpu].mperf)];
                        let cur_aperf = cur_group_data[cpu][&(self.groups[cpu].aperf)];
                        let prev_aperf = prev_group_data[cpu][&(self.groups[cpu].aperf)];
                        let ratio =
                            (cur_aperf - prev_aperf) as f64 / (cur_mperf - prev_mperf) as f64;
                        let running_frequency = (ratio * base_frequency as f64) as i64;
                        avg_frequency += running_frequency;
                        ipns += (cpu_ipc as f64 * ratio) as i64;
                    }
                    RUNNINGFREQUENCY.set(avg_frequency / nr_cpu);
                    IPKC.set(ipc / nr_cpu);
                    IPKNS.set(ipns / nr_cpu);
                }
            }
        }

        self.prev_group_data = Some(cur_group_data);
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
