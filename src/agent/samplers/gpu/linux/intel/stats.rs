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

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

use super::MAX_GPUS;

// `refresh()` visits each GPU once and, per visit, issues a grouped perf
// `read(2)` and a DRM ioctl — one read section by principle 18's "device sweep"
// archetype, device-major by a property of the source (the perf group fd and
// the render node belong to that one device and are reached together).
//
// That single read section nonetheless needs TWO groups, because the metrics it
// fills live in two different index spaces:
//
//   - per-engine metrics are indexed by `engine_index(gpu, engine)`, a
//     `MAX_ENGINES`-wide block per GPU;
//   - per-GPU metrics (frequency, VRAM) are indexed by the plain GPU id.
//
// A group's member set is applied verbatim to every metric tagged with it, so
// one shared group would declare each engine index as a GPU id too. On a
// two-GPU host that published phantom `gpu_frequency_sample{id="2"}` and
// `{id="3"}` series — all-zero, and missing the `device`/`type` labels that
// only real entries carry — which is exactly what the member-set mechanism
// exists to prevent. Caught on hardware, not in review.
//
// Both are stamped from the same bracket, so the two windows are identical and
// nothing is over-stated by the split.
//
// Single writer for both: the `spawn_blocking` task dispatched from
// `Sampler::refresh`, which is guarded against overlapping itself.

/// Window for the per-engine metrics, whose members are [`super::engine_index`]
/// values.
pub static GPU_INTEL_PMU_ENGINE_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(super::NAME, "gpu_intel_pmu_engines");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static GPU_INTEL_PMU_ENGINE_ACQ_REG: &'static AcquisitionGroup = &GPU_INTEL_PMU_ENGINE_ACQ;

/// Window for the per-device metrics, whose members are GPU ids.
pub static GPU_INTEL_PMU_DEVICE_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(super::NAME, "gpu_intel_pmu_devices");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static GPU_INTEL_PMU_DEVICE_ACQ_REG: &'static AcquisitionGroup = &GPU_INTEL_PMU_DEVICE_ACQ;

// ----- Per-engine occupancy -----
//
// `MAX_GPUS * MAX_ENGINES` wide: entry `gpu * MAX_ENGINES + engine`. The engine
// identity is carried in per-entry metadata set at init, since the engine set
// differs per GPU (only a discrete card has a compute engine).

use super::ENGINE_ENTRIES;

#[metric(
    name = "gpu_engine_busy_time",
    description = "Nanoseconds an engine spent executing work.",
    metadata = { acq_group = "gpu_intel_pmu_engines", vendor = "intel", unit = "nanoseconds" }
)]
pub static GPU_ENGINE_BUSY: CounterGroup = CounterGroup::new(ENGINE_ENTRIES);

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
    metadata = { acq_group = "gpu_intel_pmu_devices", vendor = "intel", frequency = "actual", unit = "megahertz" }
)]
pub static GPU_FREQUENCY_ACTUAL: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_frequency_sample",
    description = "Cumulative sum of requested GPU frequency samples in MHz; rate() yields average MHz.",
    metadata = { acq_group = "gpu_intel_pmu_devices", vendor = "intel", frequency = "requested", unit = "megahertz" }
)]
pub static GPU_FREQUENCY_REQUESTED: CounterGroup = CounterGroup::new(MAX_GPUS);

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
    metadata = { acq_group = "gpu_intel_pmu_devices", vendor = "intel", state = "free", unit = "bytes" }
)]
pub static GPU_MEMORY_FREE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU device memory (VRAM) used.",
    metadata = { acq_group = "gpu_intel_pmu_devices", vendor = "intel", state = "used", unit = "bytes" }
)]
pub static GPU_MEMORY_USED: GaugeGroup = GaugeGroup::new(MAX_GPUS);
