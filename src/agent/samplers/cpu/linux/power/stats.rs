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
// Acquisition groups are split PER METRIC, not per entity space or read loop.
//
// Principle 18 collapses like entities within a metric family into one group
// but keeps families apart, and it makes a declared group's member set a
// promise: the exposition layer drops its "skip a zero-valued slot" fallback
// once a group is declared, so every declared index emits a series whether a
// reader wrote it or not.
//
// That promise is enforced per METRIC, not per group: the snapshot builder
// applies `ag.member_set()` to every metric routed to the group. So a group
// shared by several metrics declares the UNION of their members, and any
// metric missing a member of that union publishes a fabricated zero there.
//
// Entity space alone is too coarse a partition for that reason. The four
// package-scope RAPL domains share an entity space (the package ordinal) but
// not a population: a server exposing only `energy-pkg` and `energy-ram` would,
// under one shared package-energy group, still emit `cpu_igpu_energy` and
// `cpu_cores_energy` at every package ordinal. The same holds per C-state
// level -- a part exposing c1/c3/c6/c7 would emit c2/c8/c9/c10 for every core.
// Those are exactly the fabricated zeros the member set exists to remove, and
// `docs/metrics.md` promises "the rest are absent rather than zero".
//
// So each metric gets its own group, and `refresh` brackets each group over
// only its own readers. That also makes the windows carry real information:
// one group per domain is one genuinely distinct observation window, where
// five groups over one flat sweep were five byte-identical spans -- principle
// 18's "no new information, pure schema bloat". A domain whose PMU is absent
// contributes no readers, so its group registers an empty member set and its
// metrics emit nothing, which is the honest answer.
//
// `core_cstate_residency` (the summed total) is the one metric written by
// readers from every core level, so it owns a group whose member set is the
// union of the core-scope levels -- which is correct precisely because every
// core reader writes it.

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
    "RAPL `energy-pkg`: one member per package, indexed by cpumask ordinal."
);

power_acq!(
    CPU_POWER_CORES_ENERGY_ACQ,
    CPU_POWER_CORES_ENERGY_ACQ_REG,
    "cpu_power_cores_energy",
    "RAPL `energy-cores`: one member per package, indexed by cpumask ordinal. \
     Separate from `energy-pkg` because a part may expose one and not the other."
);

power_acq!(
    CPU_POWER_IGPU_ENERGY_ACQ,
    CPU_POWER_IGPU_ENERGY_ACQ_REG,
    "cpu_power_igpu_energy",
    "RAPL `energy-gpu`: one member per package. Absent on server parts with no \
     integrated graphics, which then emit nothing rather than zero."
);

power_acq!(
    CPU_POWER_DRAM_ENERGY_ACQ,
    CPU_POWER_DRAM_ENERGY_ACQ_REG,
    "cpu_power_dram_energy",
    "RAPL `energy-ram`: one member per package, indexed by cpumask ordinal."
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
    CPU_POWER_CORE_C1_ACQ,
    CPU_POWER_CORE_C1_ACQ_REG,
    "cpu_power_core_c1",
    "Per-core C1 idle residency: one member per physical core. Declared \
     empty on a part whose `cstate_core` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_CORE_C2_ACQ,
    CPU_POWER_CORE_C2_ACQ_REG,
    "cpu_power_core_c2",
    "Per-core C2 idle residency: one member per physical core. Declared \
     empty on a part whose `cstate_core` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_CORE_C3_ACQ,
    CPU_POWER_CORE_C3_ACQ_REG,
    "cpu_power_core_c3",
    "Per-core C3 idle residency: one member per physical core. Declared \
     empty on a part whose `cstate_core` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_CORE_C6_ACQ,
    CPU_POWER_CORE_C6_ACQ_REG,
    "cpu_power_core_c6",
    "Per-core C6 idle residency: one member per physical core. Declared \
     empty on a part whose `cstate_core` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_CORE_C7_ACQ,
    CPU_POWER_CORE_C7_ACQ_REG,
    "cpu_power_core_c7",
    "Per-core C7 idle residency: one member per physical core. Declared \
     empty on a part whose `cstate_core` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_CORE_C8_ACQ,
    CPU_POWER_CORE_C8_ACQ_REG,
    "cpu_power_core_c8",
    "Per-core C8 idle residency: one member per physical core. Declared \
     empty on a part whose `cstate_core` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_CORE_C9_ACQ,
    CPU_POWER_CORE_C9_ACQ_REG,
    "cpu_power_core_c9",
    "Per-core C9 idle residency: one member per physical core. Declared \
     empty on a part whose `cstate_core` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_CORE_C10_ACQ,
    CPU_POWER_CORE_C10_ACQ_REG,
    "cpu_power_core_c10",
    "Per-core C10 idle residency: one member per physical core. Declared \
     empty on a part whose `cstate_core` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_PACKAGE_C1_ACQ,
    CPU_POWER_PACKAGE_C1_ACQ_REG,
    "cpu_power_package_c1",
    "Per-package C1 idle residency: one member per package. Declared \
     empty on a part whose `cstate_pkg` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_PACKAGE_C2_ACQ,
    CPU_POWER_PACKAGE_C2_ACQ_REG,
    "cpu_power_package_c2",
    "Per-package C2 idle residency: one member per package. Declared \
     empty on a part whose `cstate_pkg` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_PACKAGE_C3_ACQ,
    CPU_POWER_PACKAGE_C3_ACQ_REG,
    "cpu_power_package_c3",
    "Per-package C3 idle residency: one member per package. Declared \
     empty on a part whose `cstate_pkg` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_PACKAGE_C6_ACQ,
    CPU_POWER_PACKAGE_C6_ACQ_REG,
    "cpu_power_package_c6",
    "Per-package C6 idle residency: one member per package. Declared \
     empty on a part whose `cstate_pkg` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_PACKAGE_C7_ACQ,
    CPU_POWER_PACKAGE_C7_ACQ_REG,
    "cpu_power_package_c7",
    "Per-package C7 idle residency: one member per package. Declared \
     empty on a part whose `cstate_pkg` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_PACKAGE_C8_ACQ,
    CPU_POWER_PACKAGE_C8_ACQ_REG,
    "cpu_power_package_c8",
    "Per-package C8 idle residency: one member per package. Declared \
     empty on a part whose `cstate_pkg` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_PACKAGE_C9_ACQ,
    CPU_POWER_PACKAGE_C9_ACQ_REG,
    "cpu_power_package_c9",
    "Per-package C9 idle residency: one member per package. Declared \
     empty on a part whose `cstate_pkg` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_PACKAGE_C10_ACQ,
    CPU_POWER_PACKAGE_C10_ACQ_REG,
    "cpu_power_package_c10",
    "Per-package C10 idle residency: one member per package. Declared \
     empty on a part whose `cstate_pkg` PMU does not expose this level."
);

power_acq!(
    CPU_POWER_CORE_CSTATE_ACQ,
    CPU_POWER_CORE_CSTATE_ACQ_REG,
    "cpu_power_core_cstate",
    "The summed `core_cstate_residency`: one member per physical core. Every \
     core-scope level reader writes this metric, so the union of those levels \
     is exactly its population."
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
    metadata = { unit = "microjoules", acq_group = "cpu_power_cores_energy" }
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
    metadata = { unit = "microjoules", acq_group = "cpu_power_igpu_energy" }
)]
pub static CPU_IGPU_ENERGY: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "cpu_dram_energy",
    description = "The cumulative energy consumed by the DRAM attached to this package.",
    metadata = { unit = "microjoules", acq_group = "cpu_power_dram_energy" }
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
    metadata = { unit = "cycles", acq_group = "cpu_power_core_c1" }
)]
pub static CORE_C1: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c2_residency",
    description = "The cumulative TSC cycles a physical core spent in the C2 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_c2" }
)]
pub static CORE_C2: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c3_residency",
    description = "The cumulative TSC cycles a physical core spent in the C3 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_c3" }
)]
pub static CORE_C3: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c6_residency",
    description = "The cumulative TSC cycles a physical core spent in the C6 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_c6" }
)]
pub static CORE_C6: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c7_residency",
    description = "The cumulative TSC cycles a physical core spent in the C7 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_c7" }
)]
pub static CORE_C7: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c8_residency",
    description = "The cumulative TSC cycles a physical core spent in the C8 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_c8" }
)]
pub static CORE_C8: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c9_residency",
    description = "The cumulative TSC cycles a physical core spent in the C9 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_c9" }
)]
pub static CORE_C9: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "core_c10_residency",
    description = "The cumulative TSC cycles a physical core spent in the C10 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_core_c10" }
)]
pub static CORE_C10: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "package_c1_residency",
    description = "The cumulative TSC cycles a package spent in the C1 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_c1" }
)]
pub static PACKAGE_C1: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c2_residency",
    description = "The cumulative TSC cycles a package spent in the C2 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_c2" }
)]
pub static PACKAGE_C2: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c3_residency",
    description = "The cumulative TSC cycles a package spent in the C3 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_c3" }
)]
pub static PACKAGE_C3: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c6_residency",
    description = "The cumulative TSC cycles a package spent in the C6 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_c6" }
)]
pub static PACKAGE_C6: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c7_residency",
    description = "The cumulative TSC cycles a package spent in the C7 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_c7" }
)]
pub static PACKAGE_C7: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c8_residency",
    description = "The cumulative TSC cycles a package spent in the C8 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_c8" }
)]
pub static PACKAGE_C8: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c9_residency",
    description = "The cumulative TSC cycles a package spent in the C9 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_c9" }
)]
pub static PACKAGE_C9: CounterGroup = CounterGroup::new(MAX_PACKAGES);

#[metric(
    name = "package_c10_residency",
    description = "The cumulative TSC cycles a package spent in the C10 idle state. Divide by cpu_tsc for a residency fraction.",
    metadata = { unit = "cycles", acq_group = "cpu_power_package_c10" }
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
