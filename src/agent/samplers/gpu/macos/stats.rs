use metriken::*;

use crate::agent::*;

const MAX_GPUS: usize = 32;

#[metric(
    name = "gpu_power_usage",
    description = "The current power usage in milliwatts (mW).",
    metadata = { unit = "milliwatts" }
)]
pub static GPU_POWER_USAGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    metadata = { unit = "milliJoules" }
)]
pub static GPU_ENERGY_CONSUMPTION: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { clock = "graphics", unit = "Hz" }
)]
pub static GPU_CLOCK_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_utilization",
    description = "The running average percentage of time the GPU was active. (0-100).",
    metadata = { unit = "percentage" }
)]
pub static GPU_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);
