const NAME: &str = "gpu_nvidia";

use crate::agent::*;

use nvml_wrapper::enum_wrappers::device::*;
use nvml_wrapper::enums::gpm::GpmMetricId;
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::gpm::{gpm_metrics_get, GpmSample};
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
    // SAFETY: gpm_samples must be declared before nvml so that samples are
    // dropped before the Nvml instance they reference. Rust drops struct
    // fields in declaration order.
    gpm_samples: Vec<Option<GpmSample<'static>>>,
    nvml: Nvml,
    devices: usize,
    gpm_supported: Vec<bool>,
}

impl NvidiaInner {
    pub fn new() -> Result<Self, NvmlError> {
        let nvml = Nvml::init()?;
        let devices = nvml.device_count()? as usize;

        let mut gpm_supported = vec![false; devices];

        for id in 0..devices {
            if let Ok(device) = nvml.device_by_index(id as _) {
                if let Ok(supported) = device.gpm_support() {
                    gpm_supported[id] = supported;
                }
            }
        }

        let gpm_samples = (0..devices).map(|_| None).collect();

        Ok(Self {
            gpm_samples,
            nvml,
            devices,
            gpm_supported,
        })
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

                /*
                 * GPM metrics (Hopper+)
                 */

                if self.gpm_supported[id] {
                    if let Ok(new_sample) = device.gpm_sample() {
                        // SAFETY: The sample borrows from self.nvml. We transmute
                        // the lifetime to 'static so we can store it in the struct.
                        // This is sound because gpm_samples is declared before nvml
                        // in the struct, so samples drop before nvml.
                        let new_sample: GpmSample<'static> =
                            unsafe { std::mem::transmute(new_sample) };

                        if let Some(prev_sample) = self.gpm_samples[id].as_ref() {
                            if let Ok(results) = gpm_metrics_get(
                                // SAFETY: transmuting &Nvml back to its true lifetime
                                // for the duration of this call. The reference is valid
                                // because self.nvml is alive.
                                unsafe { std::mem::transmute(&self.nvml) },
                                prev_sample,
                                &new_sample,
                                &[
                                    GpmMetricId::SmUtil,
                                    GpmMetricId::SmOccupancy,
                                    GpmMetricId::DramBwUtil,
                                    GpmMetricId::AnyTensorUtil,
                                ],
                            ) {
                                for result in results.into_iter().flatten() {
                                    match result.metric_id {
                                        GpmMetricId::SmUtil => {
                                            let _ = GPU_SM_UTILIZATION.set(id, result.value as i64);
                                        }
                                        GpmMetricId::SmOccupancy => {
                                            let _ = GPU_SM_OCCUPANCY.set(id, result.value as i64);
                                        }
                                        GpmMetricId::DramBwUtil => {
                                            let _ = GPU_DRAM_BW_UTILIZATION
                                                .set(id, result.value as i64);
                                        }
                                        GpmMetricId::AnyTensorUtil => {
                                            let _ =
                                                GPU_TENSOR_UTILIZATION.set(id, result.value as i64);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }

                        self.gpm_samples[id] = Some(new_sample);
                    }
                }
            }
        }
    }
}
