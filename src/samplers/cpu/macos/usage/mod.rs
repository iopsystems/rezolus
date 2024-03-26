use crate::common::{Counter, Nop};
use crate::samplers::cpu::*;
use crate::{distributed_slice, Config, Sampler};
use core::time::Duration;
use libc::mach_port_t;
use metriken::{DynBoxedMetric, MetricBuilder};
use ringlog::error;
use std::time::Instant;

const NAME: &str = "cpu_usage";

#[distributed_slice(CPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = CpuUsage::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

struct CpuUsage {
    prev: Instant,
    next: Instant,
    interval: Duration,
    port: mach_port_t,
    nanos_per_tick: u64,
    counters_total: Vec<Counter>,
    counters_percpu: Vec<Vec<DynBoxedMetric<metriken::Counter>>>,
}

impl CpuUsage {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let now = Instant::now();

        let cpus = num_cpus::get();

        let counters_total = vec![
            Counter::new(&CPU_USAGE_USER, Some(&CPU_USAGE_USER_HISTOGRAM)),
            Counter::new(&CPU_USAGE_NICE, Some(&CPU_USAGE_NICE_HISTOGRAM)),
            Counter::new(&CPU_USAGE_SYSTEM, Some(&CPU_USAGE_SYSTEM_HISTOGRAM)),
            Counter::new(&CPU_USAGE_IDLE, Some(&CPU_USAGE_IDLE_HISTOGRAM)),
        ];

        let mut counters_percpu = Vec::with_capacity(cpus);

        for cpu in 0..cpus {
            let states = ["user", "nice", "system", "idle"];

            let counters: Vec<DynBoxedMetric<metriken::Counter>> = states
                .iter()
                .map(|state| {
                    MetricBuilder::new("cpu/usage")
                        .metadata("id", format!("{}", cpu))
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
            prev: now,
            next: now,
            interval: config.interval(NAME),
            port: unsafe { libc::mach_host_self() },
            nanos_per_tick,
            counters_total,
            counters_percpu,
        })
    }
}

impl Sampler for CpuUsage {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        if unsafe { self.sample_processor_info(elapsed) }.is_err() {
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

impl CpuUsage {
    unsafe fn sample_processor_info(&mut self, elapsed: f64) -> Result<(), std::io::Error> {
        let mut num_cpu: u32 = 0;
        let mut cpu_info: *mut i32 = std::ptr::null_mut();
        let mut cpu_info_len: u32 = 0;

        let mut total_user = 0;
        let mut total_system = 0;
        let mut total_idle = 0;
        let mut total_nice = 0;

        if libc::host_processor_info(
            self.port,
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut num_cpu as *mut u32,
            &mut cpu_info as *mut *mut i32,
            &mut cpu_info_len as *mut u32,
        ) == libc::KERN_SUCCESS
        {
            for cpu in 0..num_cpu {
                let user = (*cpu_info
                    .offset((cpu as i32 * libc::CPU_STATE_MAX + libc::CPU_STATE_USER) as isize)
                    as u64)
                    .wrapping_mul(self.nanos_per_tick);
                let system = (*cpu_info
                    .offset((cpu as i32 * libc::CPU_STATE_MAX + libc::CPU_STATE_SYSTEM) as isize)
                    as u64)
                    .wrapping_mul(self.nanos_per_tick);
                let idle = (*cpu_info
                    .offset((cpu as i32 * libc::CPU_STATE_MAX + libc::CPU_STATE_IDLE) as isize)
                    as u64)
                    .wrapping_mul(self.nanos_per_tick);
                let nice = (*cpu_info
                    .offset((cpu as i32 * libc::CPU_STATE_MAX + libc::CPU_STATE_NICE) as isize)
                    as u64)
                    .wrapping_mul(self.nanos_per_tick);

                self.counters_percpu[cpu as usize][0].set(user);
                self.counters_percpu[cpu as usize][1].set(nice);
                self.counters_percpu[cpu as usize][2].set(system);
                self.counters_percpu[cpu as usize][3].set(idle);

                total_user += user;
                total_system += system;
                total_idle += idle;
                total_nice += nice;
            }

            self.counters_total[0].set(elapsed, total_user);
            self.counters_total[1].set(elapsed, total_nice);
            self.counters_total[2].set(elapsed, total_system);
            self.counters_total[3].set(elapsed, total_idle);
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "failed to refresh processor info",
            ));
        }

        Ok(())
    }
}
