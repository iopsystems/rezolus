//! Metrics for the Apple GPU sampler.
//!
//! Every metric carries `vendor = "apple"`, matching the `amd`/`intel`/`nvidia`
//! values the Linux GPU samplers use and the `"apple"` that `systeminfo`'s
//! macOS summary already reports. The label is what makes a GPU's `id`
//! meaningful: each vendor's sampler numbers its devices from 0, so an id is
//! unique only within a vendor, and the viewer groups per-GPU charts by
//! `(id, vendor)`. An Apple silicon Mac has one GPU, id 0.

use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

const MAX_GPUS: usize = 32;

/// The one IOReport sample each refresh takes, which supplies every metric.
pub static GPU_APPLE_ACQ: AcquisitionGroup = AcquisitionGroup::new(super::NAME, "gpu_apple");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static GPU_APPLE_ACQ_REG: &'static AcquisitionGroup = &GPU_APPLE_ACQ;

#[metric(
    name = "gpu_power_usage",
    description = "The average power usage in milliwatts (mW) since the previous refresh.",
    metadata = { vendor = "apple", unit = "milliwatts", acq_group = "gpu_apple" }
)]
pub static GPU_POWER_USAGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    metadata = { vendor = "apple", unit = "milliJoules", acq_group = "gpu_apple" }
)]
pub static GPU_ENERGY_CONSUMPTION: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The average clock speed in Hertz (Hz) over the time the GPU was in a performance state since the previous refresh; 0 when it was off throughout.",
    metadata = { vendor = "apple", clock = "graphics", unit = "Hz", acq_group = "gpu_apple" }
)]
pub static GPU_CLOCK_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_utilization",
    description = "The percentage of time the GPU was in a performance state, rather than off, since the previous refresh (0-100).",
    metadata = { vendor = "apple", unit = "percentage", acq_group = "gpu_apple" }
)]
pub static GPU_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);
