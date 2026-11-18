use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

use crate::agent::samplers::gpu::linux::amd::MAX_GPUS;

/// ONE group for the per-device sweep, the "device sweep" read section of
/// principle 18: `gpu_metrics` and `pp_dpm_fclk` for every GPU, in one
/// synchronous loop. Single writer: the `spawn_blocking` task dispatched from
/// `Sampler::refresh`, which is guarded against overlapping itself.
pub static GPU_AMD_SYSFS_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(super::NAME, "gpu_amd_sysfs_devices");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static GPU_AMD_SYSFS_ACQ_REG: &'static AcquisitionGroup = &GPU_AMD_SYSFS_ACQ;

// Clocks

#[metric(
    name = "gpu_clock",
    description = "The graphics clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "graphics", unit = "Hz", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_CLOCK_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The compute clock speed in Hertz (Hz). The graphics clock on these GPUs.",
    metadata = { vendor = "amd", clock = "compute", unit = "Hz", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_CLOCK_COMPUTE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The memory clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "memory", unit = "Hz", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_CLOCK_MEMORY: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The SoC clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "soc", unit = "Hz", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_CLOCK_SOC: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current data fabric DPM level's clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "fabric", unit = "Hz", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_CLOCK_FABRIC: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Power

#[metric(
    name = "gpu_power_usage",
    description = "The socket power in milliwatts (mW), which includes the CPU cores on an APU.",
    metadata = { vendor = "amd", unit = "milliwatts", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_POWER_USAGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_rail_power",
    description = "The power on the VDD rail in milliwatts (mW).",
    metadata = { vendor = "amd", rail = "vdd", unit = "milliwatts", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_RAIL_POWER_VDD: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_rail_power",
    description = "The power on the SoC rail in milliwatts (mW).",
    metadata = { vendor = "amd", rail = "soc", unit = "milliwatts", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_RAIL_POWER_SOC: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Thermals

#[metric(
    name = "gpu_temperature",
    description = "The current graphics temperature in degrees Celsius (C).",
    metadata = { vendor = "amd", sensor = "graphics", unit = "Celsius", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_TEMPERATURE_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_temperature",
    description = "The current SoC temperature in degrees Celsius (C).",
    metadata = { vendor = "amd", sensor = "soc", unit = "Celsius", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_TEMPERATURE_SOC: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Utilization and throttling

#[metric(
    name = "gpu_utilization",
    description = "The percentage of time the graphics engine was busy. (0-100).",
    metadata = { vendor = "amd", unit = "percentage", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_throttle_status",
    description = "The firmware's throttle status bitmask. Bit meanings depend on the SMU firmware.",
    metadata = { vendor = "amd", acq_group = "gpu_amd_sysfs_devices" }
)]
pub static GPU_THROTTLE_STATUS: GaugeGroup = GaugeGroup::new(MAX_GPUS);
