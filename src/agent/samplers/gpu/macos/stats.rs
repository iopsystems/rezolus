//! Metrics for the Apple GPU sampler.
//!
//! Every metric carries `vendor = "apple"`, matching the `amd`/`intel`/`nvidia`
//! values the Linux GPU samplers use and the `"apple"` that `systeminfo`'s
//! macOS summary already reports. The label is what makes a GPU's `id`
//! meaningful: each vendor's sampler numbers its devices from 0, so an id is
//! unique only within a vendor, and the viewer groups per-GPU charts by
//! `(id, vendor)`. Apple GPUs are the only ones this sampler can see — it
//! reads Apple Silicon counters through powermetrics — so the value is static.

use metriken::*;

const MAX_GPUS: usize = 32;

#[metric(
    name = "gpu_power_usage",
    description = "The current power usage in milliwatts (mW).",
    metadata = { vendor = "apple", unit = "milliwatts" }
)]
pub static GPU_POWER_USAGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    metadata = { vendor = "apple", unit = "milliJoules" }
)]
pub static GPU_ENERGY_CONSUMPTION: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "apple", clock = "graphics", unit = "Hz" }
)]
pub static GPU_CLOCK_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_utilization",
    description = "The running average percentage of time the GPU was active. (0-100).",
    metadata = { vendor = "apple", unit = "percentage" }
)]
pub static GPU_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);
