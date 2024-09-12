use crate::common::*;
use crate::samplers::cpu::*;
use libc::mach_port_t;
use metriken::{DynBoxedMetric, MetricBuilder};
use ringlog::error;
use std::time::Instant;

const NAME: &str = "cpu_usage";

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> Box<dyn Sampler> {
    if let Ok(s) = CpuUsage::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

struct CpuUsage {
    interval: Interval,
    port: mach_port_t,
    nanos_per_tick: u64,
    counters_total: Vec<Counter>,
    counters_percpu: Vec<Vec<DynBoxedMetric<metriken::Counter>>>,
}

impl CpuUsage {
    pub fn new(config: Arc<Config>) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let cpus = num_cpus::get();

        let counters_total = vec![
            Counter::new(&CPU_USAGE_USER, None),
            Counter::new(&CPU_USAGE_NICE, None),
            Counter::new(&CPU_USAGE_SYSTEM, None),
            Counter::new(&CPU_USAGE_IDLE, None),
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
            interval: Interval::new(Instant::now(), config.interval(NAME)),
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

        if let Ok(elapsed) = self.interval.try_wait(now) {
            METADATA_CPU_USAGE_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

            unsafe {
                let _ = self.sample_processor_info(elapsed.as_secs_f64());
            }

            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_CPU_USAGE_RUNTIME.add(elapsed);
            let _ = METADATA_CPU_USAGE_RUNTIME_HISTOGRAM.increment(elapsed);
        }
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
        let mut total_busy = 0;

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
                let busy = user.wrapping_add(system.wrapping_add(nice));

                self.counters_percpu[cpu as usize][0].set(user);
                self.counters_percpu[cpu as usize][1].set(nice);
                self.counters_percpu[cpu as usize][2].set(system);
                self.counters_percpu[cpu as usize][3].set(idle);
                self.counters_percpu[cpu as usize][4].set(busy);

                total_user += user;
                total_system += system;
                total_idle += idle;
                total_nice += nice;
                total_busy += busy;
            }

            self.counters_total[0].set(elapsed, total_user);
            self.counters_total[1].set(elapsed, total_nice);
            self.counters_total[2].set(elapsed, total_system);
            self.counters_total[3].set(elapsed, total_idle);
            self.counters_total[4].set(elapsed, total_busy);
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "failed to refresh processor info",
            ));
        }

        Ok(())
    }
}
