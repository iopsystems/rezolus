mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_usage.bpf.rs"));
}

use super::NAME;

use metriken::MetricBuilder;

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::cpu::stats::*;
use crate::samplers::cpu::*;
use crate::samplers::hwinfo::hardware_info;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.obj.map(name).unwrap()
    }
}

/// Collects Scheduler Runqueue Latency stats using BPF and traces:

///
/// And produces these stats:

pub struct CpuUsage {
    bpf: Bpf<ModSkel<'static>>,
    counter_interval: Duration,
    counter_next: Instant,
    counter_prev: Instant,
    distribution_interval: Duration,
    distribution_next: Instant,
    distribution_prev: Instant,
}

impl CpuUsage {
    pub fn new(config: &Config) -> Result<Self, ()> {
        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open()
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let mut bpf = Bpf::from_skel(skel);

        let cpus = match hardware_info() {
            Ok(hwinfo) => hwinfo.get_cpus(),
            Err(_) => return Err(()),
        };

        let counters = vec![
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

        let mut percpu_counters = PercpuCounters::default();

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
        }

        bpf.add_counters_with_percpu("counters", counters, percpu_counters);

        let mut distributions = vec![];

        for (name, histogram) in distributions.drain(..) {
            bpf.add_distribution(name, histogram);
        }

        Ok(Self {
            bpf,
            counter_interval: config.interval(NAME),
            counter_next: Instant::now(),
            counter_prev: Instant::now(),
            distribution_interval: config.distribution_interval(NAME),
            distribution_next: Instant::now(),
            distribution_prev: Instant::now(),
        })
    }

    pub fn refresh_counters(&mut self, now: Instant) {
        if now < self.counter_next {
            return;
        }

        let elapsed = (now - self.counter_prev).as_secs_f64();

        self.bpf.refresh_counters(elapsed);

        // determine when to sample next
        let next = self.counter_next + self.counter_interval;

        // check that next sample time is in the future
        if next > now {
            self.counter_next = next;
        } else {
            self.counter_next = now + self.counter_interval;
        }

        // mark when we last sampled
        self.counter_prev = now;
    }

    pub fn refresh_distributions(&mut self, now: Instant) {
        if now < self.distribution_next {
            return;
        }

        self.bpf.refresh_distributions();

        // determine when to sample next
        let next = self.distribution_next + self.distribution_interval;

        // check that next sample time is in the future
        if next > now {
            self.distribution_next = next;
        } else {
            self.distribution_next = now + self.distribution_interval;
        }

        // mark when we last sampled
        self.distribution_prev = now;
    }
}

impl Sampler for CpuUsage {
    fn sample(&mut self) {
        let now = Instant::now();
        self.refresh_counters(now);
        self.refresh_distributions(now);
    }
}
