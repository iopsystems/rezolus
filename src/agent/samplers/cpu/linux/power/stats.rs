use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CPUS, MAX_PACKAGES};
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `cpu/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] pub mod stats` fallback) to keep metric
// identity stable across platforms, same as `cpu_frequency`'s
// `CPU_FREQUENCY_ACQ`.
//
/// Brackets the RAPL energy sweep. Single writer: only `PowerInner::refresh`
/// acquires. One group across every energy domain because the sweep reads
/// them back-to-back in one uninterrupted loop over like entities (RAPL
/// counters, one `read()` each) with no phase boundary between domains, per
/// principle 18's like-entity rule. A single domain's failed `read()` is
/// individually, normally fallible rather than a bulk sweep failure, so there
/// is no discard path (same ruling as `cpu_l3`/`cpu_dtlb`).
pub static CPU_POWER_ENERGY_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_power"),
    "cpu_power_energy_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_POWER_ENERGY_ACQ_REG: &'static AcquisitionGroup = &CPU_POWER_ENERGY_ACQ;

/// Brackets the C-state residency sweep. A separate group from the energy
/// sweep above: idle residency is a different metric family read from
/// different PMUs (`cstate_core`/`cstate_pkg` rather than `power`), and
/// principle 18 keeps families apart even when read back-to-back.
pub static CPU_POWER_CSTATE_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_power"),
    "cpu_power_cstate_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_POWER_CSTATE_ACQ_REG: &'static AcquisitionGroup = &CPU_POWER_CSTATE_ACQ;

// Each metric is indexed by the logical CPU id that the perf counter was
// created on, which is the id the PMU's cpumask designated as the reader for
// that domain. For package-scope PMUs that is one CPU per package; for
// core-scope PMUs it is one CPU per physical core.
//
// Group sizes follow the scope of the underlying counter: package-scope
// domains are sized to MAX_PACKAGES, core-scope domains to MAX_CPUS, and the
// platform domain to a single entry since it covers the whole host.

// Energy
//
// Sourced from the RAPL energy accumulators exposed by the `power` and
// `power_core` PMUs. These are monotonic counters, which is the form worth
// keeping: power is a derivative that can always be recomputed from energy,
// while energy cannot be reconstructed from sampled power.
//
// The perf events report joules via a scale factor. We accumulate microjoules
// so that the hardware quantum (61.035 uJ on Intel, 15.259 uJ on AMD Zen) is
// preserved without needing floats in the metric layer.
//
// Energy is the only form reported. Power is its derivative and is better
// computed at query time as `irate(cpu_<domain>_energy) / 1e6`, which is
// correct over any window. A sampled power gauge would only ever describe the
// sampler's own refresh interval, so a scrape landing mid-interval reads a
// stale value, and an integer-milliwatt gauge truncates a microwatt-scale
// domain (an idle iGPU) to zero while its energy counter still advances.

#[metric(
    name = "cpu_package_energy",
    description = "The cumulative energy consumed by the CPU package, including cores, uncore, and integrated graphics.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_energy_sweep" }
)]
pub static CPU_PACKAGE_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_cores_energy",
    description = "The cumulative energy consumed by all CPU cores in the package.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_energy_sweep" }
)]
pub static CPU_CORES_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_core_energy",
    description = "The cumulative energy consumed by a single CPU core.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_energy_sweep" }
)]
pub static CPU_CORE_ENERGY: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_igpu_energy",
    description = "The cumulative energy consumed by the integrated graphics.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_energy_sweep" }
)]
pub static CPU_IGPU_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_dram_energy",
    description = "The cumulative energy consumed by the DRAM attached to this package.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_energy_sweep" }
)]
pub static CPU_DRAM_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_platform_energy",
    description = "The cumulative energy consumed by the whole platform.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_energy_sweep" }
)]
pub static CPU_PLATFORM_ENERGY: CounterGroup = CounterGroup::new(1);

// C-state residency
//
// Sourced from the `cstate_core` and `cstate_pkg` PMUs. The counters tick in
// TSC cycles spent in each idle state, so a residency fraction is
// `rate(core_cN_residency) / rate(cpu_tsc)` in post-processing. We report the
// raw cycles rather than a precomputed percentage so that the ratio can be
// taken over any window without resampling error.
//
// Which levels a part exposes varies, so a metric is defined for every level
// the PMUs are known to expose and only the ones present are ever written.

#[metric(
    name = "core_c1_residency",
    description = "The cumulative TSC cycles a physical core spent in the C1 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_C1: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c2_residency",
    description = "The cumulative TSC cycles a physical core spent in the C2 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_C2: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c3_residency",
    description = "The cumulative TSC cycles a physical core spent in the C3 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_C3: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c6_residency",
    description = "The cumulative TSC cycles a physical core spent in the C6 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_C6: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c7_residency",
    description = "The cumulative TSC cycles a physical core spent in the C7 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_C7: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c8_residency",
    description = "The cumulative TSC cycles a physical core spent in the C8 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_C8: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c9_residency",
    description = "The cumulative TSC cycles a physical core spent in the C9 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_C9: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c10_residency",
    description = "The cumulative TSC cycles a physical core spent in the C10 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_C10: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "package_c1_residency",
    description = "The cumulative TSC cycles a package spent in the C1 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static PACKAGE_C1: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c2_residency",
    description = "The cumulative TSC cycles a package spent in the C2 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static PACKAGE_C2: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c3_residency",
    description = "The cumulative TSC cycles a package spent in the C3 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static PACKAGE_C3: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c6_residency",
    description = "The cumulative TSC cycles a package spent in the C6 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static PACKAGE_C6: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c7_residency",
    description = "The cumulative TSC cycles a package spent in the C7 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static PACKAGE_C7: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c8_residency",
    description = "The cumulative TSC cycles a package spent in the C8 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static PACKAGE_C8: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c9_residency",
    description = "The cumulative TSC cycles a package spent in the C9 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static PACKAGE_C9: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c10_residency",
    description = "The cumulative TSC cycles a package spent in the C10 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static PACKAGE_C10: CounterGroup = CounterGroup::new(MAX_PACKAGES);

// The sum of every core C-state level a part exposes, per core. Individual
// levels are useful for understanding which idle state the hardware chose, but
// the question asked most often is simply how much of the time a core was idle
// at all. Summing the levels in post-processing requires knowing which ones
// this part exposes, so we do it here where that set is already known.

#[metric(
    name = "core_cstate_residency",
    description = "The cumulative TSC cycles a physical core spent in any idle C-state, summed across every level the hardware exposes. Divide by cpu_tsc for a total idle residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_cstate_sweep" }
)]
pub static CORE_CSTATE: CounterGroup = CounterGroup::new(MAX_CPUS);
