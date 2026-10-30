use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CPUS;
use linkme::distributed_slice;

// The sampler the group belongs to: `cpu_frequency` where a sampler compiles
// this file in its own module (Linux and macOS), `unattributed` elsewhere.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const GROUP_SAMPLER: &str = "cpu_frequency";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const GROUP_SAMPLER: &str = crate::agent::samplers::bpf_sampler_name("cpu_frequency");

// Registered here (not in mod.rs) because this file is also `include!`d by the
// macOS sampler and, on platforms other than Linux and macOS, directly (see
// `cpu/mod.rs`'s `#[cfg(not(target_os = "linux"))] pub mod stats` fallback) to
// keep metric identity stable across platforms, same as `cpu_cores`'s
// `CPU_CORES_ACQ`.
//
/// Brackets the per-CPU perf-event sweep (`FrequencyInner::refresh`, which
/// triggers the dedicated perf-read thread(s) and awaits their completion —
/// see `cpu_dtlb`'s `CPU_DTLB_ACQ` doc comment for the shared "fourth
/// machinery" shape). Single writer per platform: `FrequencyInner::refresh` on
/// Linux, `macos::frequency::Frequency::refresh` on macOS. One group for all
/// three metrics: `Core::refresh()` reads aperf/mperf/tsc for one CPU via a
/// single uninterrupted `read_group()` call, with no phase boundary between
/// the three.
pub static CPU_FREQUENCY_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "cpu_frequency_sweep");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_FREQUENCY_ACQ_REG: &'static AcquisitionGroup = &CPU_FREQUENCY_ACQ;

// per-CPU metrics

#[metric(
    name = "cpu_aperf",
    metadata = { unit = "cycles", acq_group = "cpu_frequency_sweep" }
)]
pub static CPU_APERF: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_mperf",
    metadata = { unit = "cycles", acq_group = "cpu_frequency_sweep" }
)]
pub static CPU_MPERF: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tsc",
    metadata = { unit = "cycles", acq_group = "cpu_frequency_sweep" }
)]
pub static CPU_TSC: CounterGroup = CounterGroup::new(MAX_CPUS);
