use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `memory/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s sampler code is
// Linux-only. A metric declaring `acq_group = "memory_meminfo_read"` must
// find its group registered on every platform that compiles this file, not
// just the one that actually drives it — see
// `crate::agent::samplers::bpf_sampler_name`'s doc comment (the mechanism
// applies to any sampler whose `stats.rs` is compiled cross-platform for
// metric-identity continuity, not just BPF ones).
//
/// Brackets the single `/proc/meminfo` read + parse (single writer: this
/// sampler's own `refresh()`).
pub static MEMINFO_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("memory_meminfo"),
    "memory_meminfo_read",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static MEMINFO_ACQ_REG: &'static AcquisitionGroup = &MEMINFO_ACQ;

#[metric(
    name = "memory_total",
    description = "The total amount of system memory",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_TOTAL: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_free",
    description = "The amount of system memory that is currently free",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_FREE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_available",
    description = "The amount of system memory that is available for allocation",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_AVAILABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_buffers",
    description = "The amount of system memory used for buffers",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_BUFFERS: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_cached",
    description = "The amount of system memory used by the page cache",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_CACHED: LazyGauge = LazyGauge::new(Gauge::default);
