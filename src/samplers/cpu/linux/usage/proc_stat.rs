use crate::common::{Counter, Interval};
use crate::samplers::cpu::*;
use crate::samplers::hwinfo::hardware_info;
use clocksource::precise::UnixInstant;
use metriken::DynBoxedMetric;
use metriken::MetricBuilder;
use std::fs::File;
use std::io::{Read, Seek};

use super::NAME;

const CPU_IDLE_FIELD_INDEX: usize = 3;
const CPU_IO_WAIT_FIELD_INDEX: usize = 4;

pub struct ProcStat {
    interval: Interval,
    nanos_per_tick: u64,
    file: File,
    total_counters: Vec<Counter>,
    total_busy: Counter,
    percpu_counters: Vec<Vec<DynBoxedMetric<metriken::Counter>>>,
    percpu_busy: Vec<DynBoxedMetric<metriken::Counter>>,
}

impl ProcStat {
    pub fn new(config: Arc<Config>) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let cpus = match hardware_info() {
            Ok(hwinfo) => hwinfo.get_cpus(),
            Err(_) => return Err(()),
        };

        let total_counters = vec![
            Counter::new(&CPU_USAGE_USER, None),
            Counter::new(&CPU_USAGE_NICE, None),
            Counter::new(&CPU_USAGE_SYSTEM, None),
            Counter::new(&CPU_USAGE_IDLE,  None),
            Counter::new(&CPU_USAGE_IO_WAIT, None),
            Counter::new(&CPU_USAGE_IRQ,  None),
            Counter::new(&CPU_USAGE_SOFTIRQ,  None),
            Counter::new(&CPU_USAGE_STEAL,  None),
            Counter::new(&CPU_USAGE_GUEST,  None),
            Counter::new(&CPU_USAGE_GUEST_NICE,  None),
        ];

        let mut percpu_counters = Vec::with_capacity(cpus.len());
        let mut percpu_busy = Vec::new();

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

            percpu_counters.push(counters);

            percpu_busy.push(
                MetricBuilder::new("cpu/usage")
                    .metadata("id", format!("{}", cpu.id()))
                    .metadata("core", format!("{}", cpu.core()))
                    .metadata("die", format!("{}", cpu.die()))
                    .metadata("package", format!("{}", cpu.package()))
                    .metadata("state", "busy")
                    .formatter(cpu_metric_formatter)
                    .build(metriken::Counter::new()),
            );
        }

        let sc_clk_tck =
            sysconf::raw::sysconf(sysconf::raw::SysconfVariable::ScClkTck).map_err(|_| {
                error!("Failed to get system clock tick rate");
            })?;

        let nanos_per_tick = 1_000_000_000 / (sc_clk_tck as u64);

        Ok(Self {
            file: File::open("/proc/stat").expect("file not found"),
            total_counters,
            total_busy: Counter::new(&CPU_USAGE_BUSY,  None),
            percpu_counters,
            percpu_busy,
            nanos_per_tick,
            interval: Interval::new(Instant::now(), config.interval(NAME)),
        })
    }
}

impl Sampler for ProcStat {
    fn sample(&mut self) {
        let now = Instant::now();

        if let Ok(elapsed) = self.interval.try_wait(now) {
            METADATA_CPU_USAGE_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

            let _ = self.sample_proc_stat(elapsed.as_secs_f64());

            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_CPU_USAGE_RUNTIME.add(elapsed);
            let _ = METADATA_CPU_USAGE_RUNTIME_HISTOGRAM.increment(elapsed);
        }
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
                let mut busy: u64 = 0;

                for (field, counter) in self.total_counters.iter_mut().enumerate() {
                    if let Some(Ok(v)) = parts.get(field + 1).map(|v| v.parse::<u64>()) {
                        if field != CPU_IDLE_FIELD_INDEX && field != CPU_IO_WAIT_FIELD_INDEX {
                            busy = busy.wrapping_add(v);
                        }
                        counter.set(elapsed, v.wrapping_mul(self.nanos_per_tick));
                    }

                    self.total_busy
                        .set(elapsed, busy.wrapping_mul(self.nanos_per_tick));
                }
            } else if header.starts_with("cpu") {
                if let Ok(id) = header.replace("cpu", "").parse::<usize>() {
                    let mut busy: u64 = 0;

                    for (field, counter) in self.percpu_counters[id].iter_mut().enumerate() {
                        if let Some(Ok(v)) = parts.get(field + 1).map(|v| v.parse::<u64>()) {
                            if field != CPU_IDLE_FIELD_INDEX && field != CPU_IO_WAIT_FIELD_INDEX {
                                busy = busy.wrapping_add(v);
                            }
                            counter.set(v.wrapping_mul(self.nanos_per_tick));
                        }
                    }

                    self.percpu_busy[id].set(busy.wrapping_mul(self.nanos_per_tick));
                }
            }
        }

        Ok(())
    }
}
