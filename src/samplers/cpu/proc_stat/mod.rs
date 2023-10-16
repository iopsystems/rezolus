use super::stats::*;
use super::*;
use crate::common::Counter;
use crate::common::Nop;
use crate::samplers::hwinfo::hardware_info;
use metriken::DynBoxedMetric;
use metriken::MetricBuilder;
use std::fs::File;
use std::io::{Read, Seek};

#[distributed_slice(CPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(procstat) = ProcStat::new(config) {
        Box::new(procstat)
    } else {
        Box::new(Nop {})
    }
}

const NAME: &str = "cpu_proc_stat";

pub struct ProcStat {
    prev: Instant,
    next: Instant,
    interval: Duration,
    nanos_per_tick: u64,
    file: File,
    counters_total: Vec<Counter>,
    counters_percpu: Vec<Vec<DynBoxedMetric<metriken::Counter>>>,
}

impl ProcStat {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let now = Instant::now();

        let cpus = match hardware_info() {
            Ok(hwinfo) => hwinfo.get_cpus(),
            Err(_) => return Err(()),
        };

        let counters_total = vec![
            Counter::new(&CPU_USAGE_USER, Some(&CPU_USAGE_USER_HISTOGRAM)),
            Counter::new(&CPU_USAGE_NICE, Some(&CPU_USAGE_NICE_HISTOGRAM)),
            Counter::new(&CPU_USAGE_SYSTEM, Some(&CPU_USAGE_SYSTEM_HISTOGRAM)),
            Counter::new(&CPU_USAGE_IDLE, Some(&CPU_USAGE_IDLE_HISTOGRAM)),
            Counter::new(&CPU_USAGE_IO_WAIT, Some(&CPU_USAGE_IO_WAIT_HISTOGRAM)),
            Counter::new(&CPU_USAGE_IRQ, Some(&CPU_USAGE_IRQ_HISTOGRAM)),
            Counter::new(&CPU_USAGE_SOFTIRQ, Some(&CPU_USAGE_SOFTIRQ_HISTOGRAM)),
            Counter::new(&CPU_USAGE_STEAL, Some(&CPU_USAGE_STEAL_HISTOGRAM)),
            Counter::new(&CPU_USAGE_GUEST, Some(&CPU_USAGE_GUEST_HISTOGRAM)),
            Counter::new(&CPU_USAGE_GUEST_NICE, Some(&CPU_USAGE_GUEST_NICE_HISTOGRAM)),
        ];

        let mut counters_percpu = Vec::with_capacity(cpus.len());

        for cpu in cpus {
            let states = [
                "user",
                "nice",
                "system",
                "idle",
                "io_wait",
                "irq",
                "softirq",
                "steal",
                "guest",
                "guest_nice",
            ];

            let counters: Vec<DynBoxedMetric<metriken::Counter>> = states
                .iter()
                .map(|state| {
                    MetricBuilder::new("cpu/usage")
                        .metadata("id", format!("{}", cpu.id()))
                        .metadata("core", format!("{}", cpu.core()))
                        .metadata("die", format!("{}", cpu.die()))
                        .metadata("package", format!("{}", cpu.package()))
                        .metadata("state", *state)
                        .formatter(cpu_metric_formatter)
                        .build(metriken::Counter::new())
                })
                .collect();

            counters_percpu.push(counters);
        }

        let sc_clk_tck =
            sysconf::raw::sysconf(sysconf::raw::SysconfVariable::ScClkTck).map_err(|_| {
                error!("Failed to get system clock tick rate");
            })?;

        let nanos_per_tick = 1_000_000_000 / (sc_clk_tck as u64);

        Ok(Self {
            file: File::open("/proc/stat").expect("file not found"),
            counters_total,
            counters_percpu,
            nanos_per_tick,
            prev: now,
            next: now,
            interval: config.interval(NAME),
        })
    }
}

impl Sampler for ProcStat {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        if self.sample_proc_stat(elapsed).is_err() {
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
    fn sample_proc_stat(&mut self, elapsed: f64) -> Result<(), std::io::Error> {
        self.file.rewind()?;

        let mut data = String::new();
        self.file.read_to_string(&mut data)?;

        let lines = data.lines();

        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();

            if parts.is_empty() {
                continue;
            }

            let header = parts.first().unwrap();

            if *header == "cpu" {
                for (field, counter) in self.counters_total.iter_mut().enumerate() {
                    if let Some(Ok(v)) = parts.get(field + 1).map(|v| v.parse::<u64>()) {
                        counter.set(elapsed, v.wrapping_mul(self.nanos_per_tick))
                    }
                }
            } else if header.starts_with("cpu") {
                if let Ok(id) = header.replace("cpu", "").parse::<usize>() {
                    for (field, counter) in self.counters_percpu[id].iter_mut().enumerate() {
                        if let Some(Ok(v)) = parts.get(field + 1).map(|v| v.parse::<u64>()) {
                            counter.set(v.wrapping_mul(self.nanos_per_tick));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
