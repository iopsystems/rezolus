const NAME: &str = "gpu_nvidia";

use crate::agent::*;

use nvml_wrapper::enum_wrappers::device::*;
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::Nvml;
use tokio::sync::Mutex;

mod stats;

use stats::*;

const KB: i64 = 1024;
const MB: i64 = 1024 * KB;
const MHZ: i64 = 1_000_000;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = NvidiaInner::new()?;

    Ok(Some(Box::new(Nvidia {
        inner: inner.into(),
    })))
}

struct Nvidia {
    inner: Mutex<NvidiaInner>,
}

#[async_trait]
impl Sampler for Nvidia {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;
        let _ = inner.refresh().await;
    }
}

struct NvidiaInner {
    nvml: Nvml,
    devices: usize,
}

impl NvidiaInner {
    pub fn new() -> Result<Self, NvmlError> {
        let nvml = Nvml::init()?;
        let devices = nvml.device_count()? as usize;

        Ok(Self { nvml, devices })
    }

    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        self.refresh_nvml();
        Ok(())
    }

    fn refresh_nvml(&mut self) {
        for id in 0..self.devices {
            if let Ok(device) = self.nvml.device_by_index(id as _) {
                /*
                 * energy
                 */

                if let Ok(v) = device.total_energy_consumption() {
                    let _ = GPU_ENERGY_CONSUMPTION.set(id, v as _);
                }

                /*
                 * power
                 */

                if let Ok(v) = device.power_usage() {
                    let _ = GPU_POWER_USAGE.set(id, v as _);
                }

                /*
                 * temperature
                 */

                if let Ok(v) = device.temperature(TemperatureSensor::Gpu) {
                    let _ = GPU_TEMPERATURE.set(id, v as _);
                }

                /*
                 * pcie link
                 */

                if let Ok(v) = device
                    .pcie_throughput(PcieUtilCounter::Receive)
                    .map(|v| v as i64 * KB)
                {
                    let _ = GPU_PCIE_THROUGHPUT_RX.set(id, v);
                }

                if let Ok(v) = device
                    .pcie_throughput(PcieUtilCounter::Send)
                    .map(|v| v as i64 * KB)
                {
                    let _ = GPU_PCIE_THROUGHPUT_TX.set(id, v);
                }

                if let Ok(link_width) = device.current_pcie_link_width() {
                    if let Ok(link_gen) = device.current_pcie_link_gen() {
                        let v = match link_gen {
                            1 => 250 * MB,
                            2 => 500 * MB,
                            3 => 984 * MB,
                            4 => 1970 * MB,
                            5 => 3940 * MB,
                            6 => 7560 * MB,
                            7 => 15130 * MB,
                            _ => 0,
                        };

                        if v > 0 {
                            let v = v * link_width as i64;
                            let _ = GPU_PCIE_BANDWIDTH.set(id, v as _);
                        }
                    }
                }

                /*
                 * memory
                 */

                if let Ok(memory_info) = device.memory_info() {
                    let _ = GPU_MEMORY_FREE.set(id, memory_info.free as _);
                    let _ = GPU_MEMORY_USED.set(id, memory_info.used as _);
                }

                /*
                 * clocks
                 */

                if let Ok(frequency) = device.clock_info(Clock::Graphics).map(|f| f as i64 * MHZ) {
                    let _ = GPU_CLOCK_GRAPHICS.set(id, frequency);
                }

                if let Ok(frequency) = device.clock_info(Clock::SM).map(|f| f as i64 * MHZ) {
                    let _ = GPU_CLOCK_COMPUTE.set(id, frequency);
                }

                if let Ok(frequency) = device.clock_info(Clock::Memory).map(|f| f as i64 * MHZ) {
                    let _ = GPU_CLOCK_MEMORY.set(id, frequency);
                }

                if let Ok(frequency) = device.clock_info(Clock::Video).map(|f| f as i64 * MHZ) {
                    let _ = GPU_CLOCK_VIDEO.set(id, frequency);
                }

                /*
                 * utilization
                 */

                if let Ok(utilization) = device.utilization_rates() {
                    let _ = GPU_UTILIZATION.set(id, utilization.gpu as i64);
                    let _ = GPU_MEMORY_UTILIZATION.set(id, utilization.memory as i64);
                }
            }
        }
    }
}
