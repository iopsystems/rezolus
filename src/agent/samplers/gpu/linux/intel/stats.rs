//! Metrics for the Intel GPU PMU sampler.
//!
//! Two sources, and the distinction matters for how each is consumed:
//!
//! **Perf events (counters).** Everything read from the i915/xe PMU is a
//! **monotonically cumulative counter**, so it is published raw and the viewer
//! derives the interesting quantity as a rate:
//!
//! - Engine busy is cumulative **nanoseconds**. `rate(busy[N])` is the fraction
//!   of wall time the engine was executing work, so
//!   `rate(gpu_engine_busy_time[1m]) * 100` is utilization in percent.
//! - The frequency events are **not** instantaneous gauges. The driver adds one
//!   MHz sample per internal tick, and only while the GT is awake, so the raw
//!   counter is a running sum of MHz samples. `rate()` over it recovers average
//!   MHz over the interval — which is exactly the number `perf stat -I` and
//!   `intel_gpu_top` display. Publishing it as a gauge would be wrong, because a
//!   single read carries no information without its predecessor.
//!
//! **DRM query ioctl (gauges).** VRAM is not a PMU counter; it comes from
//! `DRM_IOCTL_I915_QUERY` and is an instantaneous byte count (see `drm_memory`).
//!
//! The `id` label indexes the GPU in discovery order and `device` carries the
//! PCI address (or `integrated`), which is the stable identifier to join on.

use metriken::*;

use super::MAX_GPUS;

// ----- Per-engine occupancy -----
//
// `MAX_GPUS * MAX_ENGINES` wide: entry `gpu * MAX_ENGINES + engine`. The engine
// identity is carried in per-entry metadata set at init, since the engine set
// differs per GPU (only a discrete card has a compute engine).

use super::ENGINE_ENTRIES;

#[metric(
    name = "gpu_engine_busy_time",
    description = "Nanoseconds an engine spent executing work.",
    metadata = { vendor = "intel", unit = "nanoseconds" }
)]
pub static GPU_ENGINE_BUSY: WindowedCounterGroup = WindowedCounterGroup::new(ENGINE_ENTRIES);

// ----- Per-GPU frequency -----
//
// Cumulative sums of per-tick MHz samples. Take `rate()` to get the average
// frequency in MHz over an interval; see the module docs above.
//
// Pair these with engine busy time: the PMU reports occupancy, not efficiency,
// so busy-at-a-low-clock and genuinely saturated look identical without them.

#[metric(
    name = "gpu_frequency_sample",
    description = "Cumulative sum of actual GPU frequency samples in MHz; rate() yields average MHz.",
    metadata = { vendor = "intel", frequency = "actual", unit = "megahertz" }
)]
pub static GPU_FREQUENCY_ACTUAL: WindowedCounterGroup = WindowedCounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_frequency_sample",
    description = "Cumulative sum of requested GPU frequency samples in MHz; rate() yields average MHz.",
    metadata = { vendor = "intel", frequency = "requested", unit = "megahertz" }
)]
pub static GPU_FREQUENCY_REQUESTED: WindowedCounterGroup = WindowedCounterGroup::new(MAX_GPUS);

// ----- VRAM -----
//
// Unlike everything above, these are **gauges in bytes**, matching the
// `gpu_memory{state=used,free}` series the AMD and NVIDIA samplers publish, so
// cross-vendor dashboards work unchanged. They come from the DRM
// `QUERY_MEMORY_REGIONS` ioctl rather than the PMU, which has no memory counter
// (see `drm_memory`).
//
// Only GPUs with device-local memory emit these: an integrated GPU has no VRAM
// region and reports nothing rather than passing host RAM off as VRAM.

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU device memory (VRAM) free.",
    metadata = { vendor = "intel", state = "free", unit = "bytes" }
)]
pub static GPU_MEMORY_FREE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU device memory (VRAM) used.",
    metadata = { vendor = "intel", state = "used", unit = "bytes" }
)]
pub static GPU_MEMORY_USED: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);
