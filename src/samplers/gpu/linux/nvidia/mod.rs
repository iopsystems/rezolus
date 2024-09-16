const NAME: &str = "gpu_nvidia";

use crate::common::*;
use crate::samplers::gpu::linux::stats::*;
use crate::*;

use nvml_wrapper::enum_wrappers::device::*;
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::Nvml;
use parking_lot::Mutex;
use tokio::task::spawn_blocking;

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
        inner: Arc::new(Mutex::new(inner)),
    })))
}

pub struct Nvidia {
    inner: Arc<Mutex<NvidiaInner>>,
}

#[async_trait]
impl Sampler for Nvidia {
    async fn refresh(&self) {
        let inner = self.inner.clone();

        // we spawn onto a blocking thread because this can take on the order of
        // tens of milliseconds on large systems

        let _ = spawn_blocking(move || {
            let mut inner = inner.lock();
            let _ = inner.refresh();
        })
        .await;
    }
}

struct NvidiaInner {
    nvml: Nvml,
    pergpu_metrics: Vec<GpuMetrics>,
}

struct GpuMetrics {
    // total energy consumption in millijoules (mJ)
    energy_consumption: DynamicCounter,

    // current power usage in mW
    power_usage: DynamicGauge,

    // current die temperature in C
    temperature: DynamicGauge,

    // current pcie throughput in Bytes/s
    pcie_throughput_rx: DynamicGauge,
    pcie_throughput_tx: DynamicGauge,

    // current pcie bandwidth in Bytes/s
    pcie_bandwidth: DynamicGauge,

    // memory usage in bytes
    memory_free: DynamicGauge,
    memory_used: DynamicGauge,

    // current clock frequencies in Hz
    clock_graphics: DynamicGauge,
    clock_compute: DynamicGauge,
    clock_memory: DynamicGauge,
    clock_video: DynamicGauge,

    // current average gpu utilization as % (0-100)
    gpu_utilization: DynamicGauge,

    // current average gpu memory utilization as % (0-100)
    memory_utilization: DynamicGauge,
}

impl NvidiaInner {
    pub fn new() -> Result<Self, NvmlError> {
        let nvml = Nvml::init()?;

        let devices = nvml.device_count()?;

        let mut pergpu_metrics = Vec::with_capacity(devices as _);

        for device in 0..devices {
            pergpu_metrics.push(GpuMetrics {
                energy_consumption: DynamicCounterBuilder::new("gpu/energy/consumption")
                    .metadata("id", format!("{}", device))
                    .formatter(gpu_metric_formatter)
                    .build(),
                power_usage: DynamicGaugeBuilder::new("gpu/power/usage")
                    .metadata("id", format!("{}", device))
                    .formatter(gpu_metric_formatter)
                    .build(),
                temperature: DynamicGaugeBuilder::new("gpu/temperature")
                    .metadata("id", format!("{}", device))
                    .formatter(gpu_metric_formatter)
                    .build(),
                pcie_throughput_rx: DynamicGaugeBuilder::new("gpu/pcie/throughput")
                    .metadata("id", format!("{}", device))
                    .metadata("direction", "receive")
                    .formatter(gpu_metric_formatter)
                    .build(),
                pcie_throughput_tx: DynamicGaugeBuilder::new("gpu/pcie/throughput")
                    .metadata("id", format!("{}", device))
                    .metadata("direction", "transmit")
                    .formatter(gpu_metric_formatter)
                    .build(),
                pcie_bandwidth: DynamicGaugeBuilder::new("gpu/pcie/bandwidth")
                    .metadata("id", format!("{}", device))
                    .formatter(gpu_metric_formatter)
                    .build(),
                memory_free: DynamicGaugeBuilder::new("gpu/memory")
                    .metadata("id", format!("{}", device))
                    .metadata("state", "free")
                    .formatter(gpu_metric_formatter)
                    .build(),
                memory_used: DynamicGaugeBuilder::new("gpu/memory")
                    .metadata("id", format!("{}", device))
                    .metadata("state", "used")
                    .formatter(gpu_metric_formatter)
                    .build(),
                clock_graphics: DynamicGaugeBuilder::new("gpu/clock")
                    .metadata("id", format!("{}", device))
                    .metadata("type", "graphics")
                    .formatter(gpu_metric_formatter)
                    .build(),
                clock_compute: DynamicGaugeBuilder::new("gpu/clock")
                    .metadata("id", format!("{}", device))
                    .metadata("type", "compute")
                    .formatter(gpu_metric_formatter)
                    .build(),
                clock_memory: DynamicGaugeBuilder::new("gpu/clock")
                    .metadata("id", format!("{}", device))
                    .metadata("type", "memory")
                    .formatter(gpu_metric_formatter)
                    .build(),
                clock_video: DynamicGaugeBuilder::new("gpu/clock")
                    .metadata("id", format!("{}", device))
                    .metadata("type", "video")
                    .formatter(gpu_metric_formatter)
                    .build(),
                gpu_utilization: DynamicGaugeBuilder::new("gpu/utilization")
                    .metadata("id", format!("{}", device))
                    .formatter(gpu_metric_formatter)
                    .build(),
                memory_utilization: DynamicGaugeBuilder::new("gpu/memory_utilization")
                    .metadata("id", format!("{}", device))
                    .formatter(gpu_metric_formatter)
                    .build(),
            });
        }

        Ok(Self {
            nvml,
            pergpu_metrics,
        })
    }

    fn refresh(&mut self) -> Result<(), std::io::Error> {
        // current power usage in mW
        let mut power_usage = 0;

        // current pcie throughput scaled to Bytes/s
        let mut pcie_throughput_rx = 0;
        let mut pcie_throughput_tx = 0;

        // current pcie bandwidth scaled to Bytes/s
        let mut pcie_bandwidth = 0;

        // current memory stats in Bytes
        let mut gpu_memory_free = 0;
        let mut gpu_memory_used = 0;

        // current average utilization (%)
        let mut gpu_utilization = 0;
        let mut gpu_memory_utilization = 0;

        for (device_id, device_metrics) in self.pergpu_metrics.iter().enumerate() {
            if let Ok(device) = self.nvml.device_by_index(device_id as _) {
                /*
                 * energy
                 */

                if let Ok(v) = device.total_energy_consumption() {
                    device_metrics.energy_consumption.set(v as _);
                }

                /*
                 * power
                 */

                if let Ok(v) = device.power_usage() {
                    power_usage += v;
                    device_metrics.power_usage.set(v as _);
                }

                /*
                 * temperature
                 */

                if let Ok(v) = device.temperature(TemperatureSensor::Gpu) {
                    device_metrics.temperature.set(v as _);
                }

                /*
                 * pcie link
                 */

                if let Ok(v) = device
                    .pcie_throughput(PcieUtilCounter::Receive)
                    .map(|v| v as i64 * KB)
                {
                    pcie_throughput_rx += v;
                    device_metrics.pcie_throughput_rx.set(v);
                }

                if let Ok(v) = device
                    .pcie_throughput(PcieUtilCounter::Send)
                    .map(|v| v as i64 * KB)
                {
                    pcie_throughput_tx += v;
                    device_metrics.pcie_throughput_tx.set(v);
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
                            pcie_bandwidth += v;
                            device_metrics.pcie_bandwidth.set(v as _);
                        }
                    }
                }

                /*
                 * memory
                 */

                if let Ok(memory_info) = device.memory_info() {
                    gpu_memory_free += memory_info.free;
                    gpu_memory_used += memory_info.used;
                    device_metrics.memory_free.set(memory_info.free as _);
                    device_metrics.memory_used.set(memory_info.used as _);
                }

                /*
                 * clocks
                 */

                if let Ok(frequency) = device.clock_info(Clock::Graphics).map(|f| f as i64 * MHZ) {
                    device_metrics.clock_graphics.set(frequency);
                }

                if let Ok(frequency) = device.clock_info(Clock::SM).map(|f| f as i64 * MHZ) {
                    device_metrics.clock_compute.set(frequency);
                }

                if let Ok(frequency) = device.clock_info(Clock::Memory).map(|f| f as i64 * MHZ) {
                    device_metrics.clock_memory.set(frequency);
                }

                if let Ok(frequency) = device.clock_info(Clock::Video).map(|f| f as i64 * MHZ) {
                    device_metrics.clock_video.set(frequency);
                }

                /*
                 * utilization
                 */

                if let Ok(utilization) = device.utilization_rates() {
                    gpu_utilization += utilization.gpu / self.pergpu_metrics.len() as u32;
                    gpu_memory_utilization += utilization.memory / self.pergpu_metrics.len() as u32;

                    device_metrics.gpu_utilization.set(utilization.gpu as i64);
                    device_metrics
                        .memory_utilization
                        .set(utilization.memory as i64);
                }
            }
        }

        GPU_POWER_USAGE.set(power_usage as _);

        GPU_PCIE_BANDWIDTH.set(pcie_bandwidth as _);
        GPU_PCIE_THROUGHPUT_RX.set(pcie_throughput_rx as _);
        GPU_PCIE_THROUGHPUT_TX.set(pcie_throughput_tx as _);

        GPU_MEMORY_FREE.set(gpu_memory_free as _);
        GPU_MEMORY_USED.set(gpu_memory_used as _);

        GPU_UTILIZATION.set(gpu_utilization as _);
        GPU_MEMORY_UTILIZATION.set(gpu_memory_utilization as _);

        Ok(())
    }
}
