const NAME: &str = "cpu_usage";

use crate::samplers::cpu::stats::*;
use crate::*;

use libc::mach_host_self;
use libc::mach_port_t;
use tokio::sync::Mutex;

use std::io::Error;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = UsageInner::new()?;

    Ok(Some(Box::new(Usage {
        inner: Arc::new(Mutex::new(inner)),
    })))
}

pub struct Usage {
    inner: Arc<Mutex<UsageInner>>,
}

#[async_trait]
impl Sampler for Usage {
    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

pub struct UsageInner {
    port: mach_port_t,
    nanos_per_tick: u64,
}

impl UsageInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let sc_clk_tck = sysconf::raw::sysconf(sysconf::raw::SysconfVariable::ScClkTck)
            .map_err(|_| Error::other("Failed to get system clock tick rate"))?;

        let nanos_per_tick = 1_000_000_000 / (sc_clk_tck as u64);

        Ok(Self {
            port: unsafe { mach_host_self() },
            nanos_per_tick,
        })
    }

    async fn refresh(&mut self) {
        let mut num_cpu: u32 = 0;
        let mut cpu_info: *mut i32 = std::ptr::null_mut();
        let mut cpu_info_len: u32 = 0;

        let mut user: u64 = 0;
        let mut system: u64 = 0;
        let mut nice: u64 = 0;

        unsafe {
            if libc::host_processor_info(
                self.port,
                libc::PROCESSOR_CPU_LOAD_INFO,
                &mut num_cpu as *mut u32,
                &mut cpu_info as *mut *mut i32,
                &mut cpu_info_len as *mut u32,
            ) == libc::KERN_SUCCESS
            {
                for cpu in 0..num_cpu {
                    user = user.wrapping_add(
                        (*cpu_info.offset(
                            (cpu as i32 * libc::CPU_STATE_MAX + libc::CPU_STATE_USER) as isize,
                        ) as u64)
                            .wrapping_mul(self.nanos_per_tick),
                    );
                    system = system.wrapping_add(
                        (*cpu_info.offset(
                            (cpu as i32 * libc::CPU_STATE_MAX + libc::CPU_STATE_SYSTEM) as isize,
                        ) as u64)
                            .wrapping_mul(self.nanos_per_tick),
                    );
                    nice = nice.wrapping_add(
                        (*cpu_info.offset(
                            (cpu as i32 * libc::CPU_STATE_MAX + libc::CPU_STATE_NICE) as isize,
                        ) as u64)
                            .wrapping_mul(self.nanos_per_tick),
                    );
                }

                let busy = user.wrapping_add(system.wrapping_add(nice));

                CPU_CORES.set(num_cpu as i64);

                CPU_USAGE_USER.set(user);
                CPU_USAGE_SYSTEM.set(system);
                CPU_USAGE_NICE.set(nice);
                CPU_USAGE_BUSY.set(busy);
            }
        }
    }
}
