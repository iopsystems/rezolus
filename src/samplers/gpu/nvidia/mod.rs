use super::stats::*;
use super::*;
use crate::common::Nop;
use nvml_wrapper::enum_wrappers::device::*;
use nvml_wrapper::Nvml;

const KB: i64 = 1024;
const MB: i64 = 1024 * KB;
const MHZ: i64 = 1_000_000;

#[distributed_slice(GPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(nvidia) = Nvidia::new(config) {
        Box::new(nvidia)
    } else {
        Box::new(Nop {})
    }
}

pub struct Nvidia {
    prev: Instant,
    next: Instant,
    interval: Duration,
    nvml: Nvml,
}

impl Nvidia {
    pub fn new(_config: &Config) -> Result<Self, ()> {
        let now = Instant::now();
        let nvml = Nvml::init().map_err(|e| {
            error!("error initializing: {e}");
        })?;

        Ok(Self {
            nvml,
            prev: now,
            next: now,
            interval: Duration::from_millis(50),
        })
    }
}

impl Sampler for Nvidia {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        if let Err(e) = self.sample_nvml(now) {
            error!("error sampling: {e}");
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

impl Nvidia {
    fn sample_nvml(&mut self, now: Instant) -> Result<(), std::io::Error> {
        let devices = self
            .nvml
            .device_count()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        for device_id in 0..devices {
            if let Ok(device) = self.nvml.device_by_index(device_id) {
                /*
                 * power
                 */

                if let Ok(power_usage) = device.power_usage() {
                    // current power usage in mW
                    GPU_POWER_USAGE.set(power_usage as _);
                    let _ = GPU_POWER_USAGE_HEATMAP.increment(now, power_usage as _);
                }

                /*
                 * temperature
                 */

                if let Ok(temperature) = device.temperature(TemperatureSensor::Gpu) {
                    // current die temperature in C
                    GPU_TEMPERATURE.set(temperature as _);
                    let _ = GPU_TEMPERATURE_HEATMAP.increment(now, temperature as _);
                }

                /*
                 * pcie link
                 */

                if let Ok(pcie_throughput_rx) = device.pcie_throughput(PcieUtilCounter::Receive) {
                    // current pcie receive throughput scaled to Bytes/s
                    GPU_PCIE_THROUGHPUT_RX.set(pcie_throughput_rx as i64 * KB);
                    let _ = GPU_PCIE_THROUGHPUT_RX_HEATMAP
                        .increment(now, (pcie_throughput_rx as i64 * KB) as _);
                }

                if let Ok(pcie_throughput_tx) = device.pcie_throughput(PcieUtilCounter::Send) {
                    // current pcie transmit throughput scaled to Bytes/s
                    GPU_PCIE_THROUGHPUT_TX.set(pcie_throughput_tx as i64 * KB);
                    let _ = GPU_PCIE_THROUGHPUT_TX_HEATMAP
                        .increment(now, (pcie_throughput_tx as i64 * KB) as _);
                }

                if let Ok(link_width) = device.current_pcie_link_width() {
                    if let Ok(link_gen) = device.current_pcie_link_gen() {
                        let pcie_link_bandwidth = match link_gen {
                            1 => 250 * MB,
                            2 => 500 * MB,
                            3 => 984 * MB,
                            4 => 1970 * MB,
                            5 => 3940 * MB,
                            6 => 7560 * MB,
                            7 => 15130 * MB,
                            _ => 0,
                        };

                        if pcie_link_bandwidth > 0 {
                            // current device pcie bandwidth scaled to Bytes/s
                            GPU_PCIE_BANDWIDTH.set(pcie_link_bandwidth * link_width as i64);
                            let _ = GPU_PCIE_BANDWIDTH_HEATMAP
                                .increment(now, (pcie_link_bandwidth * link_width as i64) as _);
                        }
                    }
                }

                /*
                 * clocks
                 */

                if let Ok(memory_info) = device.memory_info() {
                    // current memory stats in Bytes
                    GPU_MEMORY_FREE.set(memory_info.free as _);
                    GPU_MEMORY_TOTAL.set(memory_info.total as _);
                    GPU_MEMORY_USED.set(memory_info.used as _);

                    let _ = GPU_MEMORY_FREE_HEATMAP.increment(now, memory_info.free as _);
                    let _ = GPU_MEMORY_TOTAL_HEATMAP.increment(now, memory_info.total as _);
                    let _ = GPU_MEMORY_USED_HEATMAP.increment(now, memory_info.used as _);
                }

                if let Ok(frequency) = device.clock_info(Clock::Graphics) {
                    // current clock frequency scaled to Hz
                    GPU_CLOCK_GRAPHICS.set(frequency as i64 * MHZ);
                    let _ =
                        GPU_CLOCK_GRAPHICS_HEATMAP.increment(now, (frequency as i64 * MHZ) as _);
                }

                if let Ok(frequency) = device.clock_info(Clock::SM) {
                    // current clock frequency scaled to Hz
                    GPU_CLOCK_COMPUTE.set(frequency as i64 * MHZ);
                    let _ = GPU_CLOCK_COMPUTE_HEATMAP.increment(now, (frequency as i64 * MHZ) as _);
                }

                if let Ok(frequency) = device.clock_info(Clock::Memory) {
                    // current clock frequency scaled to Hz
                    GPU_CLOCK_MEMORY.set(frequency as i64 * MHZ);
                    let _ = GPU_CLOCK_MEMORY_HEATMAP.increment(now, (frequency as i64 * MHZ) as _);
                }

                if let Ok(frequency) = device.clock_info(Clock::Video) {
                    // current clock frequency scaled to Hz
                    GPU_CLOCK_VIDEO.set(frequency as i64 * MHZ);
                    let _ = GPU_CLOCK_VIDEO_HEATMAP.increment(now, (frequency as i64 * MHZ) as _);
                }

                if let Ok(frequency) = device.max_clock_info(Clock::Graphics) {
                    // max clock frequency scaled to Hz
                    GPU_MAX_CLOCK_GRAPHICS.set(frequency as i64 * MHZ);
                    let _ = GPU_MAX_CLOCK_GRAPHICS_HEATMAP
                        .increment(now, (frequency as i64 * MHZ) as _);
                }

                if let Ok(frequency) = device.max_clock_info(Clock::SM) {
                    // max clock frequency scaled to Hz
                    GPU_MAX_CLOCK_COMPUTE.set(frequency as i64 * MHZ);
                    let _ =
                        GPU_MAX_CLOCK_COMPUTE_HEATMAP.increment(now, (frequency as i64 * MHZ) as _);
                }

                if let Ok(frequency) = device.max_clock_info(Clock::Memory) {
                    // max clock frequency scaled to Hz
                    GPU_MAX_CLOCK_MEMORY.set(frequency as i64 * MHZ);
                    let _ =
                        GPU_MAX_CLOCK_MEMORY_HEATMAP.increment(now, (frequency as i64 * MHZ) as _);
                }

                if let Ok(frequency) = device.max_clock_info(Clock::Video) {
                    // max clock frequency scaled to Hz
                    GPU_MAX_CLOCK_VIDEO.set(frequency as i64 * MHZ);
                    let _ =
                        GPU_MAX_CLOCK_VIDEO_HEATMAP.increment(now, (frequency as i64 * MHZ) as _);
                }
            }
        }

        Ok(())
    }
}
