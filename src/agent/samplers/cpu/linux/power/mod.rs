//! Collects CPU energy, power, and idle-state residency from the perf PMUs the
//! kernel exposes for them.
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

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let readers = discover();

    if readers.is_empty() {
        // None of the PMUs are present. This is the normal case under
        // virtualization, so it is not an error.
        debug!("{NAME}: no energy or c-state PMUs available, disabling sampler");
        return Ok(None);
    }

    Ok(Some(Box::new(Power {
        inner: PowerInner { readers }.into(),
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
    readers: Vec<Reader>,
}

impl PowerInner {
    fn refresh(&mut self) {
        for reader in self.readers.iter_mut() {
            let Ok(raw) = reader.counter.read() else {
                continue;
            };

            // The first sample only establishes a baseline. A counter that goes
            // backwards was reset underneath us; re-baseline and skip the
            // interval rather than emitting a bogus spike.
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

                    // Accumulate in floating point, emit the integer part, and
                    // carry the remainder into the next interval so that
                    // truncation does not drift.
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
    Zero,
}

/// Build the reader set by walking the PMUs the kernel exposes.
fn discover() -> Vec<Reader> {
    let mut readers = Vec::new();

    for (pmu, event, index, energy) in [
        ("power", "energy-pkg", Index::Ordinal, &CPU_PACKAGE_ENERGY),
        ("power", "energy-cores", Index::Ordinal, &CPU_CORES_ENERGY),
        ("power", "energy-gpu", Index::Ordinal, &CPU_IGPU_ENERGY),
        ("power", "energy-ram", Index::Ordinal, &CPU_DRAM_ENERGY),
        ("power", "energy-psys", Index::Zero, &CPU_PLATFORM_ENERGY),
        ("power_core", "energy-core", Index::Cpu, &CPU_CORE_ENERGY),
    ] {
        open_energy(pmu, event, index, energy, &mut readers);
    }

    // Core C-states are core-scope and also feed the per-core total; package
    // C-states are package-scope and have no equivalent aggregate.
    open_cstate(
        "cstate_core",
        Index::Cpu,
        core_cstate_metric,
        Some(&CORE_CSTATE),
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
    /// lose that series. Warn once here, at discovery, instead of leaving the
    /// loss invisible.
    fn resolve(self, pmu: &str, event: &str, cpu: usize, ordinal: usize) -> usize {
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
        }

        index
    }
}

/// Open one energy counter per CPU in the PMU's cpumask.
fn open_energy(
    pmu: &str,
    event: &str,
    index: Index,
    energy: &'static CounterGroup,
    readers: &mut Vec<Reader>,
) {
    let Some(scale) = event_scale(pmu, event) else {
        return;
    };

    for (ordinal, cpu) in cpumask(pmu).into_iter().enumerate() {
        let Some(counter) = open_counter(pmu, event, cpu) else {
            continue;
        };

        debug!("{NAME}: {pmu}/{event} on cpu{cpu}");

        readers.push(Reader {
            counter,
            index: index.resolve(pmu, event, cpu, ordinal),
            kind: Kind::Energy {
                scale,
                energy,
                energy_uj: 0.0,
            },
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
    level_metric: fn(u8) -> Option<&'static CounterGroup>,
    total: Option<&'static CounterGroup>,
    readers: &mut Vec<Reader>,
) {
    let cpus = cpumask(pmu);

    if cpus.is_empty() {
        return;
    }

    for (event, level) in cstate_events(pmu) {
        let Some(residency) = level_metric(level) else {
            debug!("{NAME}: {pmu}/{event} has no metric for level c{level}, skipping");
            continue;
        };

        for (ordinal, &cpu) in cpus.iter().enumerate() {
            let Some(counter) = open_counter(pmu, &event, cpu) else {
                continue;
            };

            debug!("{NAME}: {pmu}/{event} on cpu{cpu}");

            readers.push(Reader {
                counter,
                index: index.resolve(pmu, &event, cpu, ordinal),
                kind: Kind::Cstate { residency, total },
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

fn core_cstate_metric(level: u8) -> Option<&'static CounterGroup> {
    Some(match level {
        1 => &CORE_C1,
        2 => &CORE_C2,
        3 => &CORE_C3,
        6 => &CORE_C6,
        7 => &CORE_C7,
        8 => &CORE_C8,
        9 => &CORE_C9,
        10 => &CORE_C10,
        _ => return None,
    })
}

fn package_cstate_metric(level: u8) -> Option<&'static CounterGroup> {
    Some(match level {
        1 => &PACKAGE_C1,
        2 => &PACKAGE_C2,
        3 => &PACKAGE_C3,
        6 => &PACKAGE_C6,
        7 => &PACKAGE_C7,
        8 => &PACKAGE_C8,
        9 => &PACKAGE_C9,
        10 => &PACKAGE_C10,
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

    parse_cpu_list(&raw)
}

/// Parse a sysfs CPU list such as `0`, `0-11`, or `0,2,4,12-15`.
fn parse_cpu_list(raw: &str) -> Vec<usize> {
    let mut cpus = Vec::new();

    for range in raw.trim().split(',') {
        if range.is_empty() {
            continue;
        }

        let mut parts = range.split('-');

        let Some(start) = parts.next().and_then(|v| v.trim().parse::<usize>().ok()) else {
            continue;
        };

        match parts.next() {
            Some(end) => {
                if let Ok(end) = end.trim().parse::<usize>() {
                    for cpu in start..=end {
                        cpus.push(cpu);
                    }
                }
            }
            None => cpus.push(start),
        }
    }

    cpus.sort_unstable();
    cpus.dedup();
    cpus
}
