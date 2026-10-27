const NAME: &str = "cpu_usage";

use crate::agent::*;

#[allow(deprecated)]
use libc::mach_host_self;
use libc::mach_port_t;
use tokio::sync::Mutex;

use std::io::Error;

mod stats;

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = UsageInner::new()?;

    Ok(Some(Box::new(Usage {
        inner: Arc::new(Mutex::new(inner)),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

pub struct Usage {
    inner: Arc<Mutex<UsageInner>>,
}

#[async_trait]
impl Sampler for Usage {
    fn name(&self) -> &'static str {
        NAME
    }

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
            #[allow(deprecated)]
            port: unsafe { mach_host_self() },
            nanos_per_tick,
        })
    }

    // `host_processor_info` returns, per CPU, `CPU_STATE_MAX` u32 tick
    // counters; at `CLK_TCK` = 100 Hz each wraps after ~497 days in its state.
    // The kernel allocates the array in this task and the caller must
    // `vm_deallocate` it, or every refresh leaks a page.
    async fn refresh(&mut self) {
        let guard = CPU_USAGE_ACQ.acquire();

        let mut num_cpu: libc::natural_t = 0;
        let mut cpu_info: libc::processor_info_array_t = std::ptr::null_mut();
        let mut cpu_info_len: libc::mach_msg_type_number_t = 0;

        let rc = unsafe {
            libc::host_processor_info(
                self.port,
                libc::PROCESSOR_CPU_LOAD_INFO,
                &mut num_cpu,
                &mut cpu_info,
                &mut cpu_info_len,
            )
        };

        if rc != libc::KERN_SUCCESS {
            return;
        }

        // SAFETY: on success `cpu_info` points to `cpu_info_len` integers.
        let info = unsafe { std::slice::from_raw_parts(cpu_info, cpu_info_len as usize) };

        let cpus = (num_cpu as usize).min(MAX_CPUS);

        let (per_cpu, _) = info.as_chunks::<{ libc::CPU_STATE_MAX as usize }>();

        for (id, ticks) in per_cpu.iter().take(cpus).enumerate() {
            let nanos = |state: i32| (ticks[state as usize] as u32 as u64) * self.nanos_per_tick;

            CPU_USAGE_USER.set(
                id,
                nanos(libc::CPU_STATE_USER) + nanos(libc::CPU_STATE_NICE),
            );
            CPU_USAGE_SYSTEM.set(id, nanos(libc::CPU_STATE_SYSTEM));
        }

        #[allow(deprecated)]
        unsafe {
            libc::vm_deallocate(
                libc::mach_task_self(),
                cpu_info as libc::vm_address_t,
                cpu_info_len as libc::vm_size_t * std::mem::size_of::<libc::integer_t>(),
            );
        }

        CPU_CORES.set(num_cpu as i64);

        CPU_USAGE_ACQ.set_member_bound(cpus);
        guard.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The per-CPU series are published only if their module resolves to this
    // sampler; under any other name the group is unbacked and bounded to zero.
    #[test]
    fn stats_attribute_to_this_sampler() {
        let stats = format!("{}::stats", module_path!().trim_end_matches("::tests"));
        let modules = crate::agent::samplers::sampler_modules();

        assert_eq!(
            crate::agent::samplers::attribute_sampler(&stats, &modules),
            NAME
        );
        assert_eq!(CPU_USAGE_ACQ.sampler, NAME);
    }

    #[tokio::test]
    async fn refresh_sets_every_cpu_and_stamps_the_group() {
        let mut inner = UsageInner::new().unwrap();
        inner.refresh().await;

        let cpus = CPU_CORES.value() as usize;
        assert!(cpus > 0);
        assert_eq!(CPU_USAGE_ACQ.member_bound(), Some(cpus));
        assert!(CPU_USAGE_ACQ.window().is_some());

        for id in 0..cpus {
            assert!(CPU_USAGE_USER.value(id).is_some(), "cpu {id} user unset");
            assert!(
                CPU_USAGE_SYSTEM.value(id).is_some(),
                "cpu {id} system unset"
            );
        }
        assert!(CPU_USAGE_USER.value(cpus).is_none());
    }
}
