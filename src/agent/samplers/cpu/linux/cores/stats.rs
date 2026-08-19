use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `cpu/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] pub mod stats` fallback) to keep metric
// identity stable across platforms. Same cross-platform-name mechanism as
// the BPF samplers in this family (e.g. `cpu_migrations`'s `MIGRATIONS_ACQ`)
// — see `crate::agent::samplers::bpf_sampler_name`'s doc comment.
//
/// Brackets the single `/sys/devices/system/cpu/online` read + parse (single
/// writer: this sampler's own `refresh()`).
pub static CPU_CORES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_cores"),
    "cpu_cores_read",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_CORES_ACQ_REG: &'static AcquisitionGroup = &CPU_CORES_ACQ;

#[metric(
    name = "cpu_cores",
    description = "The total number of logical cores that are currently online",
    metadata = { acq_group = "cpu_cores_read" }
)]
pub static CPU_CORES: LazyGauge = LazyGauge::new(Gauge::default);
