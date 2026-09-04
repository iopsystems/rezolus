//! Collects CPU energy and idle-state residency from the perf PMUs the kernel
//! exposes for them.
//!
//! Energy, not power: energy is a monotonic counter that aggregates correctly
//! over any window, and power is its derivative, recoverable at query time. No
//! power gauge is exported -- see the note on the energy metrics in `stats.rs`.
//!
//! This sampler produces, for each domain the hardware implements:
//! * `cpu_<domain>_energy` - cumulative energy in microjoules. Power is its
//!   derivative, computed at query time as
//!   `irate(cpu_<domain>_energy) / 1e6`, which is correct over any window.
//! * `core_cN_residency` / `package_cN_residency` - cumulative TSC cycles in
//!   each idle C-state, plus `core_cstate_residency` summing every core level
//!
//! # Discovery
//!
//! Everything is discovered from `/sys/bus/event_source/devices`. Four PMUs are
//! consulted, and each is optional:
//!
//! * `power` - package-scope RAPL energy (`energy-pkg`, `energy-cores`,
//!   `energy-gpu`, `energy-ram`, `energy-psys`)
//! * `power_core` - per-core RAPL energy (`energy-core`), AMD only in practice
//! * `cstate_core` - per-core idle residency (`cN-residency`)
//! * `cstate_pkg` - package idle residency (`cN-residency`)
//!
//! There is deliberately no CPU vendor detection. Which PMUs exist, which
//! events they expose, and which CPUs may read them are all properties the
//! kernel already publishes, and they vary by part as much as by vendor: two
//! Intel CPUs here expose different C-states and different RAPL domains as each
//! other. Asking the kernel is both simpler and more accurate than branching on
//! vendor and then guessing at the model.
//!
//! # cpumask
//!
//! Each PMU publishes a `cpumask` naming the CPUs permitted to read it, and
//! opening a counter on any other CPU fails with `EINVAL`. The mask also
//! encodes the counter's scope, which is what makes vendor detection
//! unnecessary:
//!
//! * A package-scope PMU lists one CPU per package, so one counter covers the
//!   whole package.
//! * A core-scope PMU lists one CPU per *physical* core, with SMT siblings
//!   excluded. Each counter then covers exactly one core, and the kernel has
//!   already done the sibling deduplication for us.
//!
//! Core-scope metrics are indexed by the CPU id the counter was created on,
//! which is a physical core reader. Package-scope metrics are indexed by the
//! package's ordinal position in the cpumask instead: the cpumask's CPU ids are
//! arbitrary (`0,64` on a two-socket host), whereas the ordinal is dense and
//! matches the `MAX_PACKAGES` sizing of those metric groups.
//!
//! # Why perf only
//!
//! RAPL can also be read from `/dev/cpu/N/msr`, but the perf PMUs cover every
//! domain the hardware actually implements, need only `CAP_PERFMON` rather than
//! `CAP_SYS_RAWIO`, let the kernel handle the 32-bit counter wrap, and report
//! energy pre-scaled. Note that the `msr` PMU is not an alternative route to
//! the RAPL registers: despite taking a 64-bit `config`, `msr_event_init()`
//! rejects anything outside a fixed allowlist of eight architectural counters,
//! none of which are energy registers.

const NAME: &str = "cpu_power";

use crate::agent::*;

use crate::agent::timing::AcquisitionGroup;
use metriken::CounterGroup;
use perf_event::events::Dynamic;
use tokio::sync::Mutex;

mod stats;

use stats::*;

/// A counter we sample every refresh.
struct Reader {
    counter: perf_event::Counter,
    /// The index this counter's samples are written at within its metric
    /// group. Derived from the CPU the counter was created on via `Index`.
    index: usize,
    kind: Kind,
    /// The acquisition group this counter belongs to -- equivalently, the
    /// metric it feeds, since each metric owns a group. Held per reader so
    /// `init` can build the member set from what actually opened and partition
    /// the readers into `ReaderGroup`s, which is what lets `refresh` bracket
    /// each group over only its own readers.
    acq: &'static AcquisitionGroup,
    /// Previous cumulative value, for differencing. `None` until the first
    /// sample establishes a baseline.
    previous: Option<u64>,
}

/// What a counter measures and how its raw value should be interpreted.
enum Kind {
    /// A RAPL energy counter. `scale` converts the raw count to joules.
    Energy {
        scale: f64,
        energy: &'static CounterGroup,
        /// Accumulated microjoules, kept in floating point so that the
        /// sub-microjoule hardware quantum is not lost to repeated truncation.
        energy_uj: f64,
    },
    /// A C-state residency counter, in TSC cycles. Core-scope counters also
    /// accumulate into a per-core total across all levels.
    Cstate {
        residency: &'static CounterGroup,
        total: Option<&'static CounterGroup>,
    },
}

/// One acquisition group and the readers that populate it.
///
/// The partition is built once at `init` so that `refresh` brackets each group
/// over only its own readers: a group's window then describes the read of that
/// group's members and nothing else.
struct ReaderGroup {
    acq: &'static AcquisitionGroup,
    readers: Vec<Reader>,
}

/// Every acquisition group this sampler declares, so `init` can build each
/// one's member set and `refresh` can bracket each independently.
///
/// One group per metric, because the exposition layer applies a group's member
/// set to every metric routed to it: a group shared by several metrics
/// declares the union of their populations, and any metric short of that union
/// publishes fabricated zeros at the surplus indices. See the header comment in
/// `stats.rs`.
const ACQ_GROUPS: &[&AcquisitionGroup] = &[
    &CPU_POWER_PACKAGE_ENERGY_ACQ,
    &CPU_POWER_CORES_ENERGY_ACQ,
    &CPU_POWER_IGPU_ENERGY_ACQ,
    &CPU_POWER_DRAM_ENERGY_ACQ,
    &CPU_POWER_CORE_ENERGY_ACQ,
    &CPU_POWER_PLATFORM_ENERGY_ACQ,
    &CPU_POWER_CORE_C1_ACQ,
    &CPU_POWER_CORE_C2_ACQ,
    &CPU_POWER_CORE_C3_ACQ,
    &CPU_POWER_CORE_C6_ACQ,
    &CPU_POWER_CORE_C7_ACQ,
    &CPU_POWER_CORE_C8_ACQ,
    &CPU_POWER_CORE_C9_ACQ,
    &CPU_POWER_CORE_C10_ACQ,
    &CPU_POWER_PACKAGE_C1_ACQ,
    &CPU_POWER_PACKAGE_C2_ACQ,
    &CPU_POWER_PACKAGE_C3_ACQ,
    &CPU_POWER_PACKAGE_C6_ACQ,
    &CPU_POWER_PACKAGE_C7_ACQ,
    &CPU_POWER_PACKAGE_C8_ACQ,
    &CPU_POWER_PACKAGE_C9_ACQ,
    &CPU_POWER_PACKAGE_C10_ACQ,
];

/// The core-scope C-state level groups, in the order their levels are declared.
///
/// `core_cstate_residency` sums every one of these levels, so its own group's
/// population is the union of theirs -- and unlike the per-level metrics that
/// union is correct, because every core-scope reader writes the total.
const CORE_CSTATE_LEVEL_ACQ_GROUPS: &[&AcquisitionGroup] = &[
    &CPU_POWER_CORE_C1_ACQ,
    &CPU_POWER_CORE_C2_ACQ,
    &CPU_POWER_CORE_C3_ACQ,
    &CPU_POWER_CORE_C6_ACQ,
    &CPU_POWER_CORE_C7_ACQ,
    &CPU_POWER_CORE_C8_ACQ,
    &CPU_POWER_CORE_C9_ACQ,
    &CPU_POWER_CORE_C10_ACQ,
];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let mut readers = discover();

    if readers.is_empty() {
        // None of the PMUs are present. This is the normal case under
        // virtualization, so it is not an error.
        debug!("{NAME}: no energy or c-state PMUs available, disabling sampler");
        return Ok(None);
    }

    // Membership comes from registration, not values (principle 18). Declare
    // the exact indices rather than a `0..n` prefix: these are genuinely
    // sparse, and an unwritten CounterGroup slot reads as 0, so a declared
    // index nothing writes would publish zero joules -- a wrong value on a
    // metric whose whole purpose is measuring draw, not a missing one.
    //
    // Two ways the indices go sparse, one structural and one exceptional:
    // `Index::Cpu` uses the CPU id, and a hybrid part's core-scope cpumask
    // skips SMT siblings (`0,2,4,6,8,10,12-15` on a 16-thread host here), so
    // a prefix would span six ids nothing ever writes; and any single
    // `open_counter` failure leaves a hole while later indices still succeed.
    //
    // The set is built per group from the readers that actually opened, so a
    // domain whose PMU is absent declares no members at all -- and since every
    // group backs exactly one metric, that set is that metric's own population
    // rather than a union over its neighbours'.
    //
    // Readers are partitioned into their groups here, once, so `refresh` can
    // bracket each group over only its own readers. These are independent perf
    // fds with no per-device handle to re-open, so reading them group-major
    // costs exactly what one flat sweep did -- there is no re-visit to trade
    // against, which is why this does not need the device-visit archetype's
    // "stale value under a fresh window" dispensation.
    let mut groups: Vec<ReaderGroup> = Vec::new();

    for acq in ACQ_GROUPS {
        let (mine, rest): (Vec<Reader>, Vec<Reader>) =
            readers.into_iter().partition(|r| std::ptr::eq(r.acq, *acq));

        readers = rest;

        // Declared even when empty: a group with no readers is a metric whose
        // PMU this part does not expose, and an empty member set is how it
        // says "no members" rather than falling back to backing capacity.
        acq.set_member_set(&mine.iter().map(|r| r.index).collect::<Vec<_>>());

        if !mine.is_empty() {
            groups.push(ReaderGroup { acq, readers: mine });
        }
    }

    debug_assert!(
        readers.is_empty(),
        "every reader's acquisition group must appear in ACQ_GROUPS"
    );

    // `core_cstate_residency` is written by every core-scope level reader, so
    // its population is the union of the level groups' -- the one place a
    // union is the right answer, because here every member really is written.
    // Its window is not stamped separately: the readers that feed it are
    // bracketed by their own level groups, and `set_member_set` sorts and
    // de-duplicates, so the repeated core ids collapse.
    let total_members: Vec<usize> = CORE_CSTATE_LEVEL_ACQ_GROUPS
        .iter()
        .filter_map(|acq| acq.member_set())
        .flatten()
        .copied()
        .collect();

    CPU_POWER_CORE_CSTATE_ACQ.set_member_set(&total_members);

    Ok(Some(Box::new(Power {
        inner: PowerInner { groups }.into(),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Power {
    inner: Mutex<PowerInner>,
}

#[async_trait]
impl Sampler for Power {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        inner.refresh();
    }
}

struct PowerInner {
    groups: Vec<ReaderGroup>,
}

impl PowerInner {
    // Brackets each group separately (principle 18): one group per metric, so
    // a window describes the read of exactly that metric's members and the
    // groups carry genuinely distinct spans rather than N copies of the whole
    // sweep. Every bracket spans its own member writes and stamps last via
    // `finish()`.
    //
    // None discard: a single counter's failed `read()` is individually,
    // normally fallible rather than a bulk sweep failure -- same ruling as
    // `cpu_l3`/`cpu_dtlb`, which applies here for the same reason it does
    // there now that each bracket really is one group's own read section.
    fn refresh(&mut self) {
        for group in self.groups.iter_mut() {
            let guard = group.acq.acquire();

            // Whether any member value was actually written under this
            // bracket. On the very first refresh every reader takes the
            // baseline `continue` below, so nothing is written and there is no
            // interval to describe; stamping anyway would publish a fresh
            // window over counters still reading 0 -- the same fabricated-zero
            // failure the sparse member set exists to prevent, and the honest
            // signal is "no new data this tick" instead.
            let mut wrote = false;

            for reader in group.readers.iter_mut() {
                let Ok(raw) = reader.counter.read() else {
                    continue;
                };

                // The first sample only establishes a baseline. A counter that
                // goes backwards was reset underneath us; re-baseline and skip
                // the interval rather than emitting a bogus spike.
                let Some(previous) = reader.previous.replace(raw) else {
                    continue;
                };

                let Some(delta) = raw.checked_sub(previous) else {
                    continue;
                };

                let index = reader.index;

                match &mut reader.kind {
                    Kind::Energy {
                        scale,
                        energy,
                        energy_uj,
                    } => {
                        let delta_uj = delta as f64 * *scale * 1_000_000.0;

                        // Accumulate in floating point, emit the integer part,
                        // and carry the remainder into the next interval so
                        // that truncation does not drift.
                        *energy_uj += delta_uj;
                        let whole = energy_uj.trunc();
                        *energy_uj -= whole;

                        let _ = energy.add(index, whole as u64);
                    }
                    Kind::Cstate { residency, total } => {
                        let _ = residency.add(index, delta);

                        if let Some(total) = total {
                            let _ = total.add(index, delta);
                        }
                    }
                }

                wrote = true;
            }

            if wrote {
                guard.finish();
            } else {
                guard.discard();
            }
        }
    }
}

/// How a counter's metric index is derived from the CPU it was opened on.
#[derive(Clone, Copy)]
enum Index {
    /// Index by the CPU id itself. Used for core-scope counters, where the CPU
    /// id names the physical core being measured.
    Cpu,
    /// Index by the CPU's ordinal position within the PMU's cpumask. Used for
    /// package-scope counters, whose metric groups are sized to MAX_PACKAGES
    /// and whose cpumask CPU ids are sparse (`0,64` on a two-socket host).
    Ordinal,
    /// Always index zero. Used for host-scope counters, of which there is one.
    ///
    /// A host-scope domain still publishes a package-scope cpumask -- `energy-psys`
    /// names one CPU per package like its `power`-PMU siblings -- but it measures
    /// the whole platform, so every one of those CPUs reads the SAME quantity.
    /// Opening all of them would `add()` that quantity into index 0 once per
    /// package: a two-socket host would report double the platform draw, not two
    /// halves of it. Only the first cpumask entry is opened; see `open_energy`.
    Zero,
}

/// Build the reader set by walking the PMUs the kernel exposes.
fn discover() -> Vec<Reader> {
    let mut readers = Vec::new();

    // Each domain carries its own acquisition group, because each backs its
    // own metric: a part exposing `energy-pkg` but not `energy-gpu` must
    // declare no members for the latter rather than inheriting the former's.
    // `Index` still encodes the entity space the members are numbered in:
    // package ordinal, physical core, or the single host.
    for (pmu, event, index, energy, acq) in [
        (
            "power",
            "energy-pkg",
            Index::Ordinal,
            &CPU_PACKAGE_ENERGY,
            &CPU_POWER_PACKAGE_ENERGY_ACQ,
        ),
        (
            "power",
            "energy-cores",
            Index::Ordinal,
            &CPU_CORES_ENERGY,
            &CPU_POWER_CORES_ENERGY_ACQ,
        ),
        (
            "power",
            "energy-gpu",
            Index::Ordinal,
            &CPU_IGPU_ENERGY,
            &CPU_POWER_IGPU_ENERGY_ACQ,
        ),
        (
            "power",
            "energy-ram",
            Index::Ordinal,
            &CPU_DRAM_ENERGY,
            &CPU_POWER_DRAM_ENERGY_ACQ,
        ),
        (
            "power",
            "energy-psys",
            Index::Zero,
            &CPU_PLATFORM_ENERGY,
            &CPU_POWER_PLATFORM_ENERGY_ACQ,
        ),
        (
            "power_core",
            "energy-core",
            Index::Cpu,
            &CPU_CORE_ENERGY,
            &CPU_POWER_CORE_ENERGY_ACQ,
        ),
    ] {
        open_energy(pmu, event, index, energy, acq, &mut readers);
    }

    // Core C-states are core-scope and also feed the per-core total; package
    // C-states are package-scope and have no equivalent aggregate.
    open_cstate(
        "cstate_core",
        Index::Cpu,
        core_cstate_metric,
        Some((&CORE_CSTATE, &CPU_POWER_CORE_CSTATE_ACQ)),
        &mut readers,
    );
    open_cstate(
        "cstate_pkg",
        Index::Ordinal,
        package_cstate_metric,
        None,
        &mut readers,
    );

    readers
}

impl Index {
    /// Map a cpumask entry to the index its metric group is written at.
    ///
    /// Writes past a metric group's capacity are dropped by metriken rather
    /// than panicking, so an index that overflows its group would silently
    /// lose that series. Warn once here, at discovery, and return `None` so no
    /// reader is built for it: keeping one would cost a `read()` syscall per
    /// refresh, forever, for a value that is discarded every time. Declining
    /// the counter makes the ceiling visible as absent data instead.
    fn resolve(self, pmu: &str, event: &str, cpu: usize, ordinal: usize) -> Option<usize> {
        let (index, limit) = match self {
            Index::Cpu => (cpu, MAX_CPUS),
            Index::Ordinal => (ordinal, MAX_PACKAGES),
            Index::Zero => (0, 1),
        };

        if index >= limit {
            warn!(
                "{NAME}: {pmu}/{event} on cpu{cpu} maps to index {index}, \
                 beyond the metric group's {limit} entries; this series will not be reported"
            );

            return None;
        }

        Some(index)
    }
}

/// Open one energy counter per CPU in the PMU's cpumask.
fn open_energy(
    pmu: &str,
    event: &str,
    index: Index,
    energy: &'static CounterGroup,
    acq: &'static AcquisitionGroup,
    readers: &mut Vec<Reader>,
) {
    let Some(scale) = event_scale(pmu, event) else {
        return;
    };

    let cpus = cpumask(pmu);

    // A host-scope domain is one quantity however many CPUs may read it, so
    // take a single reader for it. Every other scope opens its whole cpumask.
    let cpus = match index {
        Index::Zero => &cpus[..cpus.len().min(1)],
        Index::Cpu | Index::Ordinal => &cpus[..],
    };

    for (ordinal, &cpu) in cpus.iter().enumerate() {
        let Some(resolved) = index.resolve(pmu, event, cpu, ordinal) else {
            continue;
        };

        let Some(counter) = open_counter(pmu, event, cpu) else {
            continue;
        };

        debug!("{NAME}: {pmu}/{event} on cpu{cpu}");

        readers.push(Reader {
            counter,
            index: resolved,
            kind: Kind::Energy {
                scale,
                energy,
                energy_uj: 0.0,
            },
            acq,
            previous: None,
        });
    }
}

/// Open every `cN-residency` event the PMU exposes, on every CPU in its
/// cpumask. Which levels exist varies by part, so they are enumerated from the
/// PMU's events directory rather than assumed.
fn open_cstate(
    pmu: &str,
    index: Index,
    level_metric: fn(u8) -> Option<(&'static CounterGroup, &'static AcquisitionGroup)>,
    total: Option<(&'static CounterGroup, &'static AcquisitionGroup)>,
    readers: &mut Vec<Reader>,
) {
    let cpus = cpumask(pmu);

    if cpus.is_empty() {
        return;
    }

    for (event, level) in cstate_events(pmu) {
        let Some((residency, acq)) = level_metric(level) else {
            debug!("{NAME}: {pmu}/{event} has no metric for level c{level}, skipping");
            continue;
        };

        for (ordinal, &cpu) in cpus.iter().enumerate() {
            let Some(resolved) = index.resolve(pmu, &event, cpu, ordinal) else {
                continue;
            };

            let Some(counter) = open_counter(pmu, &event, cpu) else {
                continue;
            };

            debug!("{NAME}: {pmu}/{event} on cpu{cpu}");

            readers.push(Reader {
                counter,
                index: resolved,
                kind: Kind::Cstate {
                    residency,
                    total: total.map(|(metric, _)| metric),
                },
                acq,
                previous: None,
            });
        }
    }
}

/// Open a single counter for `pmu/event/` on `cpu`.
fn open_counter(pmu: &str, event: &str, cpu: usize) -> Option<perf_event::Counter> {
    let mut builder = Dynamic::builder(pmu).ok()?;

    let event_builder = match builder.event(event) {
        Ok(b) => b,
        Err(e) => {
            debug!("{NAME}: {pmu}/{event} not exposed: {e}");
            return None;
        }
    };

    let dynamic = match event_builder.build() {
        Ok(d) => d,
        Err(e) => {
            debug!("{NAME}: {pmu}/{event} could not be built: {e}");
            return None;
        }
    };

    // These are free-running hardware counters that are not attributable to a
    // privilege level. The kernel rejects any attempt to filter one, and the
    // builder excludes kernel and hypervisor by default, so both must be
    // cleared explicitly or perf_event_open returns EINVAL.
    let mut counter = match perf_event::Builder::new(dynamic)
        .one_cpu(cpu)
        .any_pid()
        .exclude_kernel(false)
        .exclude_hv(false)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            debug!("{NAME}: {pmu}/{event} unavailable on cpu{cpu}: {e}");
            return None;
        }
    };

    if let Err(e) = counter.enable() {
        debug!("{NAME}: {pmu}/{event} could not be enabled on cpu{cpu}: {e}");
        return None;
    }

    Some(counter)
}

/// Read an event's `.scale`, which converts its raw count into joules.
///
/// Absence means the event is not exposed by this PMU, which is the normal way
/// an unsupported domain presents itself.
fn event_scale(pmu: &str, event: &str) -> Option<f64> {
    let mut builder = Dynamic::builder(pmu).ok()?;

    builder.event(event).ok()?.scale().ok().flatten()
}

/// Enumerate the `cN-residency` events a c-state PMU exposes, as
/// `(event_name, level)` pairs sorted by level.
fn cstate_events(pmu: &str) -> Vec<(String, u8)> {
    let dir = format!("/sys/bus/event_source/devices/{pmu}/events");

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut events: Vec<(String, u8)> = entries
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;

            // Skip the `.scale`/`.unit`/`.snapshot` sidecar files.
            let level = name.strip_prefix('c')?.strip_suffix("-residency")?;

            Some((name.clone(), level.parse().ok()?))
        })
        .collect();

    events.sort_by_key(|(_, level)| *level);
    events
}

/// The metric and acquisition group for one core C-state level.
///
/// Paired here so a level cannot be routed to a group that is not its own:
/// each level owns a group, since a part exposing c1/c3/c6/c7 must declare no
/// members for c2/c8/c9/c10 rather than inheriting the levels it does expose.
fn core_cstate_metric(level: u8) -> Option<(&'static CounterGroup, &'static AcquisitionGroup)> {
    Some(match level {
        1 => (&CORE_C1, &CPU_POWER_CORE_C1_ACQ),
        2 => (&CORE_C2, &CPU_POWER_CORE_C2_ACQ),
        3 => (&CORE_C3, &CPU_POWER_CORE_C3_ACQ),
        6 => (&CORE_C6, &CPU_POWER_CORE_C6_ACQ),
        7 => (&CORE_C7, &CPU_POWER_CORE_C7_ACQ),
        8 => (&CORE_C8, &CPU_POWER_CORE_C8_ACQ),
        9 => (&CORE_C9, &CPU_POWER_CORE_C9_ACQ),
        10 => (&CORE_C10, &CPU_POWER_CORE_C10_ACQ),
        _ => return None,
    })
}

/// The metric and acquisition group for one package C-state level.
fn package_cstate_metric(level: u8) -> Option<(&'static CounterGroup, &'static AcquisitionGroup)> {
    Some(match level {
        1 => (&PACKAGE_C1, &CPU_POWER_PACKAGE_C1_ACQ),
        2 => (&PACKAGE_C2, &CPU_POWER_PACKAGE_C2_ACQ),
        3 => (&PACKAGE_C3, &CPU_POWER_PACKAGE_C3_ACQ),
        6 => (&PACKAGE_C6, &CPU_POWER_PACKAGE_C6_ACQ),
        7 => (&PACKAGE_C7, &CPU_POWER_PACKAGE_C7_ACQ),
        8 => (&PACKAGE_C8, &CPU_POWER_PACKAGE_C8_ACQ),
        9 => (&PACKAGE_C9, &CPU_POWER_PACKAGE_C9_ACQ),
        10 => (&PACKAGE_C10, &CPU_POWER_PACKAGE_C10_ACQ),
        _ => return None,
    })
}

/// The CPUs a PMU permits its counters to be opened on.
///
/// This is both a permission list and a statement of scope: a package-scope PMU
/// names one CPU per package, a core-scope PMU one CPU per physical core.
fn cpumask(pmu: &str) -> Vec<usize> {
    let path = format!("/sys/bus/event_source/devices/{pmu}/cpumask");

    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    // Shared with the PMU-reservation mask parser rather than hand-rolled
    // here. That one rejects a malformed list outright instead of returning a
    // partial set; sysfs never emits one, so the strictness costs nothing, and
    // an unreadable or unparseable cpumask lands in the same place an absent
    // PMU does -- no counters for it.
    crate::agent::pmu::parse_cpu_list(&raw).unwrap_or_default()
}
