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
// Acquisition groups are split by ENTITY TYPE, not by read loop.
//
// Principle 18 collapses like entities within a metric family into one group
// but keeps families apart, and it makes a declared group's member set a
// promise: the exposition layer drops its "skip a zero-valued slot" fallback
// once a group is declared, so every declared index emits a series whether a
// reader wrote it or not.
//
// That promise is per group, and `members()` walks one index list for every
// metric in the group. So a group must only hold metrics that share an entity
// space -- otherwise the set is a union across incompatible domains and the
// surplus indices publish fabricated zeros. Grouping package-scope and
// core-scope energy together did exactly that: on a 16-thread hybrid host the
// union of the core cpumask (`0,2,4,6,8,10,12-15`) with the package cpumask
// (`0`) got clamped to each metric's backing array, and `package_cN_residency`
// -- which has exactly one real package -- published four.
//
// The entity space is precisely the `Index` variant each event is opened with,
// so that enum is the partition: `Ordinal` (package), `Cpu` (physical core),
// `Zero` (host). A domain whose PMU is absent contributes no readers, so its
// group registers an empty member set and its metrics emit nothing -- which is
// the honest answer, and what a fabricated zero would have hidden.

macro_rules! power_acq {
    ($vis_static:ident, $reg:ident, $name:literal, $doc:literal) => {
        #[doc = $doc]
        pub static $vis_static: AcquisitionGroup =
            AcquisitionGroup::new(crate::agent::samplers::bpf_sampler_name("cpu_power"), $name);

        #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
        static $reg: &'static AcquisitionGroup = &$vis_static;
    };
}

power_acq!(
    CPU_POWER_PACKAGE_ENERGY_ACQ,
    CPU_POWER_PACKAGE_ENERGY_ACQ_REG,
    "cpu_power_package_energy",
    "Package-scope RAPL domains (`energy-pkg`, `energy-cores`, `energy-gpu`, \
     `energy-ram`): one member per package, indexed by cpumask ordinal."
);

power_acq!(
    CPU_POWER_CORE_ENERGY_ACQ,
    CPU_POWER_CORE_ENERGY_ACQ_REG,
    "cpu_power_core_energy",
    "Core-scope RAPL energy (`power_core/energy-core`, AMD): one member per \
     physical core, indexed by the CPU id the kernel designated as its reader."
);

power_acq!(
    CPU_POWER_PLATFORM_ENERGY_ACQ,
    CPU_POWER_PLATFORM_ENERGY_ACQ_REG,
    "cpu_power_platform_energy",
    "Whole-platform RAPL energy (`energy-psys`): a single host-scope member."
);

power_acq!(
    CPU_POWER_CORE_CSTATE_ACQ,
    CPU_POWER_CORE_CSTATE_ACQ_REG,
    "cpu_power_core_cstate",
    "Per-core idle residency (`cstate_core`), including the summed \
     `core_cstate_residency`: one member per physical core."
);

power_acq!(
    CPU_POWER_PACKAGE_CSTATE_ACQ,
    CPU_POWER_PACKAGE_CSTATE_ACQ_REG,
    "cpu_power_package_cstate",
    "Per-package idle residency (`cstate_pkg`): one member per package."
);

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
    metadata = { unit = "microjoules", acq_group = "cpu_power_package_energy" }
)]
pub static CPU_PACKAGE_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_cores_energy",
    description = "The cumulative energy consumed by all CPU cores in the package.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_package_energy" }
)]
pub static CPU_CORES_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_core_energy",
    description = "The cumulative energy consumed by a single CPU core.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_core_energy" }
)]
pub static CPU_CORE_ENERGY: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_igpu_energy",
    description = "The cumulative energy consumed by the integrated graphics.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_package_energy" }
)]
pub static CPU_IGPU_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_dram_energy",
    description = "The cumulative energy consumed by the DRAM attached to this package.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_package_energy" }
)]
pub static CPU_DRAM_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_platform_energy",
    description = "The cumulative energy consumed by the whole platform.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_platform_energy" }
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
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_C1: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c2_residency",
    description = "The cumulative TSC cycles a physical core spent in the C2 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_C2: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c3_residency",
    description = "The cumulative TSC cycles a physical core spent in the C3 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_C3: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c6_residency",
    description = "The cumulative TSC cycles a physical core spent in the C6 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_C6: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c7_residency",
    description = "The cumulative TSC cycles a physical core spent in the C7 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_C7: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c8_residency",
    description = "The cumulative TSC cycles a physical core spent in the C8 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_C8: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c9_residency",
    description = "The cumulative TSC cycles a physical core spent in the C9 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_C9: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c10_residency",
    description = "The cumulative TSC cycles a physical core spent in the C10 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_C10: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "package_c1_residency",
    description = "The cumulative TSC cycles a package spent in the C1 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_cstate" }
)]
pub static PACKAGE_C1: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c2_residency",
    description = "The cumulative TSC cycles a package spent in the C2 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_cstate" }
)]
pub static PACKAGE_C2: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c3_residency",
    description = "The cumulative TSC cycles a package spent in the C3 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_cstate" }
)]
pub static PACKAGE_C3: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c6_residency",
    description = "The cumulative TSC cycles a package spent in the C6 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_cstate" }
)]
pub static PACKAGE_C6: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c7_residency",
    description = "The cumulative TSC cycles a package spent in the C7 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_cstate" }
)]
pub static PACKAGE_C7: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c8_residency",
    description = "The cumulative TSC cycles a package spent in the C8 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_cstate" }
)]
pub static PACKAGE_C8: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c9_residency",
    description = "The cumulative TSC cycles a package spent in the C9 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_cstate" }
)]
pub static PACKAGE_C9: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c10_residency",
    description = "The cumulative TSC cycles a package spent in the C10 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_cstate" }
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
    metadata = { unit = "cycles", acq_group = "cpu_power_core_cstate" }
)]
pub static CORE_CSTATE: CounterGroup = CounterGroup::new(MAX_CPUS);
