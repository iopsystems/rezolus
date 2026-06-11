//! Collects AMD GPU telemetry via the ROCm SMI library.
//!
//! The library (`librocm_smi64.so`) is loaded at runtime with `dlopen` (see
//! [`rocm_smi`]) rather than linked at build time, so the agent compiles on
//! hosts without ROCm installed. On a host with no AMD GPU or no ROCm runtime
//! the sampler gracefully returns `Ok(None)` at init time and reports as
//! disabled (not failed).

const NAME: &str = "gpu_amd_smi";

use crate::agent::*;

use tokio::sync::Mutex;

mod rocm_smi;
mod stats;

use rocm_smi::{ClockType, RocmSmi, TempSensor};
use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = match AmdInner::new() {
        Ok(Some(inner)) => inner,
        Ok(None) => {
            debug!("{NAME}: no AMD GPUs found");
            return Ok(None);
        }
        Err(e) => {
            debug!("{NAME}: failed to initialize: {e}");
            return Ok(None);
        }
    };

    Ok(Some(Box::new(Amd {
        inner: inner.into(),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry =
    crate::agent::samplers::SamplerEntry { name: NAME, init };

struct Amd {
    inner: Mutex<AmdInner>,
}

#[async_trait]
impl Sampler for Amd {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;
        inner.refresh();
    }
}

struct AmdInner {
    rocm: RocmSmi,
    devices: usize,
}

impl AmdInner {
    fn new() -> Result<Option<Self>, String> {
        let rocm = RocmSmi::new()?;
        let devices = rocm
            .num_devices()
            .map_err(|_| "rsmi_num_monitor_devices failed")?;

        if devices == 0 {
            return Ok(None);
        }

        Ok(Some(Self { rocm, devices }))
    }

    fn refresh(&mut self) {
        for id in 0..self.devices {
            /*
             * memory
             */

            if let (Ok(total), Ok(used)) = (self.rocm.memory_total(id), self.rocm.memory_used(id)) {
                let _ = GPU_MEMORY_USED.set(id, used as i64);
                let _ = GPU_MEMORY_FREE.set(id, total.saturating_sub(used) as i64);
            }

            /*
             * utilization
             */

            if let Ok(v) = self.rocm.busy_percent(id) {
                let _ = GPU_UTILIZATION.set(id, v as i64);
            }

            if let Ok(v) = self.rocm.memory_busy_percent(id) {
                let _ = GPU_MEMORY_UTILIZATION.set(id, v as i64);
            }

            /*
             * temperature
             */

            if let Ok(v) = self.rocm.temperature(id, TempSensor::Edge) {
                let _ = GPU_TEMPERATURE_EDGE.set(id, v);
            }

            if let Ok(v) = self.rocm.temperature(id, TempSensor::Junction) {
                let _ = GPU_TEMPERATURE_JUNCTION.set(id, v);
            }

            if let Ok(v) = self.rocm.temperature(id, TempSensor::Memory) {
                let _ = GPU_TEMPERATURE_MEMORY.set(id, v);
            }

            /*
             * power and energy
             */

            if let Ok(v) = self.rocm.power_milliwatts(id) {
                let _ = GPU_POWER_USAGE.set(id, v as i64);
            }

            if let Ok(v) = self.rocm.energy_millijoules(id) {
                let _ = GPU_ENERGY_CONSUMPTION.set(id, v);
            }

            /*
             * clocks
             */

            if let Ok(hz) = self.rocm.clock_hz(id, ClockType::System) {
                let hz = hz as i64;
                let _ = GPU_CLOCK_GRAPHICS.set(id, hz);
                let _ = GPU_CLOCK_COMPUTE.set(id, hz);
            }

            if let Ok(hz) = self.rocm.clock_hz(id, ClockType::Memory) {
                let _ = GPU_CLOCK_MEMORY.set(id, hz as i64);
            }

            /*
             * pcie throughput
             */

            if let Ok((sent, received)) = self.rocm.pcie_throughput(id) {
                let _ = GPU_PCIE_THROUGHPUT_TX.set(id, sent as i64);
                let _ = GPU_PCIE_THROUGHPUT_RX.set(id, received as i64);
            }
        }
    }
}
