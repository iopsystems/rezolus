const ONLINE_CORES_REFRESH: Duration = Duration::from_secs(1);

#[allow(clippy::module_inception)]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_usage.bpf.rs"));
}

use super::NAME;

use std::io::{Read, Seek};

use metriken::{DynBoxedMetric, MetricBuilder};

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::cpu::*;
use crate::samplers::hwinfo::hardware_info;

const MAX_CPUS: usize = 1024;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.obj.map(name).unwrap()
    }
}

/// Collects CPU Usage stats using BPF and traces:
/// * __cgroup_account_cputime_field
///
/// And produces these stats:
/// * cpu/usage/*

pub struct CpuUsage {
    bpf: Bpf<ModSkel<'static>>,
    percpu_counters: Arc<PercpuCounters>,
    total_busy: Counter,
    total_idle: Counter,
    percpu_busy: Vec<DynBoxedMetric<metriken::Counter>>,
    percpu_idle: Vec<DynBoxedMetric<metriken::Counter>>,
    counter_interval: Interval,
    distribution_interval: Interval,
    online_cores: OnlineCores,
    online_cores_interval: Interval,
}

pub struct OnlineCores {
    count: usize,
    cpus: [bool; MAX_CPUS],
    file: std::fs::File,
}

impl OnlineCores {
    pub fn new() -> Result<Self, ()> {
        let file = std::fs::File::open("/sys/devices/system/cpu/online")
            .map_err(|e| error!("couldn't open: {e}"))?;

        let mut online_cores = OnlineCores {
            count: 0,
            cpus: [false; MAX_CPUS],
            file,
        };

        let _ = online_cores.refresh();

        online_cores
    }

    pub fn refresh(&mut self) -> Result<(), ()> {
        self.file.rewind()
            .map_err(|e| error!("failed to seek to start of file: {e}"))?;

        let mut count = 0;
        let mut raw = String::new();

        let _ = self.file
            .read_to_string(&mut raw)
            .map_err(|e| error!("failed to read file: {e}"))?;

        for cpu in self.cpus.iter_mut() {
            *cpu = false;
        }

        for range in raw.trim().split(',') {
            let mut parts = range.split('-');

            let first: Option<usize> = parts
                .next()
                .map(|text| text.parse())
                .transpose()
                .map_err(|e| error!("couldn't parse: {e}"))?;
            let second: Option<usize> = parts
                .next()
                .map(|text| text.parse())
                .transpose()
                .map_err(|e| error!("couldn't parse: {e}"))?;

            if parts.next().is_some() {
                // The line is invalid, report error
                return Err(error!("invalid content in file"));
            }

            match (first, second) {
                (Some(value), None) => {
                    self.cpus[value] = true;
                    count += 1;
                }
                (Some(start), Some(stop)) => {
                    for value in start..=stop {
                        self.cpus[value] = true;
                    }
                    count += stop + 1 - start;
                }
                _ => continue,
            }
        }

        self.count = count;
    }
}

impl CpuUsage {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !(config.enabled(NAME) && config.bpf(NAME)) {
            return Err(());
        }

        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open()
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        debug!(
            "{NAME} cpuacct_account_field() BPF instruction count: {}",
            skel.progs().cpuacct_account_field_kprobe().insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let online_cores = OnlineCores::new()
            .map_err(|_| error!("couldn't determine number of online cores"))?;

        let cpus = match hardware_info() {
            Ok(hwinfo) => hwinfo.get_cpus(),
            Err(_) => return Err(()),
        };

        let counters = vec![
            Counter::new(&CPU_USAGE_USER, Some(&CPU_USAGE_USER_HISTOGRAM)),
            Counter::new(&CPU_USAGE_NICE, Some(&CPU_USAGE_NICE_HISTOGRAM)),
            Counter::new(&CPU_USAGE_SYSTEM, Some(&CPU_USAGE_SYSTEM_HISTOGRAM)),
            Counter::new(&CPU_USAGE_SOFTIRQ, Some(&CPU_USAGE_SOFTIRQ_HISTOGRAM)),
            Counter::new(&CPU_USAGE_IRQ, Some(&CPU_USAGE_IRQ_HISTOGRAM)),
            Counter::new(&CPU_USAGE_STEAL, Some(&CPU_USAGE_STEAL_HISTOGRAM)),
            Counter::new(&CPU_USAGE_GUEST, Some(&CPU_USAGE_GUEST_HISTOGRAM)),
            Counter::new(&CPU_USAGE_GUEST_NICE, Some(&CPU_USAGE_GUEST_NICE_HISTOGRAM)),
        ];

        let mut percpu_counters = PercpuCounters::default();
        let mut percpu_busy = Vec::new();
        let mut percpu_idle = Vec::new();

        let states = [
            "user",
            "nice",
            "system",
            "softirq",
            "irq",
            "steal",
            "guest",
            "guest_nice",
        ];

        for cpu in cpus {
            for state in states {
                percpu_counters.push(
                    cpu.id(),
                    MetricBuilder::new("cpu/usage")
                        .metadata("id", format!("{}", cpu.id()))
                        .metadata("core", format!("{}", cpu.core()))
                        .metadata("die", format!("{}", cpu.die()))
                        .metadata("package", format!("{}", cpu.package()))
                        .metadata("state", state)
                        .formatter(cpu_metric_formatter)
                        .build(metriken::Counter::new()),
                );
            }
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
            percpu_idle.push(
                MetricBuilder::new("cpu/usage")
                    .metadata("id", format!("{}", cpu.id()))
                    .metadata("core", format!("{}", cpu.core()))
                    .metadata("die", format!("{}", cpu.die()))
                    .metadata("package", format!("{}", cpu.package()))
                    .metadata("state", "idle")
                    .formatter(cpu_metric_formatter)
                    .build(metriken::Counter::new()),
            );
        }

        let percpu_counters = Arc::new(percpu_counters);

        let bpf = BpfBuilder::new(skel)
            .percpu_counters("counters", counters, percpu_counters.clone())
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            counter_interval: Interval::new(now, config.interval(NAME)),
            distribution_interval: Interval::new(now, config.distribution_interval(NAME)),
            total_busy: Counter::new(&CPU_USAGE_BUSY, Some(&CPU_USAGE_BUSY_HISTOGRAM)),
            total_idle: Counter::new(&CPU_USAGE_IDLE, Some(&CPU_USAGE_IDLE_HISTOGRAM)),
            percpu_counters,
            percpu_busy,
            percpu_idle,
            online_cores,
            online_cores_interval: Interval::new(now, ONLINE_CORES_REFRESH),
        })
    }

    pub fn refresh_counters(&mut self, now: Instant) -> Result<(), ()> {
        let elapsed = self.counter_interval.try_wait(now)?;

        // refresh the counters from the kernel-space counters
        self.bpf.refresh_counters(elapsed);

        // update busy time metric
        let busy: u64 = busy();
        let busy_prev = self.total_busy.value();
        let busy_delta = busy.wrapping_sub(busy_prev);
        self.total_busy.set(elapsed.as_secs_f64(), busy);

        // calculate the idle time elapsed since last sample, update metric
        let idle_prev = self.total_idle.value();
        let idle_delta =
            (self.online_cores.count as u64 * elapsed.as_nanos() as u64).saturating_sub(busy_delta);
        self.total_idle
            .set(elapsed.as_secs_f64(), idle_prev.wrapping_add(idle_delta));

        // do the same for percpu counters
        for (cpu, (busy_counter, idle_counter)) in self
            .percpu_busy
            .iter_mut()
            .zip(self.percpu_idle.iter_mut())
            .enumerate()
        {
            if !self.online_cores.cpus[cpu] {
                continue;
            }

            let busy: u64 = self.percpu_counters.sum(cpu).unwrap_or(0);
            let busy_prev = busy_counter.set(busy);
            let busy_delta = busy.wrapping_sub(busy_prev);

            let idle_delta = (elapsed.as_nanos() as u64).saturating_sub(busy_delta);
            idle_counter.add(idle_delta);
        }

        Ok(())
    }

    pub fn refresh_distributions(&mut self, now: Instant) -> Result<(), ()> {
        self.distribution_interval.try_wait(now)?;
        self.bpf.refresh_distributions();

        Ok(())
    }

    pub fn update_online_cores(&mut self, now: Instant) -> Result<(), ()> {
        self.online_cores_interval.try_wait(now)?;
        self.online_cores.refresh()?;

        Ok(())
    }
}

fn busy() -> u64 {
    [
        &CPU_USAGE_USER,
        &CPU_USAGE_NICE,
        &CPU_USAGE_SYSTEM,
        &CPU_USAGE_SOFTIRQ,
        &CPU_USAGE_IRQ,
        &CPU_USAGE_STEAL,
        &CPU_USAGE_GUEST,
        &CPU_USAGE_GUEST_NICE,
    ]
    .iter()
    .map(|v| v.value())
    .sum()
}

impl Sampler for CpuUsage {
    fn sample(&mut self) {
        let now = Instant::now();
        let _ = self.update_online_cores(now);
        let _ = self.refresh_counters(now);
        let _ = self.refresh_distributions(now);
    }
}
