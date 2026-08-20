use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CPUS;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `cpu/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] pub mod stats` fallback) to keep metric
// identity stable across platforms, same as `cpu_cores`'s `CPU_CORES_ACQ`.
//
/// Brackets the per-L3-domain perf-event sweep (`CpuL3Inner::refresh`,
/// which triggers the dedicated perf-read thread(s) and awaits their
/// completion — see `cpu_dtlb`'s `CPU_DTLB_ACQ` doc comment for the shared
/// "fourth machinery" shape). Single writer: only `CpuL3Inner::refresh`
/// calls `acquire()`/`finish()`. One group for both metrics:
/// `L3Cache::refresh()` reads access+miss for one domain in a single
/// uninterrupted `read_group()` call, fanned out to every CPU that shares
/// the domain, with no phase boundary between the two metrics.
pub static CPU_L3_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_l3"),
    "cpu_l3_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_L3_ACQ_REG: &'static AcquisitionGroup = &CPU_L3_ACQ;

// per-CPU metrics

#[metric(
    name = "cpu_l3_access",
    description = "The number of L3 cache access",
    metadata = { acq_group = "cpu_l3_sweep" }
)]
pub static CPU_L3_ACCESS: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_l3_miss",
    description = "The number of L3 cache miss",
    metadata = { acq_group = "cpu_l3_sweep" }
)]
pub static CPU_L3_MISS: CounterGroup = CounterGroup::new(MAX_CPUS);
